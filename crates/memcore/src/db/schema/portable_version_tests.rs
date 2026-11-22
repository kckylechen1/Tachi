//! D7: owned database fixtures exercise the real admission funnel.

use std::path::Path;

use rusqlite::Connection;

use crate::db::migrations::catalogue::test_support::{take_invocations, with_future_migration};
use crate::db::{DbOpenContext, StoreProfile};
use crate::MemoryError;
use crate::MemoryStore;

pub(super) fn context(profile: StoreProfile, allow: bool) -> DbOpenContext {
    if allow {
        DbOpenContext::open_existing_allow("test:d7")
    } else {
        DbOpenContext::open_existing_deny()
    }
    .with_exact_profile(profile)
}

pub(super) fn shipped_fixture(profile: StoreProfile) -> (tempfile::TempDir, std::path::PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("tachi-memory.db");
    drop(MemoryStore::open_with_context(path.to_str().unwrap(), &context(profile, false)).unwrap());
    (dir, path)
}

pub(super) fn fixture(profile: StoreProfile) -> (tempfile::TempDir, std::path::PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("tachi-memory.db");
    crate::db::migrations::catalogue::test_support::with_prefix(39, || {
        drop(
            MemoryStore::open_with_context(path.to_str().unwrap(), &context(profile, false))
                .unwrap(),
        );
    });
    (dir, path)
}

pub(super) fn backups(path: &Path) -> Vec<std::path::PathBuf> {
    std::fs::read_dir(path.parent().unwrap())
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .filter(|entry| {
            entry
                .file_name()
                .unwrap()
                .to_string_lossy()
                .contains("migration-bak")
        })
        .collect()
}

pub(super) fn sentinels(conn: &Connection) -> Vec<Vec<String>> {
    super::inventory::query_rows(conn, "SELECT namespace,key,value_json,version FROM hard_state WHERE namespace='migrations' ORDER BY key")
}

fn future_body(conn: &Connection) -> Result<(), MemoryError> {
    conn.execute_batch("CREATE TABLE d7_future_effect(value TEXT NOT NULL); INSERT INTO d7_future_effect VALUES ('applied');")?;
    Ok(())
}

#[test]
fn product_only_future_bump_keeps_portable_band_and_refuses_full() {
    super::current_store_admission::with_historical_d7(|| {
        let version = 39;
        {
            let (_dir, path) = fixture(StoreProfile::PortableKernel);
            stamp_and_mark(&path, version);
            let raw = Connection::open(&path).unwrap();
            let before_sentinels = sentinels(&raw);
            let cookie: u32 = raw
                .query_row("PRAGMA schema_version", [], |row| row.get(0))
                .unwrap();
            drop(raw);
            with_future_migration(false, future_body, || {
                take_invocations();
                let store = MemoryStore::open_with_context(
                    path.to_str().unwrap(),
                    &context(StoreProfile::PortableKernel, false),
                )
                .unwrap();
                assert_eq!(
                    take_invocations(),
                    0,
                    "band must bypass the runner entirely"
                );
                assert_eq!(
                    crate::db::migrations::read_schema_version(store.connection()).unwrap(),
                    version
                );
                assert_eq!(sentinels(store.connection()), before_sentinels);
                assert_eq!(
                    store
                        .connection()
                        .query_row::<u32, _, _>("PRAGMA schema_version", [], |row| row.get(0))
                        .unwrap(),
                    cookie
                );
                assert!(backups(&path).is_empty());
                assert!(
                    matches!(crate::store_version_status(&path, StoreProfile::PortableKernel.into()), crate::StoreVersionStatus::Current { stamp } if stamp == version)
                );
            });
        }
        let (_dir, path) = fixture(StoreProfile::TachiFull);
        with_future_migration(false, future_body, || {
            take_invocations();
            let error = match MemoryStore::open_with_context(
                path.to_str().unwrap(),
                &context(StoreProfile::TachiFull, false),
            ) {
                Ok(_) => panic!("Full still requires the Product migration"),
                Err(error) => error,
            };
            assert!(matches!(
                error,
                MemoryError::SchemaMigrationOptInRequired {
                    stored: 39,
                    expected: 40,
                    ..
                }
            ));
            assert_eq!(take_invocations(), 0);
            assert!(backups(&path).is_empty());
        });
    });
}

