//! The window between the open funnel's preflight and its `BEGIN IMMEDIATE`.
//!
//! `test_hooks::arm_before_schema_transaction` runs a closure after every
//! preflight gate, the backup and the connection PRAGMAs, immediately before
//! the transaction opens. The closure opens its own connection and commits,
//! standing in for another process. Every gate must then be decided on the
//! in-transaction state, never on the preflight values.

use super::*;
use rusqlite::Connection;
use std::cell::RefCell;
use std::rc::Rc;

use crate::db::{DbOpenContext, StoreProfile};

type SchemaRows = Vec<(String, String, Option<String>)>;
type StateRows = Vec<(String, String, String)>;

fn snapshot(conn: &Connection) -> (u32, SchemaRows, StateRows) {
    let version = crate::db::migrations::read_schema_version(conn).expect("user_version");
    let mut stmt = conn
        .prepare("SELECT type, name, sql FROM main.sqlite_schema ORDER BY type, name")
        .expect("prepare schema");
    let schema = stmt
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
        .expect("query schema")
        .map(|r| r.expect("schema row"))
        .collect();
    let state = if schema_has_table(conn, "hard_state") {
        let mut stmt = conn
            .prepare("SELECT namespace, key, value_json FROM hard_state ORDER BY namespace, key")
            .expect("prepare hard_state");
        stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
            .expect("query hard_state")
            .map(|r| r.expect("hard_state row"))
            .collect()
    } else {
        Vec::new()
    };
    (version, schema, state)
}

fn schema_has_table(conn: &Connection, table: &str) -> bool {
    conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM main.sqlite_schema WHERE type = 'table' AND name = ?1)",
        [table],
        |r| r.get(0),
    )
    .expect("table probe")
}

fn backups_in(dir: &Path) -> usize {
    std::fs::read_dir(dir)
        .expect("read dir")
        .filter_map(|e| e.ok())
        .filter(|e| e.file_name().to_string_lossy().contains("migration-bak"))
        .count()
}

/// Another process provisions a current TachiFull store at `path`.
fn other_process_creates_current_store(path: &Path) {
    let mut other = Connection::open(path).expect("other process connection");
    crate::db::init_schema_with_label_mut(
        &mut other,
        "global",
        path,
        &DbOpenContext::create_fresh(),
    )
    .expect("other process provisions the store");
}

fn seed_current_store(path: &Path) {
    crate::db::enable_simple_auto_extension().unwrap();
    other_process_creates_current_store(path);
}

/// Arm the window hook to run `commit` and then record the store's state as
/// that "other process" left it.
fn arm_window(
    commit: impl FnOnce(&Path) + 'static,
) -> Rc<RefCell<Option<(u32, SchemaRows, StateRows)>>> {
    let after_window = Rc::new(RefCell::new(None));
    let slot = Rc::clone(&after_window);
    test_hooks::arm_before_schema_transaction(move |path| {
        commit(path);
        let observer = Connection::open(path).expect("observer connection");
        *slot.borrow_mut() = Some(snapshot(&observer));
    });
    after_window
}

fn open_existing_allow() -> DbOpenContext {
    DbOpenContext::open_existing_allow("test:open-race")
}

/// (a) A newer kernel commits a v40 migration in the window. The v39 opener
/// passed its preflight at v39; it must refuse with the version gate's error
/// on the in-transaction state, and must not stamp the store back to 39 or run
/// any DDL.
#[test]
fn newer_schema_committed_in_window_is_refused_not_stamped_back() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let path = tmp.path().join("race-v40.db");
    seed_current_store(&path);

    let after_window = arm_window(|path| {
        let other = Connection::open(path).expect("newer kernel connection");
        other
            .execute_batch(&format!(
                "BEGIN IMMEDIATE;
                 CREATE TABLE v40_newer_kernel_table(x);
                 PRAGMA user_version = {};
                 COMMIT;",
                crate::db::migrations::EXPECTED_SCHEMA_VERSION + 1
            ))
            .expect("newer kernel commits v40");
    });

    let mut conn = Connection::open(&path).expect("opener connection");
    let err = crate::db::init_schema_with_label_mut(
        &mut conn,
        "global",
        &path,
        &DbOpenContext::open_existing_deny(),
    )
    .expect_err("a store that became newer than this kernel must be refused");
    assert!(
        err.to_string().contains("newer than supported"),
        "expected the #984 version-gate refusal, got {err:?}"
    );
    drop(conn);

    let expected = after_window.borrow().clone().expect("window ran");
    let actual = snapshot(&Connection::open(&path).expect("inspect"));
    assert_eq!(
        actual.0,
        crate::db::migrations::EXPECTED_SCHEMA_VERSION + 1,
        "user_version must still be the newer kernel's"
    );
    assert_eq!(actual, expected, "no DDL, stamp or state write may remain");
}

