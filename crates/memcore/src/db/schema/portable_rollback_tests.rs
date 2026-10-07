//! D7 T12/T13: real file-backed opens plus a pinned pre-B39 policy fixture.
use super::portable_version_tests::{backups, context, fixture, sentinels, stamp_and_mark};
use super::pre_b_39;
use crate::db::migrations::catalogue::test_support::{
    take_invocations, with_future_migration, with_product_then_portable,
};
use crate::db::{DbOpenContext, StoreProfile};
use crate::{MemoryError, MemoryStore};
use rusqlite::Connection;

fn product(conn: &Connection) -> Result<(), MemoryError> {
    conn.execute_batch("CREATE TABLE d7_future_product(value TEXT);")?;
    Ok(())
}
fn refused(path: &std::path::Path, profile: StoreProfile, newer: bool) {
    let error = match pre_b_39::with_policy(39, || {
        MemoryStore::open_with_context(path.to_str().unwrap(), &context(profile, false))
    }) {
        Ok(_) => panic!("old reader must refuse"),
        Err(error) => error,
    };
    if newer {
        assert!(
            matches!(error,MemoryError::InvalidArg(ref message) if message.contains("newer than supported 39"))
        );
    } else {
        assert!(matches!(
            error,
            MemoryError::SchemaMigrationOptInRequired {
                stored: 36,
                expected: 39,
                ..
            }
        ));
    }
    assert!(!pre_b_39::transaction_entered());
}

#[test]
fn pre_b_39_existing_portable_opens_in_b_and_b_created_or_migrated_opens_in_pre_b() {
    let old_dir = tempfile::tempdir().unwrap();
    let old_path = old_dir.path().join("tachi-memory.db");
    drop(
        pre_b_39::with_policy(39, || {
            MemoryStore::open_with_context(
                old_path.to_str().unwrap(),
                &context(StoreProfile::PortableKernel, false),
            )
        })
        .unwrap(),
    );
    stamp_and_mark(&old_path, 39);
    let before = sentinels(&Connection::open(&old_path).unwrap());
    take_invocations();
    drop(
        MemoryStore::open_with_context(
            old_path.to_str().unwrap(),
            &context(StoreProfile::PortableKernel, false),
        )
        .unwrap(),
    );
    assert_eq!(take_invocations(), 0);
    assert_eq!(sentinels(&Connection::open(&old_path).unwrap()), before);
    assert!(backups(&old_path).is_empty());
    for migrated in [false, true] {
        let (_dir, path) = fixture(StoreProfile::PortableKernel);
        if migrated {
            stamp_and_mark(&path, 35);
            drop(
                MemoryStore::open_with_context(
                    path.to_str().unwrap(),
                    &context(StoreProfile::PortableKernel, true),
                )
                .unwrap(),
            );
        }
        let conn = Connection::open(&path).unwrap();
        assert_eq!(
            crate::db::migrations::read_schema_version(&conn).unwrap(),
            39
        );
        let expected = sentinels(&conn);
        drop(conn);
        stamp_and_mark(&path, 39);
        let original = backups(&path);
        drop(
            pre_b_39::with_policy(39, || {
                MemoryStore::open_with_context(
                    path.to_str().unwrap(),
                    &context(StoreProfile::PortableKernel, false),
                )
            })
            .unwrap(),
        );
        assert!(pre_b_39::transaction_entered());
        assert_eq!(backups(&path), original);
        assert_eq!(sentinels(&Connection::open(&path).unwrap()), expected);
    }
}

#[test]
fn product_40_new_or_migrated_portable_rolls_back_and_unchanged_36_still_refuses_old_reader() {
    for migrated in [false, true] {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("tachi-memory.db");
        if migrated {
            drop(
                MemoryStore::open_with_context(
                    path.to_str().unwrap(),
                    &context(StoreProfile::PortableKernel, false),
                )
                .unwrap(),
            );
            stamp_and_mark(&path, 35);
        }
        with_future_migration(false, product, || {
            drop(
                MemoryStore::open_with_context(
                    path.to_str().unwrap(),
                    &context(StoreProfile::PortableKernel, migrated),
                )
                .unwrap(),
            );
            let conn = Connection::open(&path).unwrap();
            assert_eq!(
                crate::db::migrations::read_schema_version(&conn).unwrap(),
                39
            );
            assert_eq!(conn.query_row::<u32,_,_>("SELECT count(*) FROM hard_state WHERE namespace='migrations' AND key='v40_test_future_migration'",[],|r|r.get(0)).unwrap(),0);
            drop(conn);
            stamp_and_mark(&path, 39);
            let original = backups(&path);
            drop(
                pre_b_39::with_policy(39, || {
                    MemoryStore::open_with_context(
                        path.to_str().unwrap(),
                        &context(StoreProfile::PortableKernel, false),
                    )
                })
                .unwrap(),
            );
            assert!(pre_b_39::transaction_entered());
            assert_eq!(backups(&path), original);
        });
    }
    let (_dir, path) = fixture(StoreProfile::PortableKernel);
    stamp_and_mark(&path, 36);
    drop(
        MemoryStore::open_with_context(
            path.to_str().unwrap(),
            &context(StoreProfile::PortableKernel, false),
        )
        .unwrap(),
    );
    refused(&path, StoreProfile::PortableKernel, false);
    assert_eq!(
        crate::db::migrations::read_schema_version(&Connection::open(&path).unwrap()).unwrap(),
        36
    );
    assert!(backups(&path).is_empty());
}