#[test]
fn future_portable_bump_requires_authority_and_backs_up_previous_state() {
    super::current_store_admission::with_historical_d7(|| {
        let (_dir, path) = fixture(StoreProfile::PortableKernel);
        let before = sentinels(&Connection::open(&path).unwrap());
        with_future_migration(true, future_body, || {
            let error = match MemoryStore::open_with_context(
                path.to_str().unwrap(),
                &context(StoreProfile::PortableKernel, false),
            ) {
                Ok(_) => panic!("Portable migration requires Allow"),
                Err(error) => error,
            };
            assert!(matches!(
                error,
                MemoryError::SchemaMigrationOptInRequired {
                    stored: 39,
                    expected: 40,
                    ..
                }
            ));
            assert!(matches!(
                crate::store_version_status(&path, StoreProfile::PortableKernel.into()),
                crate::StoreVersionStatus::Pending { from: 39, to: 40 }
            ));
            assert!(backups(&path).is_empty());
            take_invocations();
            let store = MemoryStore::open_with_context(
                path.to_str().unwrap(),
                &context(StoreProfile::PortableKernel, true),
            )
            .unwrap();
            assert_eq!(
                take_invocations(),
                1,
                "only the missing new Portable body runs"
            );
            assert_eq!(
                crate::db::migrations::read_schema_version(store.connection()).unwrap(),
                40
            );
            assert_eq!(
                store
                    .connection()
                    .query_row::<String, _, _>("SELECT value FROM d7_future_effect", [], |row| row
                        .get(0))
                    .unwrap(),
                "applied"
            );
            let backups = backups(&path);
            assert_eq!(backups.len(), 1);
            let backup = Connection::open(&backups[0]).unwrap();
            assert_eq!(
                crate::db::migrations::read_schema_version(&backup).unwrap(),
                39
            );
            assert_eq!(sentinels(&backup), before);
            assert_eq!(
                backup
                    .query_row::<u32, _, _>(
                        "SELECT count(*) FROM sqlite_schema WHERE name='d7_future_effect'",
                        [],
                        |row| row.get(0)
                    )
                    .unwrap(),
                0
            );
        });
    });
}

#[test]
fn fresh_portable_at_product_bump_stops_at_rollback_floor() {
    super::current_store_admission::with_historical_d7(|| {
        // Initialize the canonical baseline before injecting the consistent future
        // catalogue; the future Product body is deliberately observable on Full.
        let (_control, _) = fixture(StoreProfile::PortableKernel);
        with_future_migration(false, future_body, || {
            for exact in [false, true] {
                let dir = tempfile::tempdir().unwrap();
                let path = dir.path().join("tachi-memory.db");
                let ctx = if exact {
                    context(StoreProfile::PortableKernel, false)
                } else {
                    DbOpenContext::open_existing_deny().with_profile(StoreProfile::PortableKernel)
                };
                take_invocations();
                let store = MemoryStore::open_with_context(path.to_str().unwrap(), &ctx).unwrap();
                assert_eq!(
                    crate::db::migrations::read_schema_version(store.connection()).unwrap(),
                    39
                );
                assert_eq!(
                    store
                        .connection()
                        .query_row::<u32, _, _>(
                            "SELECT count(*) FROM hard_state WHERE key='v40_test_future_migration'",
                            [],
                            |row| row.get(0)
                        )
                        .unwrap(),
                    0
                );
                assert_eq!(
                    store
                        .connection()
                        .query_row::<u32, _, _>(
                            "SELECT count(*) FROM sqlite_schema WHERE name='d7_future_effect'",
                            [],
                            |row| row.get(0)
                        )
                        .unwrap(),
                    0
                );
                drop(store);
                drop(
                    super::pre_b_39::with_policy(39, || {
                        MemoryStore::open_with_context(path.to_str().unwrap(), &ctx)
                    })
                    .unwrap(),
                );
                assert!(
                    super::pre_b_39::transaction_entered(),
                    "pinned old reader accepted fresh floor image"
                );
            }
        });
    });
}

#[test]
fn portable_band_refuses_missing_old_or_claimed_product_sentinel_before_backup() {
    for (version, key) in [
        (44_u32, "v41_host_typed_records"),
        (44, "v39_mirror_eval_identity"),
        (44, "v44_typed_record_erasure"),
    ] {
        let (_dir, path) = shipped_fixture(StoreProfile::PortableKernel);
        let conn = Connection::open(&path).unwrap();
        conn.execute(
            "DELETE FROM hard_state WHERE namespace='migrations' AND key=?1",
            [key],
        )
        .unwrap();
        drop(conn);
        stamp_and_mark(&path, version);
        let before = sentinels(&Connection::open(&path).unwrap());
        take_invocations();
        let error = match MemoryStore::open_with_context(
            path.to_str().unwrap(),
            &context(StoreProfile::PortableKernel, true),
        ) {
            Ok(_) => panic!("band cannot recover a missing sentinel"),
            Err(error) => error,
        };
        assert!(matches!(error, MemoryError::InvalidArg(ref detail) if detail.contains(key)));
        assert_eq!(take_invocations(), 0);
        assert!(backups(&path).is_empty());
        assert_eq!(sentinels(&Connection::open(&path).unwrap()), before);
    }
}

#[test]
fn portable_pending_recovers_missing_any_index_and_product_bump_restart_is_current() {
    let (_dir, path) = super::current_store_admission::with_historical_d7(|| {
        fixture(StoreProfile::PortableKernel)
    });
    let conn = Connection::open(&path).unwrap();
    crate::db::ensure_reserved_reference_write_guard(&conn).unwrap();
    conn.execute_batch(
        "DELETE FROM hard_state WHERE namespace='migrations' AND key='v39_mirror_eval_identity';",
    )
    .unwrap();
    drop(conn);
    take_invocations();
    drop(
        MemoryStore::open_with_context(
            path.to_str().unwrap(),
            &context(StoreProfile::PortableKernel, true),
        )
        .unwrap(),
    );
    assert_eq!(
        take_invocations(),
        2,
        "selection runs pending v41 and v44 Portable bodies and vacuously stamps v39 baseline"
    );
    assert_eq!(
        crate::db::migrations::read_schema_version(&Connection::open(&path).unwrap()).unwrap(),
        44
    );
    super::current_store_admission::with_historical_d7(|| {
        let (_dir2, path2) = fixture(StoreProfile::PortableKernel);
        stamp_and_mark(&path2, 39);
        let existing_backups = backups(&path2);
        with_future_migration(false, future_body, || {
            take_invocations();
            drop(
                MemoryStore::open_with_context(
                    path2.to_str().unwrap(),
                    &context(StoreProfile::PortableKernel, false),
                )
                .unwrap(),
            );
            assert_eq!(take_invocations(), 0);
            assert_eq!(backups(&path2), existing_backups);
        });
    });
}

