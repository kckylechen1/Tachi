//! D7 T7: literal old/B outcomes and phase/side-effect receipts.
//! Shape corruption fixtures are deliberately synthetic; historical lineage is
//! tested separately by the classification fixtures.

use super::portable_version_tests::{backups, context, fixture, sentinels, stamp_and_mark};
use super::{inventory::query_rows, pre_b_39, test_hooks};
use crate::db::{DbOpenContext, StoreProfile};
use crate::{MemoryError, MemoryStore};
use rusqlite::Connection;
use std::cell::Cell;
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::rc::Rc;

#[derive(Clone, Copy, Debug, PartialEq)]
enum Variant {
    Valid,
    Role,
    Profile,
    Sentinel,
    Requirement,
    Shape,
    ShapeRole,
    IntegrityRole,
    Outbox,
}
const VARIANTS: &[Variant] = &[
    Variant::Valid,
    Variant::Role,
    Variant::Profile,
    Variant::Sentinel,
    Variant::Requirement,
    Variant::Shape,
    Variant::ShapeRole,
    Variant::IntegrityRole,
    Variant::Outbox,
];

fn code(error: &MemoryError) -> &'static str {
    match error {
        MemoryError::SchemaMigrationOptInRequired { .. } => "opt_in",
        MemoryError::StoreProfileMismatch { .. } | MemoryError::StoreProfileNotExact { .. } => {
            "requirement"
        }
        MemoryError::CurrentSchemaIncomplete { .. } => "shape",
        MemoryError::InvalidArg(message) if message.contains("newer than") => "newer",
        MemoryError::InvalidArg(message) if message.contains("sentinel") => "sentinel",
        MemoryError::InvalidArg(message)
            if message.contains("store identity stamp") || message.contains("store profile") =>
        {
            "identity"
        }
        MemoryError::InvalidArg(message) if message.contains("outbox") => "outbox",
        MemoryError::InvalidArg(message) if message.contains("delivery") => "delivery",
        error => panic!("unclassified outcome: {error:?}"),
    }
}

#[derive(Debug, PartialEq)]
struct Outcome {
    code: &'static str,
    transaction: bool,
    backups: usize,
    stamp: u32,
}

fn expected(
    profile: StoreProfile,
    stamp: u32,
    allow: bool,
    variant: Variant,
    old: bool,
) -> Outcome {
    use Variant::*;
    let pending = stamp != 0
        && stamp
            < if old || profile == StoreProfile::TachiFull || variant == Profile {
                39
            } else {
                36
            };
    let error = if stamp > 39 {
        "newer"
    } else if pending && !allow {
        "opt_in"
    } else if (stamp == 39 || (!old && profile == StoreProfile::PortableKernel && stamp >= 36))
        && variant == Sentinel
    {
        "sentinel"
    } else if (stamp == 39 || (!old && profile == StoreProfile::PortableKernel && stamp >= 36))
        && variant == IntegrityRole
    {
        "delivery"
    } else if (stamp == 39 || (!old && profile == StoreProfile::PortableKernel && stamp >= 36))
        && variant == Outbox
    {
        "outbox"
    } else if matches!(variant, Role | Profile | ShapeRole | IntegrityRole) {
        "identity"
    } else if variant == Requirement && profile == StoreProfile::PortableKernel {
        "requirement"
    } else if variant == Outbox {
        "outbox"
    } else if !old && stamp != 0 && profile == StoreProfile::PortableKernel && variant == Shape {
        "shape"
    } else {
        "ok"
    };
    let transaction = error == "ok"
        || error == "shape"
        || (error == "outbox"
            && stamp
                < if old {
                    39
                } else if profile == StoreProfile::PortableKernel {
                    36
                } else {
                    39
                });
    Outcome {
        code: error,
        transaction,
        backups: usize::from(transaction && pending),
        stamp: if error == "ok" && (old || profile == StoreProfile::TachiFull || stamp < 36) {
            39
        } else {
            stamp
        },
    }
}