fn restore(source: &std::path::Path, destination: &std::path::Path) {
    let src = Connection::open(source).unwrap();
    let mut dst = Connection::open(destination).unwrap();
    let backup = rusqlite::backup::Backup::new(&src, &mut dst).unwrap();
    assert_eq!(backup.step(-1).unwrap(), rusqlite::backup::StepResult::Done);
}

#[test]
fn product_40_full_migration_refuses_pre_b_and_its_forced_snapshot_restores_prior_version() {
    for from in [38, 39] {
        let (_dir, path) = fixture(StoreProfile::TachiFull);
        stamp_and_mark(&path, from);
        with_future_migration(false, product, || {
            drop(
                MemoryStore::open_with_context(
                    path.to_str().unwrap(),
                    &context(StoreProfile::TachiFull, true),
                )
                .unwrap(),
            );
            assert_eq!(
                crate::db::migrations::read_schema_version(&Connection::open(&path).unwrap())
                    .unwrap(),
                40
            );
        });
        let originals = backups(&path);
        assert_eq!(originals.len(), 1);
        refused(&path, StoreProfile::TachiFull, true);
        assert_eq!(backups(&path), originals);
        let restored = tempfile::tempdir().unwrap();
        let restored_path = restored.path().join("tachi-memory.db");
        restore(&originals[0], &restored_path);
        assert_eq!(
            crate::db::migrations::read_schema_version(&Connection::open(&restored_path).unwrap())
                .unwrap(),
            from
        );
        drop(
            pre_b_39::with_policy(39, || {
                MemoryStore::open_with_context(
                    restored_path.to_str().unwrap(),
                    &context(StoreProfile::TachiFull, from < 39),
                )
            })
            .unwrap(),
        );
    }
}

#[test]
fn product_40_fresh_full_refuses_pre_b_with_only_marker_fallback_backup() {
    for nonempty in [false, true] {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("tachi-memory.db");
        if nonempty {
            Connection::open(&path)
                .unwrap()
                .execute_batch("CREATE TABLE local_fixture(value TEXT);")
                .unwrap();
        }
        with_future_migration(false, product, || {
            if nonempty {
                // The public bare schema door admits unstamped legacy input;
                // the generic Store door retains its strict trigger boundary.
                let mut conn = Connection::open(&path).unwrap();
                crate::db::init_schema_with_label_mut(
                    &mut conn,
                    "global",
                    &path,
                    &DbOpenContext::create_fresh(),
                )
                .unwrap();
            } else {
                drop(
                    MemoryStore::open_with_context(
                        path.to_str().unwrap(),
                        &DbOpenContext::create_fresh(),
                    )
                    .unwrap(),
                );
            }
        });
        assert_eq!(backups(&path).len(), usize::from(nonempty));
        refused(&path, StoreProfile::TachiFull, true);
        assert_eq!(
            crate::db::migrations::read_schema_version(&Connection::open(&path).unwrap()).unwrap(),
            40
        );
    }
}

#[test]
fn portable_41_migration_refuses_old_39_and_40_then_forced_snapshot_restores_39() {
    let (_dir, path) = fixture(StoreProfile::PortableKernel);
    with_product_then_portable(|| {
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
            1,
            "only new Portable41 body, Product40 abovefloor skipped"
        );
        let conn = Connection::open(&path).unwrap();
        assert_eq!(
            crate::db::migrations::read_schema_version(&conn).unwrap(),
            41
        );
        assert_eq!(
            conn.query_row::<String, _, _>("SELECT value FROM d7_portable_41", [], |r| r.get(0))
                .unwrap(),
            "applied"
        );
        assert_eq!(conn.query_row::<u32,_,_>("SELECT count(*) FROM hard_state WHERE namespace='migrations' AND key='v40_test_future_migration'",[],|r|r.get(0)).unwrap(),0);
    });
    let original = backups(&path);
    assert_eq!(original.len(), 1);
    refused(&path, StoreProfile::PortableKernel, true);
    with_future_migration(false, product, || {
        let error = match MemoryStore::open_with_context(
            path.to_str().unwrap(),
            &context(StoreProfile::PortableKernel, false),
        ) {
            Ok(_) => panic!("E40 rejects41"),
            Err(error) => error,
        };
        assert!(
            matches!(error,MemoryError::InvalidArg(ref m) if m.contains("newer than supported 40"))
        );
    });
    let restored = tempfile::tempdir().unwrap();
    let restored_path = restored.path().join("tachi-memory.db");
    restore(&original[0], &restored_path);
    assert_eq!(
        crate::db::migrations::read_schema_version(&Connection::open(&restored_path).unwrap())
            .unwrap(),
        39
    );
    drop(
        pre_b_39::with_policy(39, || {
            MemoryStore::open_with_context(
                restored_path.to_str().unwrap(),
                &context(StoreProfile::PortableKernel, false),
            )
        })
        .unwrap(),
    );
}