pub(super) fn stamp_and_mark(path: &Path, version: u32) {
    let conn = Connection::open(path).unwrap();
    conn.pragma_update(None, "user_version", version).unwrap();
    let fingerprint = super::migration_schema_fingerprint(&conn).unwrap();
    std::fs::write(super::migration_marker_path(path), fingerprint).unwrap();
}

fn change_profile(conn: &Connection, token: &str) {
    conn.execute("UPDATE hard_state SET value_json=json_object('value',?1) WHERE namespace='store_identity' AND key='profile'", [token]).unwrap();
}

#[test]
fn preflight_identity_and_header_are_observed_from_one_wal_commit() {
    use std::cell::RefCell;
    use std::rc::Rc;
    let (_dir, path) = shipped_fixture(StoreProfile::PortableKernel);
    let raw = Connection::open(&path).unwrap();
    crate::db::ensure_reserved_reference_write_guard(&raw).unwrap();
    raw.execute_batch("INSERT INTO hard_state(namespace,key,value_json) VALUES('store_identity','role','{\"value\":\"global\"}');").unwrap();
    drop(raw);
    let observed = Rc::new(RefCell::new(None));
    let receipt = Rc::clone(&observed);
    super::test_hooks::arm_preflight_observer(move |header, role, profile| {
        *receipt.borrow_mut() = Some((header, role, profile));
    });
    super::test_hooks::arm_window_hook(super::test_hooks::Window::AfterPreflightHeader, |path| {
        let writer = Connection::open(path).unwrap();
        crate::db::ensure_reserved_reference_write_guard(&writer).unwrap();
        writer.execute_batch("BEGIN IMMEDIATE; PRAGMA user_version=39; UPDATE hard_state SET value_json='{\"value\":\"tachi_full\"}' WHERE namespace='store_identity' AND key='profile'; UPDATE hard_state SET value_json='{\"value\":\"wiki\"}' WHERE namespace='store_identity' AND key='role'; COMMIT;").unwrap();
    });
    let error = match MemoryStore::open_with_context(
        path.to_str().unwrap(),
        &context(StoreProfile::PortableKernel, false),
    ) {
        Ok(_) => panic!("authoritative Full@39 still requires migration authority"),
        Err(error) => error,
    };
    assert!(matches!(
        error,
        MemoryError::SchemaMigrationOptInRequired { stored: 39, .. }
    ));
    let (header, role, profile) = observed
        .borrow_mut()
        .take()
        .expect("actual preflight reads recorded");
    assert_eq!(header.stored, 44);
    assert_eq!(
        header.probe,
        crate::db::version_policy::ProfileProbe::Portable
    );
    assert_eq!(role.as_deref(), Some("global"));
    assert_eq!(profile, Some(StoreProfile::PortableKernel));
    let raw = Connection::open(&path).unwrap();
    assert_eq!(
        crate::db::migrations::read_schema_version(&raw).unwrap(),
        39
    );
    assert_eq!(
        crate::db::store_identity::read_stamp(&raw, crate::db::STORE_ROLE_KEY)
            .unwrap()
            .as_deref(),
        Some("wiki")
    );
}

#[test]
fn downgrade_wins_before_profile_decode_for_every_probe_class() {
    super::current_store_admission::with_historical_d7(|| {
        for token in [
            Some("portable_kernel"),
            Some("tachi_full"),
            None,
            Some("unrecognized"),
            Some("malformed-json"),
        ] {
            let (_dir, path) = fixture(StoreProfile::PortableKernel);
            let raw = Connection::open(&path).unwrap();
            match token {
                Some("malformed-json") => {
                    raw.execute_batch("UPDATE hard_state SET value_json='{' WHERE namespace='store_identity' AND key='profile';").unwrap();
                }
                Some(token) => change_profile(&raw, token),
                None => {
                    raw.execute(
                        "DELETE FROM hard_state WHERE namespace='store_identity' AND key='profile'",
                        [],
                    )
                    .unwrap();
                }
            }
            let newer = crate::db::migrations::supported_schema_version() + 1;
            raw.pragma_update(None, "user_version", newer).unwrap();
            drop(raw);
            for allow in [false, true] {
                take_invocations();
                let error = match MemoryStore::open_with_context(
                    path.to_str().unwrap(),
                    &context(StoreProfile::PortableKernel, allow),
                ) {
                    Ok(_) => panic!("newer must refuse"),
                    Err(error) => error,
                };
                assert!(
                    matches!(error, MemoryError::InvalidArg(ref detail) if detail.contains(&format!("{newer} newer than supported {}", crate::db::migrations::supported_schema_version()))),
                    "{error:?}"
                );
                assert_eq!(take_invocations(), 0);
                assert!(backups(&path).is_empty());
                assert!(matches!(
                    crate::store_version_status(&path, StoreProfile::PortableKernel.into()),
                    crate::StoreVersionStatus::Newer { stamp } if stamp == newer
                ));
            }
        }
    });
}