/// (b) `CreateFresh` passes its preflight on an empty file; another process
/// provisions the store in the window. Creating on an existing store must be
/// refused with `DbCreateTargetExists`, leaving the other process's store
/// exactly as it committed it.
#[test]
fn create_fresh_when_store_is_created_in_window_is_refused() {
    crate::db::enable_simple_auto_extension().unwrap();
    let tmp = tempfile::tempdir().expect("tempdir");
    let path = tmp.path().join("race-create.db");

    let after_window = arm_window(other_process_creates_current_store);

    let mut conn = Connection::open(&path).expect("opener connection");
    let err = crate::db::init_schema_with_label_mut(
        &mut conn,
        "global",
        &path,
        &DbOpenContext::create_fresh(),
    )
    .expect_err("create-on-existing must be refused on the in-transaction state");
    assert!(
        matches!(
            err,
            MemoryError::DbCreateTargetExists { stored, .. }
                if stored == crate::db::migrations::EXPECTED_SCHEMA_VERSION
        ),
        "expected DbCreateTargetExists, got {err:?}"
    );
    drop(conn);

    let expected = after_window.borrow().clone().expect("window ran");
    let actual = snapshot(&Connection::open(&path).expect("inspect"));
    assert_eq!(actual, expected, "no DDL, stamp or state write may remain");
}

/// (c) Liveness: an authorized opener passes its preflight at an older
/// version (and backs that state up); another process finishes the same
/// migration first. The in-transaction state is current, so the open is
/// admitted as an ordinary current open. It must neither refuse because the
/// state changed nor act on the stale "migration pending" preflight.
#[test]
fn older_to_current_in_window_is_admitted_on_the_in_tx_state() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let path = tmp.path().join("race-38-39.db");
    seed_current_store(&path);
    Connection::open(&path)
        .expect("roll back")
        .execute_batch(&format!(
            "PRAGMA user_version = {}",
            crate::db::migrations::EXPECTED_SCHEMA_VERSION - 1
        ))
        .expect("stamp the store one version older");

    let after_window = arm_window(|path| {
        let mut other = Connection::open(path).expect("other migrator");
        crate::db::init_schema_with_label_mut(&mut other, "global", path, &open_existing_allow())
            .expect("the other process finishes the migration first");
    });

    let mut conn = Connection::open(&path).expect("opener connection");
    crate::db::init_schema_with_label_mut(&mut conn, "global", &path, &open_existing_allow())
        .expect("a store that became current in the window is admitted");
    drop(conn);

    let window = after_window.borrow().clone().expect("window ran");
    assert_eq!(window.0, crate::db::migrations::EXPECTED_SCHEMA_VERSION);
    let actual = snapshot(&Connection::open(&path).expect("inspect"));
    assert_eq!(actual.0, crate::db::migrations::EXPECTED_SCHEMA_VERSION);
    assert_eq!(
        actual.1, window.1,
        "a current open adds no schema to what the migrator committed"
    );
}