// Bootstrap once per profile, then give every scenario its own complete file copy.
// The matrix measures admission/migration of existing stores, not fresh bootstrap.
struct FixtureSnapshot(Vec<(OsString, Vec<u8>)>);

impl FixtureSnapshot {
    fn new(profile: StoreProfile) -> Self {
        let (_dir, path) = fixture(profile);
        Self::capture(&path)
    }

    fn capture(path: &Path) -> Self {
        // fixture() has dropped its only MemoryStore. Refuse a live WAL rather
        // than silently copying a main database that is missing committed pages.
        let mut wal = path.as_os_str().to_owned();
        wal.push("-wal");
        match std::fs::metadata(&wal) {
            Ok(metadata) => assert_eq!(metadata.len(), 0, "fixture WAL must be checkpointed"),
            Err(error) => assert_eq!(error.kind(), std::io::ErrorKind::NotFound),
        }
        Self(
            std::fs::read_dir(path.parent().unwrap())
                .unwrap()
                .map(|entry| {
                    let entry = entry.unwrap();
                    assert!(entry.file_type().unwrap().is_file());
                    (entry.file_name(), std::fs::read(entry.path()).unwrap())
                })
                .collect(),
        )
    }

    fn materialize(&self) -> (tempfile::TempDir, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        for (name, bytes) in &self.0 {
            std::fs::write(dir.path().join(name), bytes).unwrap();
        }
        let path = dir.path().join("tachi-memory.db");
        (dir, path)
    }
}

#[test]
fn outcome_fixture_snapshots_preserve_files_and_isolate_mutations() {
    for profile in [StoreProfile::PortableKernel, StoreProfile::TachiFull] {
        let (_source_dir, source) = fixture(profile);
        let snapshot = FixtureSnapshot::capture(&source);
        let (_first_dir, first) = snapshot.materialize();
        let (_second_dir, second) = snapshot.materialize();
        assert_ne!(first, second);
        for (name, bytes) in &snapshot.0 {
            assert_eq!(
                std::fs::read(first.parent().unwrap().join(name)).unwrap(),
                *bytes
            );
            assert_eq!(
                std::fs::read(second.parent().unwrap().join(name)).unwrap(),
                *bytes
            );
        }
        let conn = Connection::open(&first).unwrap();
        assert_eq!(
            conn.query_row::<String, _, _>("PRAGMA integrity_check", [], |row| row.get(0))
                .unwrap(),
            "ok"
        );
        conn.execute_batch("CREATE TABLE snapshot_mutation(value TEXT); INSERT INTO snapshot_mutation VALUES('private'); PRAGMA user_version=7;").unwrap();
        drop(conn);
        for untouched in [&source, &second] {
            for (name, bytes) in &snapshot.0 {
                assert_eq!(
                    std::fs::read(untouched.parent().unwrap().join(name)).unwrap(),
                    *bytes
                );
            }
            let conn = Connection::open(untouched).unwrap();
            assert!(query_rows(
                &conn,
                "SELECT name FROM sqlite_schema WHERE name='snapshot_mutation'"
            )
            .is_empty());
        }
        // A later scenario must still start from the pristine in-memory snapshot.
        let (_third_dir, third) = snapshot.materialize();
        for (name, bytes) in &snapshot.0 {
            assert_eq!(
                std::fs::read(third.parent().unwrap().join(name)).unwrap(),
                *bytes
            );
        }
    }
}

#[test]
#[should_panic(expected = "fixture WAL must be checkpointed")]
fn outcome_fixture_snapshot_refuses_uncheckpointed_wal() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("tachi-memory.db");
    let conn = Connection::open(&path).unwrap();
    conn.execute_batch("PRAGMA journal_mode=WAL; PRAGMA wal_autocheckpoint=0; CREATE TABLE uncheckpointed(value TEXT); INSERT INTO uncheckpointed VALUES('committed in WAL');").unwrap();
    FixtureSnapshot::capture(&path);
    drop(conn);
}