#[test]
fn portable_band_to_full_pending_needs_actual_backup_coverage() {
    super::current_store_admission::with_historical_portable_inventory(
        43,
        check_band_to_full_pending,
    );
}

fn check_band_to_full_pending() {
    let (_dir, path) = shipped_fixture(StoreProfile::PortableKernel);
    stamp_and_mark(&path, 41);
    take_invocations();
    super::test_hooks::arm_before_schema_transaction(|path| {
        assert!(
            backups(path).is_empty(),
            "matching Portable marker skipped backup"
        );
        change_profile(&Connection::open(path).unwrap(), "tachi_full");
    });
    let ctx = DbOpenContext::open_existing_allow("test:d7-coverage")
        .with_profile(StoreProfile::PortableKernel);
    let error = match MemoryStore::open_with_context(path.to_str().unwrap(), &ctx) {
        Ok(_) => panic!("unbacked Full migration must refuse"),
        Err(error) => error,
    };
    assert!(matches!(
        error,
        MemoryError::SchemaChangedDuringOpen {
            preflight: 41,
            current: 41,
            ..
        }
    ));
    assert_eq!(take_invocations(), 0);
    assert!(backups(&path).is_empty());
    assert_eq!(
        crate::db::migrations::read_schema_version(&Connection::open(&path).unwrap()).unwrap(),
        41
    );
}

#[test]
fn empty_to_markerless_portable_band_requires_marker_fallback_backup() {
    let (_source_dir, source) = shipped_fixture(StoreProfile::PortableKernel);
    stamp_and_mark(&source, 44);
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("tachi-memory.db");
    drop(Connection::open(&path).unwrap());
    super::test_hooks::arm_before_schema_transaction(move |path| {
        assert!(backups(path).is_empty());
        let from = Connection::open(&source).unwrap();
        let mut to = Connection::open(path).unwrap();
        let backup = rusqlite::backup::Backup::new(&from, &mut to).unwrap();
        assert_eq!(backup.step(-1).unwrap(), rusqlite::backup::StepResult::Done);
    });
    take_invocations();
    let ctx = DbOpenContext::open_existing_allow("test:d7-marker-coverage")
        .with_profile(StoreProfile::PortableKernel);
    let error = match MemoryStore::open_with_context(path.to_str().unwrap(), &ctx) {
        Ok(_) => panic!("unbacked marker fallback must refuse"),
        Err(error) => error,
    };
    assert!(matches!(
        error,
        MemoryError::SchemaChangedDuringOpen {
            preflight: 0,
            current: 44,
            ..
        }
    ));
    assert_eq!(take_invocations(), 0);
    assert!(backups(&path).is_empty());
    assert!(!super::migration_marker_path(&path).exists());
    assert_eq!(
        crate::db::migrations::read_schema_version(&Connection::open(&path).unwrap()).unwrap(),
        44
    );
}

#[test]
fn malformed_portable_index_refuses_in_transaction_and_rolls_back() {
    for version in [39, 44] {
        let (_dir, path) = shipped_fixture(StoreProfile::PortableKernel);
        let raw = Connection::open(&path).unwrap();
        raw.execute_batch("DROP INDEX idx_memories_idless_identity_active; CREATE INDEX idx_memories_idless_identity_active ON memories(idless_identity) WHERE idless_identity IS NOT NULL AND archived=0 AND superseded_by IS NULL;").unwrap();
        drop(raw);
        stamp_and_mark(&path, version);
        let before = super::inventory::schema_inventory(&Connection::open(&path).unwrap());
        let fired = std::rc::Rc::new(std::cell::Cell::new(false));
        let receipt = std::rc::Rc::clone(&fired);
        super::test_hooks::arm_before_schema_transaction(move |_| receipt.set(true));
        let error = match MemoryStore::open_with_context(
            path.to_str().unwrap(),
            &context(StoreProfile::PortableKernel, true),
        ) {
            Ok(_) => panic!("present non-unique identity index is not a valid Portable shape"),
            Err(error) => error,
        };
        assert!(fired.get(), "R5.2b must be an authoritative shape refusal");
        assert!(
            matches!(error, MemoryError::CurrentSchemaIncomplete { ref missing, .. } if missing.iter().any(|name| name.contains("idx_memories_idless_identity_active"))),
            "{error:?}"
        );
        assert_eq!(
            crate::db::migrations::read_schema_version(&Connection::open(&path).unwrap()).unwrap(),
            version
        );
        assert!(
            before == super::inventory::schema_inventory(&Connection::open(&path).unwrap()),
            "shape refusal must roll back maintenance"
        );
        assert_eq!(backups(&path).len(), usize::from(version == 39));
    }
}