/// (c, literal Deny form) An older store under `Deny` is refused by the
/// preflight gate itself, before the window, so "another process finishes
/// 38→39 while a Deny opener waits" cannot reach the transaction. Pinned so
/// that stays true: the window hook must never run.
#[test]
fn older_store_under_deny_refuses_in_preflight_before_the_window() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let path = tmp.path().join("race-deny.db");
    seed_current_store(&path);
    Connection::open(&path)
        .expect("roll back")
        .execute_batch(&format!(
            "PRAGMA user_version = {}",
            crate::db::migrations::EXPECTED_SCHEMA_VERSION - 1
        ))
        .expect("stamp the store one version older");

    let window_ran = Rc::new(RefCell::new(false));
    let flag = Rc::clone(&window_ran);
    test_hooks::arm_before_schema_transaction(move |_| *flag.borrow_mut() = true);

    let mut conn = Connection::open(&path).expect("opener connection");
    let err = crate::db::init_schema_with_label_mut(
        &mut conn,
        "global",
        &path,
        &DbOpenContext::open_existing_deny(),
    )
    .expect_err("Deny must refuse an older store");
    assert!(
        matches!(err, MemoryError::SchemaMigrationOptInRequired { .. }),
        "got {err:?}"
    );
    assert!(
        !*window_ran.borrow(),
        "the preflight must refuse before the window"
    );
    // Disarm for any later test on this thread.
    test_hooks::disarm_window_hooks();
}

/// (d) The `fresh` discriminator must be the in-transaction one. A portable
/// opener passes its preflight on an empty file (fresh, so "build portable");
/// in the window another process leaves a stamped, profile-less full store.
/// On the in-transaction state that is an existing unstamped store, which a
/// portable requirement must refuse. Acting on the stale `fresh = true` would
/// instead stamp `portable_kernel` into a full store.
#[test]
fn stale_fresh_is_not_used_when_an_unprofiled_store_appears_in_window() {
    crate::db::enable_simple_auto_extension().unwrap();
    let tmp = tempfile::tempdir().expect("tempdir");
    let path = tmp.path().join("race-fresh.db");

    let after_window = arm_window(|path| {
        other_process_creates_current_store(path);
        Connection::open(path)
            .expect("legacy writer")
            .execute(
                "DELETE FROM hard_state WHERE namespace = 'store_identity'",
                [],
            )
            .expect("leave the store without identity stamps");
    });

    let mut conn = Connection::open(&path).expect("opener connection");
    let err = crate::db::init_schema_with_label_mut(
        &mut conn,
        "global",
        &path,
        &DbOpenContext::open_existing_deny().with_profile(StoreProfile::PortableKernel),
    )
    .expect_err("a portable opener must not adopt an existing unstamped store");
    assert!(
        matches!(err, MemoryError::StoreProfileUnstamped { .. }),
        "expected StoreProfileUnstamped, got {err:?}"
    );
    drop(conn);

    let expected = after_window.borrow().clone().expect("window ran");
    let actual = snapshot(&Connection::open(&path).expect("inspect"));
    assert_eq!(
        actual, expected,
        "no profile stamp or other write may remain"
    );
}

/// (e) #1180 coverage. An authorized opener passes its preflight on an empty
/// file (nothing to back up); in the window another process leaves a store
/// stamped one version older. The in-transaction state is a migration the
/// preflight never backed up, so the open refuses with
/// `SchemaChangedDuringOpen` rather than migrate without a backup.
#[test]
fn migration_state_appearing_in_window_is_refused_without_backup_coverage() {
    crate::db::enable_simple_auto_extension().unwrap();
    let tmp = tempfile::tempdir().expect("tempdir");
    let path = tmp.path().join("race-backup.db");

    let after_window = arm_window(|path| {
        other_process_creates_current_store(path);
        Connection::open(path)
            .expect("older writer")
            .execute_batch(&format!(
                "PRAGMA user_version = {}",
                crate::db::migrations::EXPECTED_SCHEMA_VERSION - 1
            ))
            .expect("stamp one version older");
    });

    let mut conn = Connection::open(&path).expect("opener connection");
    let err =
        crate::db::init_schema_with_label_mut(&mut conn, "global", &path, &open_existing_allow())
            .expect_err("a migration whose state was never backed up must be refused");
    assert!(
        matches!(
            err,
            MemoryError::SchemaChangedDuringOpen { preflight: 0, current, .. }
                if current == crate::db::migrations::EXPECTED_SCHEMA_VERSION - 1
        ),
        "expected SchemaChangedDuringOpen, got {err:?}"
    );
    drop(conn);

    assert_eq!(backups_in(tmp.path()), 0, "fixture: nothing was backed up");
    let expected = after_window.borrow().clone().expect("window ran");
    let actual = snapshot(&Connection::open(&path).expect("inspect"));
    assert_eq!(
        actual, expected,
        "no migration, stamp or state write may remain"
    );
}