#[test]
fn outcome_table_pins_pre_b_and_b_errors_phases_and_persistent_effects() {
    let portable = FixtureSnapshot::new(StoreProfile::PortableKernel);
    let full = FixtureSnapshot::new(StoreProfile::TachiFull);
    for (profile, stamp) in [
        (StoreProfile::PortableKernel, 40),
        (StoreProfile::TachiFull, 38),
        (StoreProfile::PortableKernel, 0),
        (StoreProfile::PortableKernel, 30),
        (StoreProfile::PortableKernel, 36),
        (StoreProfile::PortableKernel, 39),
    ] {
        for allow in [false, true] {
            for &variant in VARIANTS {
                for old in [true, false] {
                    let (_dir, path) = match profile {
                        StoreProfile::PortableKernel => portable.materialize(),
                        StoreProfile::TachiFull => full.materialize(),
                    };
                    let conn = Connection::open(&path).unwrap();
                    crate::db::ensure_reserved_reference_write_guard(&conn).unwrap();
                    conn.execute_batch("INSERT INTO memories(id,text,path,source,timestamp,created_at,updated_at,valid_from) VALUES('d7-outcome-row','retained row','/fixture','wiki','2026-10-07T00:00:00Z','2026-10-07T00:00:00Z','2026-10-07T00:00:00Z','2026-10-07T00:00:00Z');").unwrap();
                    if matches!(
                        variant,
                        Variant::Role | Variant::ShapeRole | Variant::IntegrityRole
                    ) {
                        conn.execute_batch("INSERT OR REPLACE INTO hard_state(namespace,key,value_json) VALUES('store_identity','role','{\"value\":7}');").unwrap();
                    }
                    if variant == Variant::Profile {
                        conn.execute_batch("UPDATE hard_state SET value_json='{\"value\":7}' WHERE namespace='store_identity' AND key='profile';").unwrap();
                    }
                    if variant == Variant::Sentinel {
                        conn.execute_batch("DELETE FROM hard_state WHERE namespace='migrations' AND key='v3_handoff_path_standardize';").unwrap();
                    }
                    if matches!(variant, Variant::Shape | Variant::ShapeRole) {
                        conn.execute_batch("DROP INDEX idx_memories_idless_identity_active; CREATE INDEX idx_memories_idless_identity_active ON memories(idless_identity) WHERE idless_identity IS NOT NULL AND archived=0;").unwrap();
                    }
                    if variant == Variant::IntegrityRole {
                        conn.execute_batch("DROP INDEX idx_delivery_events_delivery;")
                            .unwrap();
                    }
                    if variant == Variant::Outbox {
                        conn.execute_batch("DROP INDEX idx_memory_outbox_events_state_created;")
                            .unwrap();
                    }
                    drop(conn);
                    stamp_and_mark(&path, stamp);
                    let conn = Connection::open(&path).unwrap();
                    let schema = query_rows(
                        &conn,
                        "SELECT type,name,tbl_name,sql FROM sqlite_schema ORDER BY type,name",
                    );
                    let before_sentinels = sentinels(&conn);
                    let rows = query_rows(&conn, "SELECT * FROM memories ORDER BY id");
                    let cookie = query_rows(&conn, "PRAGMA schema_version");
                    drop(conn);
                    let mut ctx = context(profile, allow);
                    if variant == Variant::Requirement {
                        ctx.required_profile = StoreProfile::TachiFull.into();
                    }
                    let entered = Rc::new(Cell::new(false));
                    let receipt = Rc::clone(&entered);
                    if !old {
                        test_hooks::arm_window_hook(
                            test_hooks::Window::BeforeSchemaTransaction,
                            move |_| receipt.set(true),
                        );
                    }
                    let open = || match MemoryStore::open_with_context(path.to_str().unwrap(), &ctx)
                    {
                        Ok(store) => {
                            drop(store);
                            "ok"
                        }
                        Err(error) => code(&error),
                    };
                    let result = if old {
                        pre_b_39::with_policy(39, open)
                    } else {
                        // Replay a pre-B binary: the shipped head carries v40,
                        // but this probe's outcome matrix is pinned at the
                        // 39-catalogue behaviour (#2161 pre-B fixtures).
                        crate::db::migrations::catalogue::test_support::with_prefix(39, open)
                    };
                    let transaction = if old {
                        pre_b_39::transaction_entered()
                    } else {
                        entered.get()
                    };
                    test_hooks::disarm_window_hooks();
                    let conn = Connection::open(&path).unwrap();
                    let actual = Outcome {
                        code: result,
                        transaction,
                        backups: backups(&path).len(),
                        stamp: crate::db::migrations::read_schema_version(&conn).unwrap(),
                    };
                    assert_eq!(
                        actual,
                        expected(profile, stamp, allow, variant, old),
                        "{profile:?}@{stamp} allow={allow} {variant:?} old={old}"
                    );
                    assert_eq!(
                        query_rows(&conn, "SELECT * FROM memories ORDER BY id"),
                        rows,
                        "retained populated rows"
                    );
                    if result != "ok" {
                        assert_eq!(
                            sentinels(&conn),
                            before_sentinels,
                            "refusal cannot change sentinels"
                        );
                        assert_eq!(query_rows(&conn,"SELECT type,name,tbl_name,sql FROM sqlite_schema ORDER BY type,name"),schema,"refusal must roll back schema");
                        assert_eq!(
                            query_rows(&conn, "PRAGMA schema_version"),
                            cookie,
                            "refusal must roll back DDL cookie"
                        );
                    } else if stamp >= 36 && !old && profile == StoreProfile::PortableKernel {
                        assert_eq!(
                            sentinels(&conn),
                            before_sentinels,
                            "band must not enter sentinel runner"
                        );
                    } else if variant == Variant::Sentinel {
                        assert_ne!(
                            sentinels(&conn),
                            before_sentinels,
                            "old/pending successful migration repairs missing v3"
                        );
                        assert_eq!(conn.query_row::<u32,_,_>("SELECT count(*) FROM hard_state WHERE namespace='migrations' AND key='v3_handoff_path_standardize'",[],|r|r.get(0)).unwrap(),1);
                    }
                }
            }
        }
    }
}