#[test]
fn portable_band_without_later_product_sentinels_does_not_create_them() {
    let (_dir, path) = shipped_fixture(StoreProfile::PortableKernel);
    let raw = Connection::open(&path).unwrap();
    raw.execute_batch("DELETE FROM hard_state WHERE namespace='migrations' AND key IN ('v42_coordination_mailbox','v43_adjudication_provenance');").unwrap();
    drop(raw);
    stamp_and_mark(&path, 44);
    let before = sentinels(&Connection::open(&path).unwrap());
    take_invocations();
    let store = MemoryStore::open_with_context(
        path.to_str().unwrap(),
        &context(StoreProfile::PortableKernel, false),
    )
    .unwrap();
    assert_eq!(take_invocations(), 0);
    assert_eq!(
        crate::db::migrations::read_schema_version(store.connection()).unwrap(),
        44
    );
    assert_eq!(sentinels(store.connection()), before);
    assert!(backups(&path).is_empty());
}

#[test]
fn portable_pending_with_missing_typed_records_migrates_before_output_validation() {
    let (_dir, path) = super::current_store_admission::with_historical_d7(|| {
        fixture(StoreProfile::PortableKernel)
    });
    take_invocations();
    let store = MemoryStore::open_with_context(
        path.to_str().unwrap(),
        &context(StoreProfile::PortableKernel, true),
    )
    .unwrap();
    assert_eq!(take_invocations(), 2);
    let has_typed_records: i64 = store
        .connection()
        .query_row(
            "SELECT COUNT(*) FROM sqlite_schema WHERE type='table' AND name='typed_records'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(has_typed_records, 1);
    crate::db::migrations::validate_portable_schema_integrity(store.connection(), 44).unwrap();
    assert_eq!(
        crate::db::migrations::read_schema_version(store.connection()).unwrap(),
        44
    );
    assert_eq!(backups(&path).len(), 1);
}

#[test]
fn full_pending_recovers_missing_old_sentinel_and_preserves_coverage_before_role_error() {
    let (_dir, path) = shipped_fixture(StoreProfile::TachiFull);
    let raw = Connection::open(&path).unwrap();
    crate::db::ensure_reserved_reference_write_guard(&raw).unwrap();
    raw.execute_batch("DELETE FROM hard_state WHERE namespace='migrations' AND key='v39_mirror_eval_identity'; PRAGMA user_version=39;").unwrap();
    drop(raw);
    take_invocations();
    drop(
        MemoryStore::open_with_context(
            path.to_str().unwrap(),
            &context(StoreProfile::TachiFull, true),
        )
        .unwrap(),
    );
    let head = crate::db::migrations::supported_schema_version();
    assert_eq!(take_invocations(), 1);
    assert_eq!(
        crate::db::migrations::read_schema_version(&Connection::open(&path).unwrap()).unwrap(),
        head
    );

    let (_second_dir, path) = shipped_fixture(StoreProfile::TachiFull);
    let raw = Connection::open(&path).unwrap();
    crate::db::ensure_reserved_reference_write_guard(&raw).unwrap();
    raw.execute_batch("INSERT INTO hard_state(namespace,key,value_json) VALUES('store_identity','role','{\"value\":\"global\"}');").unwrap();
    drop(raw);
    // Fresh vector provisioning happens after the schema marker. Converge
    // that marker explicitly so this race isolates skipped backup coverage.
    stamp_and_mark(&path, head);
    super::test_hooks::arm_before_schema_transaction(|path| {
        let writer = Connection::open(path).unwrap();
        crate::db::ensure_reserved_reference_write_guard(&writer).unwrap();
        writer.execute_batch("PRAGMA user_version=39; UPDATE hard_state SET value_json='{\"value\":\"wiki\"}' WHERE namespace='store_identity' AND key='role';").unwrap();
    });
    take_invocations();
    let error = match MemoryStore::open_with_label_and_context(
        path.to_str().unwrap(),
        "global",
        &context(StoreProfile::TachiFull, true),
    ) {
        Ok(_) => panic!("uncovered version change wins before role conflict"),
        Err(error) => error,
    };
    assert!(matches!(
        error,
        MemoryError::SchemaChangedDuringOpen {
            preflight: 44,
            current: 39,
            ..
        }
    ));
    assert_eq!(take_invocations(), 0);
    assert!(backups(&path).is_empty());
}

#[test]
fn status_probe_changes_no_database_or_sidecars_and_refuses_pending_wal() {
    let (_dir, path) = shipped_fixture(StoreProfile::PortableKernel);
    stamp_and_mark(&path, 44);
    let before = std::fs::read(&path).unwrap();
    let marker = std::fs::read(super::migration_marker_path(&path)).unwrap();
    let siblings: Vec<_> = std::fs::read_dir(path.parent().unwrap())
        .unwrap()
        .map(|entry| entry.unwrap().file_name())
        .collect();
    assert!(matches!(
        crate::store_version_status(&path, StoreProfile::PortableKernel.into()),
        crate::StoreVersionStatus::Current { stamp: 44 }
    ));
    assert!(matches!(
        crate::store_version_status(&path, StoreProfile::TachiFull.into()),
        crate::StoreVersionStatus::Refused(MemoryError::StoreProfileMismatch { .. })
    ));
    assert_eq!(std::fs::read(&path).unwrap(), before);
    assert_eq!(
        std::fs::read(super::migration_marker_path(&path)).unwrap(),
        marker
    );
    assert_eq!(
        std::fs::read_dir(path.parent().unwrap())
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .collect::<Vec<_>>(),
        siblings
    );

    let writer = Connection::open(&path).unwrap();
    writer
        .execute_batch("PRAGMA wal_autocheckpoint=0; PRAGMA user_version=39;")
        .unwrap();
    let wal = crate::pending_legacy_sidecar(&path)
        .unwrap()
        .expect("committed WAL held by writer");
    let bytes = std::fs::read(&wal).unwrap();
    assert!(matches!(
        crate::store_version_status(&path, StoreProfile::PortableKernel.into()),
        crate::StoreVersionStatus::Refused(_)
    ));
    assert_eq!(std::fs::read(&wal).unwrap(), bytes);
    assert_eq!(std::fs::read(&path).unwrap(), before);
}

#[test]
fn portable_band_reopen_preserves_stamp_schema_and_matching_backup_marker() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("tachi-memory.db");
    let context =
        DbOpenContext::open_existing_deny().with_exact_profile(StoreProfile::PortableKernel);
    drop(MemoryStore::open_with_context(path.to_str().unwrap(), &context).unwrap());
    stamp_and_mark(&path, 44);
    let before = super::inventory::schema_inventory(&Connection::open(&path).unwrap());
    let cookie: u32 = Connection::open(&path)
        .unwrap()
        .query_row("PRAGMA schema_version", [], |row| row.get(0))
        .unwrap();
    let fingerprint = std::fs::read(super::migration_marker_path(&path)).unwrap();
    let result = MemoryStore::open_with_context(path.to_str().unwrap(), &context);
    let store = match result {
        Ok(store) => store,
        Err(error) => panic!("complete Portable band must admit Deny: {error:?}"),
    };
    assert_eq!(
        crate::db::migrations::read_schema_version(store.connection()).unwrap(),
        44
    );
    drop(store);
    // Compare equally fresh offline connections: vec0 exposes its transient
    // index metadata lazily, so an already-used handle is not a raw snapshot.
    let after_conn = Connection::open(&path).unwrap();
    let after = super::inventory::schema_inventory(&after_conn);
    let changes: Vec<_> = before["tables"]
        .as_object()
        .unwrap()
        .keys()
        .filter(|name| before["tables"][*name] != after["tables"][*name])
        .collect();
    let removed: Vec<_> = before["objects"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|row| !after["objects"].as_array().unwrap().contains(row))
        .collect();
    let added: Vec<_> = after["objects"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|row| !before["objects"].as_array().unwrap().contains(row))
        .collect();
    assert!(
        before == after,
        "changed tables: {changes:?}; removed: {removed:?}; added: {added:?}"
    );
    assert_eq!(
        cookie,
        after_conn
            .query_row::<u32, _, _>("PRAGMA schema_version", [], |row| row.get(0))
            .unwrap()
    );
    assert_eq!(
        fingerprint,
        std::fs::read(super::migration_marker_path(&path)).unwrap()
    );
    assert!(std::fs::read_dir(dir.path()).unwrap().all(|entry| !entry
        .unwrap()
        .file_name()
        .to_string_lossy()
        .contains("migration-bak")));
}