/// (f) #1119 authority. A `Deny` opener passes its preflight on an empty file
/// (a build needs no authority); in the window another process leaves a store
/// stamped one version older. On the in-transaction state that is an
/// unauthorized migration, which must be refused with
/// `SchemaMigrationOptInRequired` instead of migrated in place.
#[test]
fn unauthorized_migration_state_appearing_in_window_is_refused_under_deny() {
    crate::db::enable_simple_auto_extension().unwrap();
    let tmp = tempfile::tempdir().expect("tempdir");
    let path = tmp.path().join("race-deny-window.db");

    let after_window = arm_window(|path| {
        other_process_creates_current_store(path);
        Connection::open(path)
            .expect("older writer")
            .execute_batch(&format!(
                "PRAGMA user_version = {}",
                crate::db::migrations::EXPECTED_SCHEMA_VERSION - 1
            ))
            .expect("stamp one version older");
    });

    let mut conn = Connection::open(&path).expect("opener connection");
    let err = crate::db::init_schema_with_label_mut(
        &mut conn,
        "global",
        &path,
        &DbOpenContext::open_existing_deny(),
    )
    .expect_err("an unauthorized opener must not migrate a store that became older-stamped");
    assert!(
        matches!(err, MemoryError::SchemaMigrationOptInRequired { .. }),
        "expected SchemaMigrationOptInRequired, got {err:?}"
    );
    drop(conn);

    let expected = after_window.borrow().clone().expect("window ran");
    let actual = snapshot(&Connection::open(&path).expect("inspect"));
    assert_eq!(
        actual, expected,
        "no migration, stamp or state write may remain"
    );
}

/// Round 3, FIX A. Another process drops a canonical search-generation
/// trigger in the window. `init_schema_inner` would recreate it
/// (`CREATE TRIGGER IF NOT EXISTS`) before the end-of-transaction validation,
/// silently repairing damaged input. The in-transaction input-inventory check
/// must refuse first, with exactly the error a sequential open of the same
/// damaged store gives, and leave no DDL or stamp.
#[test]
fn trigger_dropped_in_window_is_refused_like_a_sequential_open() {
    const TRIGGER: &str = "memory_edge_search_generation_after_update";
    let tmp = tempfile::tempdir().expect("tempdir");
    let path = tmp.path().join("race-trigger.db");
    seed_current_store(&path);

    // Reference: the same damage, present before a plain sequential open.
    let sequential_path = tmp.path().join("sequential-trigger.db");
    seed_current_store(&sequential_path);
    Connection::open(&sequential_path)
        .expect("damage")
        .execute_batch(&format!("DROP TRIGGER {TRIGGER}"))
        .expect("damage the store before the sequential open");
    let sequential_err = crate::MemoryStore::open_with_label_and_context(
        sequential_path.to_str().expect("utf8"),
        "global",
        &DbOpenContext::open_existing_deny(),
    )
    .err()
    .expect("a sequential open must refuse a damaged current trigger inventory");

    let after_window = arm_window(|path| {
        Connection::open(path)
            .expect("damaging writer")
            .execute_batch(&format!("DROP TRIGGER {TRIGGER}"))
            .expect("damage the store inside the window");
    });
    // Through the real store funnel: its pre-init inventory check passes (the
    // trigger is still there), then the window drops it.
    let err = crate::MemoryStore::open_with_label_and_context(
        path.to_str().expect("utf8"),
        "global",
        &DbOpenContext::open_existing_deny(),
    )
    .err()
    .expect("a trigger dropped in the window must be refused, not repaired");

    assert_eq!(
        err.to_string(),
        sequential_err.to_string(),
        "the in-transaction refusal must be the sequential open's refusal"
    );
    let expected = after_window.borrow().clone().expect("window ran");
    let actual = snapshot(&Connection::open(&path).expect("inspect"));
    assert!(
        !actual.1.iter().any(|(_, name, _)| name == TRIGGER),
        "the dropped trigger must not have been recreated"
    );
    assert_eq!(actual, expected, "no DDL, stamp or state write may remain");
}