#[test]
fn converged_full_reopen_has_identical_raw_schema_and_cookie() {
    let (_dir, path) = fixture(StoreProfile::TachiFull);
    // Converge optional capability once, then sample equally fresh raw handles.
    crate::db::migrations::catalogue::test_support::with_prefix(39, || {
        drop(
            MemoryStore::open_with_context(
                path.to_str().unwrap(),
                &context(StoreProfile::TachiFull, false),
            )
            .unwrap(),
        );
    });
    stamp_and_mark(&path, 39);
    let original_backups = backups(&path);
    let before = Connection::open(&path).unwrap();
    let schema = query_rows(
        &before,
        "SELECT type,name,tbl_name,sql FROM sqlite_schema ORDER BY type,name",
    );
    let cookie = query_rows(&before, "PRAGMA schema_version");
    drop(before);
    // The second open replays a pre-B binary: without the pinned catalogue
    // the v40 Product migration would try to run against a 39-stamped store.
    crate::db::migrations::catalogue::test_support::with_prefix(39, || {
        drop(
            MemoryStore::open_with_context(
                path.to_str().unwrap(),
                &DbOpenContext::open_existing_deny(),
            )
            .unwrap(),
        );
    });
    let after = Connection::open(&path).unwrap();
    assert_eq!(
        query_rows(
            &after,
            "SELECT type,name,tbl_name,sql FROM sqlite_schema ORDER BY type,name"
        ),
        schema
    );
    assert_eq!(query_rows(&after, "PRAGMA schema_version"), cookie);
    assert_eq!(backups(&path), original_backups);
}