#[test]
fn band_missing_objects_refuse_preflight_but_present_bad_definitions_refuse_in_transaction() {
    super::current_store_admission::with_historical_d7(|| {
        use std::cell::Cell;
        use std::rc::Rc;
        for (mutation, named, transactional) in [
            ("ALTER TABLE derived_items DROP COLUMN summary;", "derived_items.summary", false),
            ("DROP TABLE derived_items;", "derived_items", false),
            ("DROP TRIGGER memory_search_generation_after_insert;", "memory_search_generation_after_insert", false),
            ("DROP INDEX idx_memories_idless_identity_active; CREATE UNIQUE INDEX idx_memories_idless_identity_active ON memories(idless_identity) WHERE idless_identity IS NOT NULL;", "idx_memories_idless_identity_active", true),
            ("DROP INDEX idx_memories_path_active_ts; CREATE INDEX idx_memories_path_active_ts ON memories(path,timestamp) WHERE archived=1;", "idx_memories_path_active_ts", true),
            ("DROP TABLE memories_vec; CREATE TABLE memories_vec(value TEXT);", "memories_vec", true),
        ] {
            let (_dir,path)=fixture(StoreProfile::PortableKernel);
            let conn=Connection::open(&path).unwrap();conn.execute_batch(mutation).unwrap();drop(conn);
            stamp_and_mark(&path,39);
            let conn=Connection::open(&path).unwrap();let schema=super::inventory::query_rows(&conn,"SELECT type,name,tbl_name,sql FROM sqlite_schema ORDER BY type,name");let before=sentinels(&conn);drop(conn);
            let entered=Rc::new(Cell::new(false));let receipt=Rc::clone(&entered);
            super::test_hooks::arm_before_schema_transaction(move |_|receipt.set(true));
            let error=match MemoryStore::open_with_context(path.to_str().unwrap(),&context(StoreProfile::PortableKernel,true)) {Ok(_)=>panic!("damaged band must refuse {named}"),Err(error)=>error};
            super::test_hooks::disarm_window_hooks();
            assert_eq!(entered.get(),transactional,"correct refusing validator for {named}: {error:?}");
            assert!(error.to_string().contains(named),"named object refused: {error:?}");
            if transactional {assert!(matches!(error,MemoryError::CurrentSchemaIncomplete {ref missing,..} if missing.iter().any(|key|key.starts_with("shape:"))),"shape validator, not mere absence: {error:?}");}
            assert!(backups(&path).is_empty());let conn=Connection::open(&path).unwrap();
            assert_eq!(crate::db::migrations::read_schema_version(&conn).unwrap(),39);assert_eq!(sentinels(&conn),before);
            assert_eq!(super::inventory::query_rows(&conn,"SELECT type,name,tbl_name,sql FROM sqlite_schema ORDER BY type,name"),schema,"no repair on refusal");
        }
    });
}