/// Round 3, FIX B (ABA). The store is current-shaped, its marker matches
/// `schema:39:<cookie>`, and an external tool has lowered `user_version` to 38.
/// The Allow preflight reads 38. Another process sets 39 before the backup
/// step, which therefore sees "no migration, marker matches" and skips. The
/// version goes back to 38 before `BEGIN IMMEDIATE`. The preflight and
/// in-transaction versions agree (38 == 38), but nothing backed up 38: the
/// coverage check must use the backup step's own decision and refuse.
#[test]
fn aba_version_around_the_backup_decision_is_refused_without_a_backup() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let path = tmp.path().join("race-aba.db");
    seed_current_store(&path);
    let older = crate::db::migrations::EXPECTED_SCHEMA_VERSION - 1;
    let set_version = |path: &Path, version: u32| {
        Connection::open(path)
            .expect("version tool")
            .execute_batch(&format!("PRAGMA user_version = {version}"))
            .expect("set user_version");
    };
    set_version(&path, older);
    assert!(
        migration_marker_path(&path).exists(),
        "fixture: seeding leaves the current-version marker"
    );

    test_hooks::arm_window_hook(test_hooks::Window::BeforeBackupDecision, move |path| {
        set_version(path, crate::db::migrations::EXPECTED_SCHEMA_VERSION)
    });
    let after_window = arm_window(move |path| set_version(path, older));

    let mut conn = Connection::open(&path).expect("opener connection");
    let err =
        crate::db::init_schema_with_label_mut(&mut conn, "global", &path, &open_existing_allow())
            .expect_err("a migration the backup step never covered must be refused");
    drop(conn);
    assert!(
        matches!(
            err,
            MemoryError::SchemaChangedDuringOpen { preflight, current, .. }
                if preflight == crate::db::migrations::EXPECTED_SCHEMA_VERSION && current == older
        ),
        "expected SchemaChangedDuringOpen {{ preflight: 39, current: 38 }}, got {err:?}"
    );
    assert_eq!(
        backups_in(tmp.path()),
        0,
        "the backup step skipped; nothing was backed up"
    );
    let expected = after_window.borrow().clone().expect("window ran");
    let actual = snapshot(&Connection::open(&path).expect("inspect"));
    assert_eq!(actual.0, older, "the store must not have been migrated");
    assert_eq!(
        actual, expected,
        "no migration, stamp or state write may remain"
    );
}

/// Table 2, row 3 (`pre = tx`, a written backup of ANOTHER version). The Allow
/// preflight reads 37; another process moves the store to 38 before the
/// backup step, which therefore copies 38; the store is back at 37 before
/// `BEGIN IMMEDIATE`. The preflight and in-transaction versions agree
/// (37 == 37), but the only backup is of 38, so migrating 37 would run
/// without a backup of the state being migrated. The coverage check must
/// compare against the backup step's own decision and refuse.
#[test]
fn written_backup_of_another_version_does_not_cover_the_in_tx_migration() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let path = tmp.path().join("race-row3.db");
    seed_current_store(&path);
    let expected_version = crate::db::migrations::EXPECTED_SCHEMA_VERSION;
    let pre = expected_version - 2;
    let backed_up = expected_version - 1;
    let set_version = |path: &Path, version: u32| {
        Connection::open(path)
            .expect("version tool")
            .execute_batch(&format!("PRAGMA user_version = {version}"))
            .expect("set user_version");
    };
    set_version(&path, pre);

    test_hooks::arm_window_hook(test_hooks::Window::BeforeBackupDecision, move |path| {
        set_version(path, backed_up)
    });
    let after_window = arm_window(move |path| set_version(path, pre));

    let mut conn = Connection::open(&path).expect("opener connection");
    let err =
        crate::db::init_schema_with_label_mut(&mut conn, "global", &path, &open_existing_allow())
            .expect_err("a backup of another version must not cover the in-tx migration");
    drop(conn);
    assert!(
        matches!(
            err,
            MemoryError::SchemaChangedDuringOpen { preflight, current, .. }
                if preflight == backed_up && current == pre
        ),
        "expected SchemaChangedDuringOpen {{ preflight: {backed_up}, current: {pre} }}, got {err:?}"
    );

    let backups: Vec<_> = std::fs::read_dir(tmp.path())
        .expect("read dir")
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.to_string_lossy().contains("migration-bak"))
        .collect();
    assert_eq!(
        backups.len(),
        1,
        "exactly the backup step's one backup: {backups:?}"
    );
    assert_eq!(
        crate::db::migrations::read_schema_version(
            &Connection::open(&backups[0]).expect("open backup")
        )
        .expect("backup user_version"),
        backed_up,
        "the backup is of the version the backup step saw, not of the in-tx state"
    );

    let expected = after_window.borrow().clone().expect("window ran");
    let actual = snapshot(&Connection::open(&path).expect("inspect"));
    assert_eq!(actual.0, pre, "the store must not have been migrated");
    assert_eq!(
        actual, expected,
        "no migration, stamp or state write may remain"
    );
}

