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
    test_hooks::run_before_schema_transaction(&path);
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