#[test]
fn pre_enum_band_shape_rebuild_is_refused_and_rolled_back_inside_transaction() {
    super::current_store_admission::with_historical_d7(|| {
        use std::cell::Cell;
        use std::rc::Rc;
        let (_dir, path) = fixture(StoreProfile::PortableKernel);
        let conn = Connection::open(&path).unwrap();
        let sql: String = conn
            .query_row(
                "SELECT sql FROM sqlite_schema WHERE name='memories'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        let (_, body) = sql.split_once('(').unwrap();
        let old_body = body.replace(",'wiki','guide','eval'", "");
        assert_ne!(
            old_body, body,
            "remove newer enum options from complete columns"
        );
        let indexes:Vec<String>=conn.prepare("SELECT sql FROM sqlite_schema WHERE type='index' AND tbl_name='memories' AND sql IS NOT NULL").unwrap().query_map([],|r|r.get(0)).unwrap().collect::<Result<_,_>>().unwrap();
        let triggers: Vec<String> = conn
            .prepare("SELECT sql FROM sqlite_schema WHERE type='trigger' AND tbl_name='memories'")
            .unwrap()
            .query_map([], |r| r.get(0))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap();
        conn.execute_batch("PRAGMA foreign_keys=OFF; PRAGMA legacy_alter_table=ON;")
            .unwrap();
        conn.execute_batch(&format!("CREATE TABLE memories_pre_enum ({old_body}; DROP TABLE memories; ALTER TABLE memories_pre_enum RENAME TO memories;")).unwrap();
        for definition in indexes.iter().chain(triggers.iter()) {
            conn.execute_batch(definition).unwrap();
        }
        drop(conn);
        stamp_and_mark(&path, 39);
        let conn = Connection::open(&path).unwrap();
        let before = super::inventory::query_rows(
            &conn,
            "SELECT type,name,tbl_name,sql FROM sqlite_schema ORDER BY type,name",
        );
        let keys = sentinels(&conn);
        drop(conn);
        let entered = Rc::new(Cell::new(false));
        let receipt = Rc::clone(&entered);
        super::test_hooks::arm_before_schema_transaction(move |_| receipt.set(true));
        let error = match MemoryStore::open_with_context(
            path.to_str().unwrap(),
            &context(StoreProfile::PortableKernel, true),
        ) {
            Ok(_) => panic!("pre-enum claimed band must refuse"),
            Err(error) => error,
        };
        assert!(entered.get(), "post-maintenance validator must refuse");
        assert!(
            matches!(error, MemoryError::CurrentSchemaIncomplete { .. }),
            "complete-shape refusal: {error:?}"
        );
        assert!(backups(&path).is_empty());
        let conn = Connection::open(&path).unwrap();
        assert_eq!(
            crate::db::migrations::read_schema_version(&conn).unwrap(),
            39
        );
        assert_eq!(sentinels(&conn), keys);
        assert_eq!(
            super::inventory::query_rows(
                &conn,
                "SELECT type,name,tbl_name,sql FROM sqlite_schema ORDER BY type,name"
            ),
            before,
            "frozen rebuild and trigger changes rolled back"
        );
    });
}

#[test]
fn pre_baseline_versions_refuse_across_all_open_doors_without_mutating_store() {
    for profile in [StoreProfile::PortableKernel, StoreProfile::TachiFull] {
        for legacy_version in [28_u32, 36, 38] {
            let (_dir, path) = shipped_fixture(profile);
            stamp_and_mark(&path, legacy_version);
            let before = std::fs::read(&path).unwrap();
            let err = match MemoryStore::open_with_context(
                path.to_str().unwrap(),
                &context(profile, true),
            ) {
                Ok(_) => panic!("pre-baseline v{legacy_version} must fail closed"),
                Err(err) => err,
            };
            assert!(
                err.to_string().contains("archived binary"),
                "unexpected pre-baseline refusal: {err}"
            );
            let rw_err = match MemoryStore::open_existing_read_write(path.to_str().unwrap()) {
                Ok(_) => panic!("read-write door must refuse pre-baseline v{legacy_version}"),
                Err(err) => err,
            };
            assert!(rw_err.to_string().contains("archived binary"));
            assert!(matches!(
                crate::store_version_status(&path, profile.into()),
                crate::StoreVersionStatus::Refused(MemoryError::InvalidArg(ref msg))
                    if msg.contains("archived binary")
            ));
            assert_eq!(std::fs::read(&path).unwrap(), before);
            assert!(backups(&path).is_empty());
        }
    }
}

/// #2041 review: the maintenance read-write door admits Portable band stores
/// without the migrating funnel, so it must run the complete shape check
/// before handing out a writable connection.
#[test]
fn maintenance_read_write_door_requires_complete_portable_band_shape() {
    super::current_store_admission::with_historical_portable_inventory(43, || {
        for version in [41, 42, 43] {
            check_maintenance_shape(version);
        }
    });
    check_maintenance_shape(44);
}

fn check_maintenance_shape(version: u32) {
    let (_dir, path) = shipped_fixture(StoreProfile::PortableKernel);
    stamp_and_mark(&path, version);
    drop(
        MemoryStore::open_existing_read_write(path.to_str().unwrap())
            .expect("healthy Portable band store opens for maintenance"),
    );

    let raw = Connection::open(&path).unwrap();
    raw.execute_batch("DROP INDEX idx_memories_idless_identity_active; CREATE INDEX idx_memories_idless_identity_active ON memories(idless_identity) WHERE idless_identity IS NOT NULL AND archived=0 AND superseded_by IS NULL;").unwrap();
    drop(raw);
    let error = match MemoryStore::open_existing_read_write(path.to_str().unwrap()) {
        Ok(_) => panic!("non-unique identity index is not a valid Portable shape"),
        Err(error) => error,
    };
    assert!(
        matches!(error, MemoryError::CurrentSchemaIncomplete { ref missing, .. } if missing.iter().any(|name| name.contains("idx_memories_idless_identity_active"))),
        "{error:?}"
    );
}

#[test]
fn v44_upgrades_shipped_typed_record_guards_and_preserves_rows() {
    for (profile, old_version) in [
        (StoreProfile::PortableKernel, 41),
        (StoreProfile::TachiFull, 43),
    ] {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("prior.db");
        let kind = crate::RecordKind {
            id: crate::RecordKindId {
                name: "episode".into(),
                version: 1,
            },
            key_schema: serde_json::json!({"type":"object", "properties":{"agent_id":{"type":"string"}}, "additionalProperties":false}),
            payload_schema: serde_json::json!({"type":"string"}),
        };
        let writer =
            crate::RecordWriter::from_host_config("host", "personal", vec![kind.id.clone()])
                .unwrap();
        let record = crate::TypedRecordInput {
            id: "historical".into(),
            kind: kind.id.clone(),
            keys: serde_json::json!({"agent_id":"a"}),
            payload: serde_json::json!("legacy content"),
            links: vec![],
        };
        let policy = crate::KernelPolicy {
            record_registry: Some(std::sync::Arc::new(
                crate::RecordRegistry::new(vec![kind]).unwrap(),
            )),
            ..Default::default()
        };
        let before = crate::db::migrations::catalogue::test_support::with_prefix(43, || {
            let mut store = MemoryStore::open_with_context(
                path.to_str().unwrap(),
                &DbOpenContext::create_fresh()
                    .with_exact_profile(profile)
                    .with_exact_role("personal"),
            )
            .unwrap()
            .with_kernel_policy(policy.clone());
            store
                .append_typed_record(&writer, &record, None)
                .unwrap()
                .record
        });
        let raw = Connection::open(&path).unwrap();
        assert_eq!(
            crate::db::migrations::read_schema_version(&raw).unwrap(),
            old_version
        );
        let old_guards = super::inventory::query_rows(&raw,"SELECT sql FROM sqlite_schema WHERE name IN ('typed_records_no_delete','typed_record_supersessions_no_delete') ORDER BY name");
        assert_eq!(old_guards.len(), 2);
        assert!(!format!("{old_guards:?}").contains("tachi_typed_record_erasure_enabled"));
        drop(raw);
        assert!(
            matches!(MemoryStore::open_with_context(path.to_str().unwrap(),&context(profile,false)),Err(MemoryError::SchemaMigrationOptInRequired {stored,expected:44,..}) if stored==old_version)
        );
        let mut store = MemoryStore::open_with_context(
            path.to_str().unwrap(),
            &context(profile, true).with_exact_role("personal"),
        )
        .unwrap()
        .with_kernel_policy(policy);
        let conn = store.connection();
        assert_eq!(
            crate::db::migrations::read_schema_version(conn).unwrap(),
            44
        );
        assert_eq!(store.get_typed_record(&record.id).unwrap(), Some(before));
        crate::db::validate_persistent_trigger_inventory(conn, true).unwrap();
        assert_eq!(backups(&path).len(), 1);
        let receipt = store
            .purge_typed_records(
                &writer,
                &std::collections::BTreeMap::from([("agent_id".into(), "a".into())]),
            )
            .unwrap();
        assert_eq!(receipt.erased_records, 1);
        drop(store);
        MemoryStore::open_with_context(path.to_str().unwrap(), &context(profile, false)).unwrap();
    }
}