/// tachi#2002 review finding 2: the store funnel's post-COMMIT trigger
/// inventory re-check. Another process drops a canonical trigger immediately
/// after the schema transaction commits. The admission was valid, but the
/// connection's state no longer is, so the open must refuse to return the
/// handle, with the same error a sequential open of the damaged store gives.
#[test]
fn trigger_dropped_after_commit_is_refused_before_a_handle_is_returned() {
    const TRIGGER: &str = "memory_edge_search_generation_after_update";
    let tmp = tempfile::tempdir().expect("tempdir");
    let path = tmp.path().join("race-after-commit.db");
    seed_current_store(&path);

    let sequential_path = tmp.path().join("sequential-after-commit.db");
    seed_current_store(&sequential_path);
    Connection::open(&sequential_path)
        .expect("damage")
        .execute_batch(&format!("DROP TRIGGER {TRIGGER}"))
        .expect("damage the store before the sequential open");
    let sequential_err = crate::MemoryStore::open_with_label_and_context(
        sequential_path.to_str().expect("utf8"),
        "global",
        &DbOpenContext::open_existing_deny(),
    )
    .err()
    .expect("a sequential open must refuse a damaged current trigger inventory");

    let fired = Rc::new(RefCell::new(false));
    {
        let fired = Rc::clone(&fired);
        test_hooks::arm_window_hook(test_hooks::Window::AfterSchemaCommit, move |path| {
            Connection::open(path)
                .expect("damaging writer")
                .execute_batch(&format!("DROP TRIGGER {TRIGGER}"))
                .expect("damage the store right after the schema COMMIT");
            *fired.borrow_mut() = true;
        });
    }
    let result = crate::MemoryStore::open_with_label_and_context(
        path.to_str().expect("utf8"),
        "global",
        &DbOpenContext::open_existing_deny(),
    );
    test_hooks::disarm_window_hooks();
    assert!(
        *fired.borrow(),
        "the post-COMMIT window must have been reached"
    );
    let err = result
        .err()
        .expect("a trigger dropped after COMMIT must refuse the handle, not return it");
    assert_eq!(
        err.to_string(),
        sequential_err.to_string(),
        "the post-COMMIT refusal must be the sequential open's refusal"
    );
}

const COLD_START_CHILD_DB: &str = "TACHI_TEST_COLD_START_CHILD_DB";
const COLD_START_CHILD_GO: &str = "TACHI_TEST_COLD_START_CHILD_GO";
const COLD_START_CHILD_ID: &str = "TACHI_TEST_COLD_START_CHILD_ID";
const COLD_START_CHILD_EXTRA_HANDLES: &str = "TACHI_TEST_COLD_START_CHILD_EXTRA_HANDLES";

fn cold_start_entry(id: String) -> crate::MemoryEntry {
    crate::MemoryEntry {
        id,
        path: "/test/cold-start".to_string(),
        summary: "cold start".to_string(),
        text: "concurrent cold start write".to_string(),
        importance: 0.5,
        timestamp: chrono::Utc::now().to_rfc3339(),
        valid_from: String::new(),
        valid_until: None,
        category: "fact".to_string(),
        topic: "cold-start".to_string(),
        keywords: vec![],
        persons: vec![],
        entities: vec![],
        location: String::new(),
        source: "test".to_string(),
        scope: "general".to_string(),
        archived: false,
        access_count: 0,
        scored_count: 0,
        last_access: None,
        last_use_at: None,
        revision: 1,
        metadata: serde_json::json!({}),
        vector: None,
        retention_policy: None,
        domain: None,
        recall_count: 0,
        query_diversity: 0,
        tier: "raw".to_string(),
    }
}

/// Child half of the concurrent cold-start test: one process performing a
/// cold start (open + schema init/migration), two writes and a second open.
#[test]
fn concurrent_cold_start_child() {
    let Ok(path) = std::env::var(COLD_START_CHILD_DB) else {
        return;
    };
    let go = PathBuf::from(std::env::var(COLD_START_CHILD_GO).expect("go file"));
    let id = std::env::var(COLD_START_CHILD_ID).expect("child id");
    while !go.exists() {
        std::thread::sleep(std::time::Duration::from_millis(1));
    }
    let ctx = open_existing_allow();
    let mut store = crate::MemoryStore::open_with_label_and_context(&path, "global", &ctx)
        .unwrap_or_else(|e| panic!("cold start open failed: {e}"));
    // #2137: optionally churn extra in-process handles (read-only and second
    // read-write opens, each resolving the generic path) while this
    // connection keeps writing, as a long-lived server process does.
    let extra_handles = std::env::var_os(COLD_START_CHILD_EXTRA_HANDLES).is_some();
    let writes = if extra_handles { 8 } else { 2 };
    let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let churn = extra_handles.then(|| {
        let (path, stop) = (path.clone(), stop.clone());
        let ctx = open_existing_allow();
        std::thread::spawn(move || {
            while !stop.load(std::sync::atomic::Ordering::Relaxed) {
                // A read-only open can be refused before the schema is current.
                drop(crate::MemoryStore::open_read_only(&path));
                crate::MemoryStore::open_with_label_and_context(&path, "global", &ctx)
                    .unwrap_or_else(|e| panic!("extra read-write open failed: {e}"));
            }
        })
    });
    for n in 0..writes {
        store
            .upsert(&cold_start_entry(format!("cold-{id}-{n}")))
            .unwrap_or_else(|e| panic!("cold start write failed: {e}"));
    }
    stop.store(true, std::sync::atomic::Ordering::Relaxed);
    if let Some(churn) = churn {
        churn.join().expect("extra handle thread");
    }
    let second = crate::MemoryStore::open_with_label_and_context(&path, "global", &ctx)
        .unwrap_or_else(|e| panic!("second open failed: {e}"));
    drop(second);
    drop(store);
}

/// #2134: N processes cold-start against one global DB at once, first on a
/// missing file and then on a behind-schema (v22) store that every child is
/// authorized to migrate. The DB must stay intact, fully migrated, and hold
/// every child's writes.
#[cfg(unix)]
#[test]
fn concurrent_cold_starts_keep_global_db_intact() {
    use std::process::{Command, Stdio};
    use std::time::{Duration, Instant};

    const CHILDREN: usize = 10;
    let cases = [(false, false), (true, false), (false, true), (true, true)];
    for (seed_older_schema, extra_handles) in cases {
        let writes = if extra_handles { 8 } else { 2 };
        let tmp = tempfile::tempdir().expect("tempdir");
        let path = tmp.path().join("tachi-memory.db");
        if seed_older_schema {
            super::migration_backup_tests::seed_v22_fixture(&path);
        }
        let go = tmp.path().join("go");
        let children: Vec<_> = (0..CHILDREN)
            .map(|id| {
                Command::new(std::env::current_exe().expect("test executable"))
                    .args([
                        "--exact",
                        "db::schema::open_race_tests::concurrent_cold_start_child",
                        "--nocapture",
                    ])
                    .env(COLD_START_CHILD_DB, &path)
                    .env(COLD_START_CHILD_GO, &go)
                    .env(COLD_START_CHILD_ID, id.to_string())
                    .envs(extra_handles.then_some((COLD_START_CHILD_EXTRA_HANDLES, "1")))
                    .stdout(Stdio::null())
                    .stderr(Stdio::piped())
                    .spawn()
                    .expect("spawn cold start child")
            })
            .collect();
        // Let every child finish loading before releasing them together.
        std::thread::sleep(Duration::from_millis(300));
        std::fs::write(&go, b"go").expect("release children");
        let deadline = Instant::now() + Duration::from_secs(120);
        let mut failures = Vec::new();
        for mut child in children {
            while child.try_wait().expect("poll child").is_none() {
                assert!(Instant::now() < deadline, "cold start child hung");
                std::thread::sleep(Duration::from_millis(10));
            }
            let output = child.wait_with_output().expect("child output");
            if !output.status.success() {
                failures.push(String::from_utf8_lossy(&output.stderr).into_owned());
            }
        }
        assert!(
            failures.is_empty(),
            "older_schema={seed_older_schema} extra_handles={extra_handles}: {} of {CHILDREN} cold starts failed:\n{}",
            failures.len(),
            failures.join("\n---\n")
        );

        crate::db::enable_simple_auto_extension().expect("register simple tokenizer");
        crate::db::register_sqlite_vec();
        let conn = Connection::open(&path).expect("inspect db");
        let integrity: String = conn
            .query_row("PRAGMA integrity_check", [], |r| r.get(0))
            .expect("integrity_check");
        assert_eq!(integrity, "ok", "older_schema={seed_older_schema}");
        assert_eq!(
            crate::db::migrations::read_schema_version(&conn).expect("user_version"),
            crate::db::migrations::EXPECTED_SCHEMA_VERSION,
            "older_schema={seed_older_schema}"
        );
        let rows: i64 = conn
            .query_row(
                "SELECT count(*) FROM memories WHERE id LIKE 'cold-%'",
                [],
                |r| r.get(0),
            )
            .expect("count child writes");
        assert_eq!(
            rows,
            (CHILDREN * writes) as i64,
            "older_schema={seed_older_schema}"
        );
    }
}

const LOCK_PROBE_CHILD_DB: &str = "TACHI_TEST_LOCK_PROBE_CHILD_DB";

/// Child half of the lock-retention test: report whether another process
/// still holds a reader-excluding lock on the database.
#[test]
fn generic_open_lock_probe_child() {
    let Ok(path) = std::env::var(LOCK_PROBE_CHILD_DB) else {
        return;
    };
    let conn = Connection::open(&path).expect("probe connection");
    conn.busy_timeout(std::time::Duration::ZERO).unwrap();
    match conn.query_row("SELECT count(*) FROM t", [], |r| r.get::<_, i64>(0)) {
        Err(rusqlite::Error::SqliteFailure(e, _))
            if e.code == rusqlite::ErrorCode::DatabaseBusy =>
        {
            println!("LOCK-PROBE:BUSY")
        }
        Ok(_) => println!("LOCK-PROBE:READ"),
        Err(other) => println!("LOCK-PROBE:ERROR {other}"),
    }
}

/// #2137: resolving a generic open path must not drop the POSIX locks other
/// SQLite connections in this process hold on the same file (closing any fd
/// on a file releases all of the process's fcntl locks on it).
#[cfg(unix)]
#[test]
fn generic_open_path_resolution_keeps_other_connections_locks() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let path = tmp.path().join("locked.db");
    let holder = Connection::open(&path).expect("holder connection");
    holder
        .execute_batch("PRAGMA journal_mode=DELETE; CREATE TABLE t(x); INSERT INTO t VALUES (1);")
        .expect("seed rollback-journal db");
    holder
        .execute_batch("BEGIN EXCLUSIVE; INSERT INTO t VALUES (2);")
        .expect("hold exclusive lock");
    let child_sees = || {
        let output = std::process::Command::new(std::env::current_exe().expect("test executable"))
            .args([
                "--exact",
                "db::schema::open_race_tests::generic_open_lock_probe_child",
                "--nocapture",
            ])
            .env(LOCK_PROBE_CHILD_DB, &path)
            .output()
            .expect("run lock probe child");
        let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
        stdout
            .lines()
            .find_map(|l| l.strip_prefix("LOCK-PROBE:").map(str::to_string))
            .unwrap_or_else(|| panic!("no probe result: {stdout}"))
    };
    assert_eq!(child_sees(), "BUSY", "fixture must hold the lock");
    for _ in 0..3 {
        crate::private_partition::resolve_generic_open_path(path.to_str().unwrap())
            .expect("resolve a valid SQLite path");
    }
    assert_eq!(
        child_sees(),
        "BUSY",
        "resolving the path in-process dropped the holder's lock"
    );
    holder.execute_batch("COMMIT").expect("commit");
}
