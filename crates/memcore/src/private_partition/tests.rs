use super::*;
use crate::db::InsertMemoryResult;

#[cfg(feature = "test-support")]
#[test]
fn before_generic_open_hook_is_path_scoped_and_removed_on_drop() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    let fixture = tempfile::tempdir().expect("fixture");
    let target = fixture.path().join("target.sqlite");
    let other = fixture.path().join("other.sqlite");
    fs::write(&target, SQLITE_HEADER).expect("target header");
    fs::write(&other, SQLITE_HEADER).expect("other header");
    let calls = Arc::new(AtomicUsize::new(0));
    let observed = calls.clone();
    let guard = install_before_generic_open_hook_for_test(&target, move |_| {
        observed.fetch_add(1, Ordering::SeqCst);
    })
    .expect("install target hook");

    resolve_generic_open_path(other.to_str().expect("other path")).expect("other open");
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    for _ in 0..2 {
        resolve_generic_open_path(target.to_str().expect("target path")).expect("target open");
    }
    assert_eq!(calls.load(Ordering::SeqCst), 2, "hook is not one-shot");
    drop(guard);
    resolve_generic_open_path(target.to_str().expect("target path")).expect("unhooked open");
    assert_eq!(calls.load(Ordering::SeqCst), 2);
}

#[cfg(feature = "test-support")]
#[test]
fn before_generic_open_hook_guard_preserves_a_later_owner() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    let fixture = tempfile::tempdir().expect("fixture");
    let target = fixture.path().join("target.sqlite");
    fs::write(&target, SQLITE_HEADER).expect("target header");
    let first = install_before_generic_open_hook_for_test(&target, |_| {}).expect("first hook");
    let stale = GenericOpenTestHookGuard {
        db_path: first.db_path.clone(),
        hook: first.hook.clone(),
    };
    assert!(install_before_generic_open_hook_for_test(&target, |_| {}).is_err());
    drop(first);
    let calls = Arc::new(AtomicUsize::new(0));
    let observed = calls.clone();
    let later = install_before_generic_open_hook_for_test(&target, move |_| {
        // Taking the registry lock inside a callback proves it is not held.
        let deadline = std::time::Instant::now() + Duration::from_secs(1);
        while generic_open_test_hooks().try_lock().is_err() {
            assert!(
                std::time::Instant::now() < deadline,
                "registry lock held by callback"
            );
            std::thread::yield_now();
        }
        observed.fetch_add(1, Ordering::SeqCst);
    })
    .expect("later hook");
    drop(stale);
    resolve_generic_open_path(target.to_str().expect("target path")).expect("later open");
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    drop(later);
}

const TRUST_DOMAIN: &str = "td-alpha";
const RECEIPT_OK: &str = "rcpt-ok";
const KEY_VERSION: &str = "kv1";
const TEST_KEY: [u8; 32] = [7u8; 32];

fn ctx(
    subject: &str,
    receipt: &str,
    caps: &[PartitionCapability],
    revoked: bool,
) -> PrivatePartitionOpenContext {
    PrivatePartitionOpenContext {
        trust_domain_id: TrustDomainId::new(TRUST_DOMAIN).unwrap(),
        subject_id: SubjectId::new(subject).unwrap(),
        receipt: CapabilityReceipt::new(receipt).unwrap(),
        capabilities: caps.iter().copied().collect(),
        key_version: KEY_VERSION.to_string(),
        revoked,
    }
}

fn keys() -> StaticKeyProvider {
    keys_for_subjects(&[
        (
            "subject-alice",
            &[
                PartitionCapability::Read,
                PartitionCapability::Write,
                PartitionCapability::Export,
            ][..],
        ),
        (
            "subject-bob",
            &[
                PartitionCapability::Read,
                PartitionCapability::Write,
                PartitionCapability::Export,
            ][..],
        ),
    ])
}

fn keys_for_subjects(subjects: &[(&str, &[PartitionCapability])]) -> StaticKeyProvider {
    let mut provider = StaticKeyProvider::new(TEST_KEY);
    for (subject, capabilities) in subjects {
        provider = provider
            .with_admission(
                TRUST_DOMAIN,
                *subject,
                RECEIPT_OK,
                capabilities.iter().copied(),
                KEY_VERSION,
            )
            .unwrap();
    }
    provider
}

#[test]
fn malformed_private_stamp_is_uniformly_refused() {
    let store = MemoryStore::open_in_memory().expect("open test store");
    let _authorization =
        db::authorize_reserved_reference_write(&store.reserved_reference_write)
            .expect("enter typed identity-write scope");
    store
        .connection()
        .execute(
            "INSERT INTO hard_state \
             (namespace, key, value_json, version, created_at, updated_at) \
             VALUES (?1, ?2, ?3, 1, ?4, ?4)",
            rusqlite::params![
                db::store_profile::STORE_IDENTITY_NAMESPACE,
                STORE_PRIVATE_PARTITION_KEY,
                "not-json",
                db::now_utc_iso(),
            ],
        )
        .expect("seed malformed private stamp");

    let error = verify_stamped_identity(
        &store,
        &AdmittedPartition {
            partition_id: "expected-partition".to_string(),
        },
    )
    .expect_err("malformed private stamp must fail closed");
    assert!(matches!(error, MemoryError::PrivatePartitionRefused));
}

fn entry(id: &str, text: &str) -> MemoryEntry {
    MemoryEntry {
        id: id.to_string(),
        path: "/".to_string(),
        summary: String::new(),
        text: text.to_string(),
        importance: 0.5,
        timestamp: "2026-08-17T00:00:00Z".to_string(),
        valid_from: String::new(),
        valid_until: None,
        category: "fact".to_string(),
        topic: String::new(),
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
        vector: None,
        retention_policy: None,
        domain: None,
        metadata: serde_json::json!({}),
        recall_count: 0,
        query_diversity: 0,
        tier: "raw".to_string(),
    }
}

#[test]
fn single_write_persist_drop_reopen_retains_committed_row() {
    let root = tempfile::tempdir().unwrap();
    let ctx = ctx(
        "subject-alice",
        RECEIPT_OK,
        &[PartitionCapability::Read, PartitionCapability::Write],
        false,
    );
    let keys = keys();
    {
        let mut part = PrivatePartition::open(root.path(), &ctx, &keys).unwrap();
        assert!(matches!(
            part.insert_if_absent(&entry("first-row", "committed-before-seal"))
                .unwrap(),
            InsertMemoryResult::Inserted
        ));
        part.persist().unwrap();
    }
    let reopened = PrivatePartition::open(root.path(), &ctx, &keys).unwrap();
    assert_eq!(
        reopened.get("first-row").unwrap().unwrap().text,
        "committed-before-seal"
    );
}

#[test]
fn drop_without_explicit_persist_does_not_create_successful_seal() {
    let root = tempfile::tempdir().unwrap();
    let ctx = ctx(
        "subject-alice",
        RECEIPT_OK,
        &[PartitionCapability::Read, PartitionCapability::Write],
        false,
    );
    let keys = keys();
    {
        let mut part = PrivatePartition::open(root.path(), &ctx, &keys).unwrap();
        part.insert_if_absent(&entry("unpersisted-row", "must-not-claim-success"))
            .unwrap();
    }
    let sealed = root
        .path()
        .join(ctx.partition_id())
        .join("partition.sealed");
    assert!(
        !sealed.exists(),
        "Drop must not silently claim persistence success"
    );

    let reopened = PrivatePartition::open(root.path(), &ctx, &keys).unwrap();
    assert!(reopened.get("unpersisted-row").unwrap().is_none());
}

#[test]
fn second_live_opener_refuses_until_first_handle_drops() {
    let root = tempfile::tempdir().unwrap();
    let ctx = ctx(
        "subject-alice",
        RECEIPT_OK,
        &[PartitionCapability::Read, PartitionCapability::Write],
        false,
    );
    let keys = keys();
    let first = PrivatePartition::open(root.path(), &ctx, &keys).unwrap();
    let err = match PrivatePartition::open(root.path(), &ctx, &keys) {
        Err(err) => err,
        Ok(_) => panic!("second live opener must refuse"),
    };
    assert!(matches!(err, MemoryError::PrivatePartitionRefused));
    drop(first);
    PrivatePartition::open(root.path(), &ctx, &keys)
        .expect("dropping the first handle releases exclusive ownership");
}

#[cfg(unix)]
#[test]
fn live_lock_symlink_is_refused_without_touching_its_target() {
    let root = tempfile::tempdir().unwrap();
    let ctx = ctx(
        "subject-alice",
        RECEIPT_OK,
        &[PartitionCapability::Read, PartitionCapability::Write],
        false,
    );
    let partition_dir = root.path().join(ctx.partition_id());
    fs::create_dir_all(&partition_dir).unwrap();
    let lock_path = lock_path_for(&partition_dir.join("partition.sealed"));
    let victim = root.path().join("lock-victim.txt");
    fs::write(&victim, b"do-not-touch").unwrap();
    std::os::unix::fs::symlink(&victim, &lock_path).unwrap();

    let err = match PrivatePartition::open(root.path(), &ctx, &keys()) {
        Err(err) => err,
        Ok(_) => panic!("a symlink must never serve as the partition lock"),
    };

    assert!(matches!(err, MemoryError::PrivatePartitionRefused));
    assert_eq!(fs::read(&victim).unwrap(), b"do-not-touch");
    assert!(fs::symlink_metadata(lock_path)
        .unwrap()
        .file_type()
        .is_symlink());
}

#[cfg(unix)]
#[test]
fn predictable_tmp_symlink_is_not_followed_or_clobbered() {
    let root = tempfile::tempdir().unwrap();
    let ctx = ctx(
        "subject-alice",
        RECEIPT_OK,
        &[PartitionCapability::Read, PartitionCapability::Write],
        false,
    );
    let keys = keys();
    {
        let mut part = PrivatePartition::open(root.path(), &ctx, &keys).unwrap();
        part.insert_if_absent(&entry("row-one", "one")).unwrap();
        part.persist().unwrap();
    }

    let sealed = root
        .path()
        .join(ctx.partition_id())
        .join("partition.sealed");
    let predictable_tmp = sealed.with_extension("sealed.tmp");
    let victim = root.path().join("victim.txt");
    fs::write(&victim, b"do-not-clobber").unwrap();
    std::os::unix::fs::symlink(&victim, &predictable_tmp).unwrap();

    {
        let mut part = PrivatePartition::open(root.path(), &ctx, &keys).unwrap();
        part.insert_if_absent(&entry("row-two", "two")).unwrap();
        part.persist().unwrap();
    }

    assert_eq!(fs::read(&victim).unwrap(), b"do-not-clobber");
    assert!(fs::symlink_metadata(&predictable_tmp)
        .unwrap()
        .file_type()
        .is_symlink());
    let reopened = PrivatePartition::open(root.path(), &ctx, &keys).unwrap();
    assert_eq!(reopened.get("row-two").unwrap().unwrap().text, "two");
}

#[cfg(unix)]
#[test]
fn sealed_reader_refuses_symlink_to_a_valid_prior_envelope() {
    let root = tempfile::tempdir().unwrap();
    let ctx = ctx(
        "subject-alice",
        RECEIPT_OK,
        &[PartitionCapability::Read, PartitionCapability::Write],
        false,
    );
    let keys = keys();
    PrivatePartition::open(root.path(), &ctx, &keys)
        .unwrap()
        .persist()
        .unwrap();
    let sealed = root
        .path()
        .join(ctx.partition_id())
        .join("partition.sealed");
    let prior = sealed.with_extension("sealed.prior");
    fs::rename(&sealed, &prior).unwrap();
    std::os::unix::fs::symlink(&prior, &sealed).unwrap();

    let err = match PrivatePartition::open(root.path(), &ctx, &keys) {
        Err(err) => err,
        Ok(_) => panic!("sealed envelope symlinks must not be followed"),
    };
    assert!(matches!(err, MemoryError::PrivatePartitionRefused));
}

#[test]
fn raw_admin_openers_refuse_a_stamped_private_image() {
    let ctx = ctx(
        "subject-alice",
        RECEIPT_OK,
        &[PartitionCapability::Read, PartitionCapability::Write],
        false,
    );
    let part = PrivatePartition::open_in_memory(&ctx, &keys()).unwrap();
    let image = snapshot_sqlite_image(&part.store.conn, &part.identity.partition_id).unwrap();
    let mut file = NamedTempFile::new().unwrap();
    file.write_all(&image).unwrap();
    file.as_file().sync_all().unwrap();

    assert!(crate::db::open_raw(file.path()).is_err());
    assert!(crate::db::open_for_wal_checkpoint(&file.path().to_string_lossy()).is_err());
    let uri = format!("file:{}?mode=ro&immutable=1", file.path().display());
    assert!(crate::db::open_immutable_readonly(&uri).is_err());
}

#[test]
fn d7_product_bump_preserves_old_and_new_real_sealed_images() {
    crate::db::schema::current_store_admission::with_historical_d7(|| {
        use crate::db::migrations::catalogue::test_support::{
            take_invocations, with_future_migration,
        };
        let context = ctx(
            "subject-alice",
            RECEIPT_OK,
            &[PartitionCapability::Read, PartitionCapability::Write],
            false,
        );
        let provider = keys();
        let old_root = tempfile::tempdir().unwrap();
        let mut old = PrivatePartition::open(old_root.path(), &context, &provider).unwrap();
        assert_eq!(
            crate::db::migrations::read_schema_version(&old.store.conn).unwrap(),
            39
        );
        let partition_id = old.identity.partition_id.clone();
        old.persist().unwrap();
        drop(old);
        let old_path = sealed_path_for(old_root.path(), &partition_id);
        let old_envelope = fs::read(&old_path).unwrap();
        // Warm the independent canonical reference at the original catalogue.
        // The invocation receipt below then counts only work on the input image.
        drop(PrivatePartition::open(old_root.path(), &context, &provider).unwrap());
        assert_eq!(fs::read(&old_path).unwrap(), old_envelope);
        with_future_migration(
            false,
            |conn| {
                conn.execute_batch("CREATE TABLE d7_future_product(value TEXT);")?;
                Ok(())
            },
            || {
                let before_b = crate::db::pre_b_39::with_policy(40, || {
                    PrivatePartition::open(old_root.path(), &context, &provider)
                });
                assert!(
                    matches!(
                        before_b,
                        Err(MemoryError::SchemaMigrationOptInRequired {
                            stored: 39,
                            expected: 40,
                            ..
                        })
                    ),
                    "old policy refuses real sealed image at product bump"
                );
                assert_eq!(fs::read(&old_path).unwrap(), old_envelope);
                take_invocations();
                let opened = PrivatePartition::open(old_root.path(), &context, &provider).unwrap();
                assert_eq!(
                    take_invocations(),
                    0,
                    "real private door admits band without versioned bodies"
                );
                assert_eq!(
                    crate::db::migrations::read_schema_version(&opened.store.conn).unwrap(),
                    39
                );
                drop(opened);
                assert_eq!(fs::read(&old_path).unwrap(), old_envelope);
                let new_root = tempfile::tempdir().unwrap();
                let mut new = PrivatePartition::open(new_root.path(), &context, &provider).unwrap();
                assert_eq!(
                    crate::db::migrations::read_schema_version(&new.store.conn).unwrap(),
                    39
                );
                assert_eq!(new.store.conn.query_row::<u32, _, _>("SELECT count(*) FROM hard_state WHERE namespace='migrations' AND key='v40_test_future_migration'", [], |row| row.get(0)).unwrap(), 0);
                assert_eq!(
                    new.store
                        .conn
                        .query_row::<u32, _, _>(
                            "SELECT count(*) FROM sqlite_schema WHERE name='d7_future_product'",
                            [],
                            |row| row.get(0)
                        )
                        .unwrap(),
                    0
                );
                new.persist().unwrap();
                drop(new);
                assert!(
                    fs::read_dir(new_root.path().join(&partition_id))
                        .unwrap()
                        .all(|entry| {
                            let name = entry.unwrap().file_name().to_string_lossy().into_owned();
                            !name.contains("migration-marker")
                                && !name.contains("migration-bak")
                                && !name.ends_with(".db")
                        }),
                    "private images create no plaintext DB or migration artifacts"
                );
                drop(
                    crate::db::pre_b_39::with_policy(39, || {
                        PrivatePartition::open(new_root.path(), &context, &provider)
                    })
                    .unwrap(),
                );
                assert!(
                    crate::db::pre_b_39::transaction_entered(),
                    "real sealed image opens under pinned pre-B39 Deny"
                );
            },
        );
    });
}

#[test]
fn d7_portable_41_refuses_real_sealed_image_without_plaintext_artifacts() {
    crate::db::schema::current_store_admission::with_historical_d7(|| {
        let context = ctx(
            "subject-alice",
            RECEIPT_OK,
            &[PartitionCapability::Read, PartitionCapability::Write],
            false,
        );
        let provider = keys();
        let root = tempfile::tempdir().unwrap();
        let mut image = PrivatePartition::open(root.path(), &context, &provider).unwrap();
        let id = image.identity.partition_id.clone();
        image.persist().unwrap();
        drop(image);
        let path = sealed_path_for(root.path(), &id);
        let envelope = fs::read(&path).unwrap();
        crate::db::migrations::catalogue::test_support::with_product_then_portable(|| {
            crate::db::migrations::catalogue::test_support::take_invocations();
            let opened = PrivatePartition::open(root.path(), &context, &provider);
            assert!(
                matches!(
                    opened,
                    Err(MemoryError::SchemaMigrationOptInRequired {
                        stored: 39,
                        expected: 41,
                        ..
                    })
                ),
                "production private door always Deny"
            );
            assert_eq!(
                crate::db::migrations::catalogue::test_support::take_invocations(),
                0
            );
        });
        assert_eq!(fs::read(&path).unwrap(), envelope);
        assert!(fs::read_dir(root.path().join(id)).unwrap().all(|entry| {
            let name = entry.unwrap().file_name().to_string_lossy().into_owned();
            !name.contains("migration-") && !name.ends_with(".db")
        }));
    });
}

#[test]
fn sealed_current_image_presence_and_previous_trigger_controls() {
    let context = ctx(
        "subject-alice",
        RECEIPT_OK,
        &[PartitionCapability::Read, PartitionCapability::Write],
        false,
    );
    let provider = keys();
    let healthy = PrivatePartition::open_in_memory(&context, &provider).unwrap();
    let image =
        snapshot_sqlite_image(&healthy.store.conn, &healthy.identity.partition_id).unwrap();
    for case in ["healthy", "table", "trigger", "previous"] {
        let root = tempfile::tempdir().unwrap();
        PrivatePartition::open(root.path(), &context, &provider)
            .unwrap()
            .persist()
            .unwrap();
        let path = sealed_path_for(root.path(), &healthy.identity.partition_id);
        let mut raw = rusqlite::Connection::open_in_memory().unwrap();
        raw.deserialize_read_exact(rusqlite::MAIN_DB, image.as_slice(), image.len(), false)
            .unwrap();
        match case {
            "table" => raw.execute_batch("DROP TABLE derived_items").unwrap(),
            "trigger" | "previous" => {
                raw.execute_batch("DROP TRIGGER memory_search_generation_after_update")
                    .unwrap();
                if case == "previous" {
                    raw.execute_batch(&db::previous_update_trigger_for_tests())
                        .unwrap();
                }
            }
            _ => {}
        }
        let damaged = raw.serialize(rusqlite::MAIN_DB).unwrap().to_vec();
        // Encrypt only an owned fixture using the test provider's key and
        // the actual envelope format; no live partition or key is read.
        let payload = serde_json::to_vec(&(&healthy.identity, damaged)).unwrap();
        let (ciphertext_b64, nonce_b64) = vault_kit::encrypt(&TEST_KEY, &payload).unwrap();
        let mut envelope = SEALED_MAGIC.to_vec();
        envelope.extend_from_slice(
            &serde_json::to_vec(&SealedEnvelope {
                ciphertext_b64,
                nonce_b64,
            })
            .unwrap(),
        );
        fs::write(&path, &envelope).unwrap();
        match PrivatePartition::open(root.path(), &context, &provider) {
            Err(MemoryError::CurrentSchemaIncomplete { missing, .. })
                if case == "table" || case == "trigger" =>
            {
                let expected = if case == "table" {
                    "table:derived_items"
                } else {
                    "trigger:memory_search_generation_after_update"
                };
                assert!(missing.iter().any(|key| key == expected));
            }
            Ok(part) if case == "healthy" || case == "previous" => {
                assert_eq!(part.store.store_profile(), StoreProfile::PortableKernel);
                // This production read checks all canonical definitions,
                // so the previous trigger must have been normalized.
                part.store.search_generation().unwrap();
            }
            Err(error) => panic!("{case}: unexpected refusal {error:?}"),
            Ok(_) => panic!("{case}: sealed current damage must be refused"),
        }
        assert_eq!(envelope, fs::read(path).unwrap());
    }
}

#[test]
fn caller_cannot_self_escalate_provider_capabilities() {
    let root = tempfile::tempdir().unwrap();
    let write_ctx = ctx(
        "subject-alice",
        RECEIPT_OK,
        &[PartitionCapability::Read, PartitionCapability::Write],
        false,
    );
    let write_keys = keys();
    {
        let mut part = PrivatePartition::open(root.path(), &write_ctx, &write_keys).unwrap();
        part.insert_if_absent(&entry("existing", "present"))
            .unwrap();
        part.persist().unwrap();
    }

    let read_only_keys =
        keys_for_subjects(&[("subject-alice", &[PartitionCapability::Read][..])]);
    let escalated_ctx = ctx(
        "subject-alice",
        RECEIPT_OK,
        &[
            PartitionCapability::Read,
            PartitionCapability::Write,
            PartitionCapability::Export,
        ],
        false,
    );
    let err = match PrivatePartition::open(root.path(), &escalated_ctx, &read_only_keys) {
        Err(err) => err,
        Ok(_) => panic!("caller-requested Write/Export must not exceed provider grant"),
    };
    assert!(matches!(err, MemoryError::PrivatePartitionRefused));

    let read_ctx = ctx(
        "subject-alice",
        RECEIPT_OK,
        &[PartitionCapability::Read],
        false,
    );
    let part = PrivatePartition::open(root.path(), &read_ctx, &read_only_keys)
        .expect("provider-granted read remains admitted");
    assert_eq!(part.get("existing").unwrap().unwrap().text, "present");
}

#[test]
fn read_only_handle_closes_without_rewriting_the_sealed_partition() {
    let root = tempfile::tempdir().unwrap();
    let write_ctx = ctx(
        "subject-alice",
        RECEIPT_OK,
        &[PartitionCapability::Read, PartitionCapability::Write],
        false,
    );
    let write_keys = keys();
    {
        let mut part = PrivatePartition::open(root.path(), &write_ctx, &write_keys).unwrap();
        part.insert_if_absent(&entry("existing", "present"))
            .unwrap();
        part.close().unwrap();
    }
    let sealed = root
        .path()
        .join(write_ctx.partition_id())
        .join("partition.sealed");
    let before = fs::read(&sealed).unwrap();

    let read_ctx = ctx(
        "subject-alice",
        RECEIPT_OK,
        &[PartitionCapability::Read],
        false,
    );
    let read_keys = keys_for_subjects(&[("subject-alice", &[PartitionCapability::Read][..])]);
    let part = PrivatePartition::open(root.path(), &read_ctx, &read_keys).unwrap();
    assert_eq!(part.get("existing").unwrap().unwrap().text, "present");
    part.close().unwrap();

    assert_eq!(
        fs::read(sealed).unwrap(),
        before,
        "closing a clean read-only handle must not rewrite the encrypted partition"
    );
}

#[test]
fn reopening_private_image_leaves_no_plaintext_migration_artifacts() {
    let root = tempfile::tempdir().unwrap();
    let ctx = ctx(
        "subject-alice",
        RECEIPT_OK,
        &[PartitionCapability::Read, PartitionCapability::Write],
        false,
    );
    let keys = keys();
    PrivatePartition::open(root.path(), &ctx, &keys)
        .unwrap()
        .close()
        .unwrap();
    PrivatePartition::open(root.path(), &ctx, &keys)
        .unwrap()
        .close()
        .unwrap();

    let partition_dir = root.path().join(ctx.partition_id());
    let names = fs::read_dir(partition_dir)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
        .collect::<Vec<_>>();
    assert!(
        names.iter().all(|name| {
            !name.contains("migration-bak")
                && !name.contains("migration-marker")
                && name != "private.sqlite"
        }),
        "private open leaked plaintext migration artifacts: {names:?}"
    );
}

#[test]
fn raw_opener_cannot_mint_a_private_partition_identity_stamp() {
    let file = tempfile::NamedTempFile::new().unwrap();
    let path = file.path().to_path_buf();
    drop(file);
    let store = MemoryStore::open(&path.to_string_lossy()).unwrap();
    drop(store);
    let raw = crate::db::open_raw(&path).unwrap();
    let forged = raw.execute(
        "INSERT INTO hard_state (namespace, key, value_json, version, created_at, updated_at)
         VALUES ('store_identity', 'private_partition', '{}', 1, 'now', 'now')",
        [],
    );
    assert!(
        forged.is_err(),
        "raw opener minted private identity authority"
    );
    drop(raw);
    MemoryStore::open(&path.to_string_lossy())
        .expect("refused forgery must leave the ordinary store openable");
}

#[test]
fn private_partition_debug_and_envelope_redact_secret_identity_material() {
    let root = tempfile::tempdir().unwrap();
    let ctx = PrivatePartitionOpenContext {
        trust_domain_id: TrustDomainId::new("td-secret-debug-domain").unwrap(),
        subject_id: SubjectId::new("subject-secret-debug-id").unwrap(),
        receipt: CapabilityReceipt::new("receipt-secret-debug-token").unwrap(),
        capabilities: [PartitionCapability::Read, PartitionCapability::Write]
            .into_iter()
            .collect(),
        key_version: "kv-secret-debug-version".to_string(),
        revoked: false,
    };
    let keys = StaticKeyProvider::new(TEST_KEY)
        .with_admission(
            "td-secret-debug-domain",
            "subject-secret-debug-id",
            "receipt-secret-debug-token",
            [PartitionCapability::Read, PartitionCapability::Write],
            "kv-secret-debug-version",
        )
        .unwrap();
    let debug_surfaces = [
        format!("{ctx:?}"),
        format!("{:?}", ctx.trust_domain_id),
        format!("{:?}", ctx.subject_id),
        format!("{:?}", ctx.receipt),
        format!("{keys:?}"),
    ];
    for rendered in debug_surfaces {
        for secret in [
            "td-secret-debug-domain",
            "subject-secret-debug-id",
            "receipt-secret-debug-token",
            "kv-secret-debug-version",
            "7, 7, 7",
        ] {
            assert!(
                !rendered.contains(secret),
                "debug surface leaked {secret}: {rendered}"
            );
        }
    }

    let mut part = PrivatePartition::open(root.path(), &ctx, &keys).unwrap();
    part.insert_if_absent(&entry("debug-redaction-row", "stored"))
        .unwrap();
    part.persist().unwrap();
    let sealed = root
        .path()
        .join(ctx.partition_id())
        .join("partition.sealed");
    let sealed_text = String::from_utf8_lossy(&fs::read(sealed).unwrap()).into_owned();
    for secret in [
        "td-secret-debug-domain",
        "subject-secret-debug-id",
        "receipt-secret-debug-token",
        "kv-secret-debug-version",
        &ctx.partition_id(),
    ] {
        assert!(
            !sealed_text.contains(secret),
            "sealed envelope plaintext leaked {secret}: {sealed_text}"
        );
    }
}

#[test]
fn denied_contexts_refuse_before_creating_or_opening_store() {
    let root = tempfile::tempdir().unwrap();
    let keys = keys();
    let missing_read = ctx(
        "subject-alice",
        RECEIPT_OK,
        &[PartitionCapability::Write],
        false,
    );
    let err = match PrivatePartition::open(root.path(), &missing_read, &keys) {
        Err(err) => err,
        Ok(_) => panic!("missing Read must refuse"),
    };
    assert!(matches!(err, MemoryError::PrivatePartitionRefused));
    assert!(!root.path().join(missing_read.partition_id()).exists());

    let wrong_receipt = ctx(
        "subject-alice",
        "receipt-wrong",
        &[PartitionCapability::Read, PartitionCapability::Write],
        false,
    );
    let err = match PrivatePartition::open(root.path(), &wrong_receipt, &keys) {
        Err(err) => err,
        Ok(_) => panic!("wrong receipt must refuse"),
    };
    assert!(matches!(err, MemoryError::PrivatePartitionRefused));
    assert!(!root.path().join(wrong_receipt.partition_id()).exists());

    let revoked = ctx(
        "subject-alice",
        RECEIPT_OK,
        &[PartitionCapability::Read, PartitionCapability::Write],
        true,
    );
    let err = match PrivatePartition::open(root.path(), &revoked, &keys) {
        Err(err) => err,
        Ok(_) => panic!("revoked context must refuse"),
    };
    assert!(matches!(err, MemoryError::PrivatePartitionRefused));
    assert!(!root.path().join(revoked.partition_id()).exists());
}

#[test]
fn two_subjects_cannot_read_each_others_content_or_existence() {
    let root = tempfile::tempdir().unwrap();
    let alice = ctx(
        "subject-alice",
        RECEIPT_OK,
        &[PartitionCapability::Read, PartitionCapability::Write],
        false,
    );
    let bob = ctx(
        "subject-bob",
        RECEIPT_OK,
        &[PartitionCapability::Read, PartitionCapability::Write],
        false,
    );
    let keys = keys();
    {
        let mut part = PrivatePartition::open(root.path(), &alice, &keys).unwrap();
        assert!(matches!(
            part.insert_if_absent(&entry("alice-secret", "SECRET-A"))
                .unwrap(),
            InsertMemoryResult::Inserted
        ));
        part.persist().unwrap();
    }
    let bob_part = PrivatePartition::open(root.path(), &bob, &keys).unwrap();
    assert!(bob_part.get("alice-secret").unwrap().is_none());
    // Opening alice's sealed file under bob's context is not possible: path
    // is derived. Pointing MemoryStore at alice's sealed file refuses.
    let alice_id = alice.partition_id();
    let sealed = root.path().join(alice_id).join("partition.sealed");
    let sealed_error = match MemoryStore::open(&sealed.to_string_lossy()) {
        Err(err) => err,
        Ok(_) => panic!("generic open must not yield a private-partition handle"),
    };
    let malformed = root.path().join("ordinary-malformed.db");
    fs::write(&malformed, b"ordinary malformed database bytes").unwrap();
    let malformed_error = match MemoryStore::open(&malformed.to_string_lossy()) {
        Err(error) => error,
        Ok(_) => panic!("ordinary malformed input must refuse"),
    };
    assert!(matches!(sealed_error, MemoryError::InvalidArg(_)));
    assert_eq!(
        sealed_error.to_string(),
        malformed_error.to_string(),
        "generic open must not disclose that malformed bytes are a sealed partition"
    );
    assert!(!sealed_error.to_string().contains("private"));
    assert!(!sealed_error.to_string().contains("alice"));

    #[cfg(unix)]
    {
        let sealed_link = root.path().join("sealed-link.db");
        std::os::unix::fs::symlink(&sealed, &sealed_link).unwrap();
        let link_error = match MemoryStore::open(&sealed_link.to_string_lossy()) {
            Err(error) => error,
            Ok(_) => panic!("generic open must not follow a sealed-file symlink"),
        };
        assert_eq!(link_error.to_string(), malformed_error.to_string());
    }
    let _ = bob_part;
}

#[test]
fn different_receipt_same_display_shape_refuses() {
    let root = tempfile::tempdir().unwrap();
    let ok = ctx(
        "subject-alice",
        RECEIPT_OK,
        &[PartitionCapability::Read, PartitionCapability::Write],
        false,
    );
    let keys = keys();
    PrivatePartition::open(root.path(), &ok, &keys)
        .unwrap()
        .persist()
        .unwrap();
    let spoofed = ctx(
        "subject-alice",
        "rcpt-other",
        &[PartitionCapability::Read, PartitionCapability::Write],
        false,
    );
    let err = match PrivatePartition::open(root.path(), &spoofed, &keys) {
        Err(err) => err,
        Ok(_) => panic!("spoofed receipt must not open"),
    };
    assert!(matches!(err, MemoryError::PrivatePartitionRefused));
}

#[test]
fn wrong_path_cannot_acquire_authority() {
    let root = tempfile::tempdir().unwrap();
    let keys = keys();
    let alice = ctx(
        "subject-alice",
        RECEIPT_OK,
        &[PartitionCapability::Read, PartitionCapability::Write],
        false,
    );
    let bob = ctx(
        "subject-bob",
        RECEIPT_OK,
        &[PartitionCapability::Read, PartitionCapability::Write],
        false,
    );
    {
        let mut part = PrivatePartition::open(root.path(), &alice, &keys).unwrap();
        part.insert_if_absent(&entry("alice-secret", "SECRET-A"))
            .unwrap();
        part.persist().unwrap();
    }
    let alice_sealed = root
        .path()
        .join(alice.partition_id())
        .join("partition.sealed");
    let bob_dir = root.path().join(bob.partition_id());
    fs::create_dir_all(&bob_dir).unwrap();
    fs::copy(&alice_sealed, bob_dir.join("partition.sealed")).unwrap();
    let err = match PrivatePartition::open(root.path(), &bob, &keys) {
        Err(err) => err,
        Ok(_) => panic!("copied sealed file must not open as bob"),
    };
    assert!(matches!(err, MemoryError::PrivatePartitionRefused));
}

#[test]
fn revoked_context_refuses_even_if_file_present() {
    let root = tempfile::tempdir().unwrap();
    let keys = keys();
    let live = ctx(
        "subject-alice",
        RECEIPT_OK,
        &[PartitionCapability::Read, PartitionCapability::Write],
        false,
    );
    PrivatePartition::open(root.path(), &live, &keys)
        .unwrap()
        .persist()
        .unwrap();
    let revoked = ctx(
        "subject-alice",
        RECEIPT_OK,
        &[PartitionCapability::Read, PartitionCapability::Write],
        true,
    );
    let err = match PrivatePartition::open(root.path(), &revoked, &keys) {
        Err(err) => err,
        Ok(_) => panic!("revoked context must not open"),
    };
    assert!(matches!(err, MemoryError::PrivatePartitionRefused));
}

#[test]
fn export_without_capability_refuses_and_mentions_nothing() {
    let ctx = ctx(
        "subject-alice",
        RECEIPT_OK,
        &[PartitionCapability::Read, PartitionCapability::Write],
        false,
    );
    let keys = keys();
    let part = PrivatePartition::open_in_memory(&ctx, &keys).unwrap();
    let err = match part.export_sealed() {
        Err(err) => err,
        Ok(_) => panic!("export without capability must refuse"),
    };
    assert!(matches!(err, MemoryError::PrivatePartitionRefused));
    assert!(!format!("{err:?}").contains("SECRET"));
}

#[test]
fn cross_partition_claim_cannot_see_foreign_rows() {
    let keys = keys();
    let alice_ctx = ctx(
        "subject-alice",
        RECEIPT_OK,
        &[PartitionCapability::Read, PartitionCapability::Write],
        false,
    );
    let bob_ctx = ctx(
        "subject-bob",
        RECEIPT_OK,
        &[PartitionCapability::Read, PartitionCapability::Write],
        false,
    );
    let mut alice = PrivatePartition::open_in_memory(&alice_ctx, &keys).unwrap();
    let mut bob = PrivatePartition::open_in_memory(&bob_ctx, &keys).unwrap();
    alice
        .insert_if_absent(&entry("alice-row", "SECRET-A"))
        .unwrap();
    bob.insert_if_absent(&entry("bob-row", "SECRET-B")).unwrap();
    // A source that only exists in Bob's partition cannot be claimed on Alice.
    let err = match alice.with_immutable_supersession_transaction(|operation| {
        operation.claim_immutable_supersession("bob-row", "alice-row")
    }) {
        Err(err) => err,
        Ok(receipt) => panic!("foreign source must not claim: {receipt:?}"),
    };
    assert!(
        err.to_string().contains("CAS refused") || err.to_string().contains("refused"),
        "{err}"
    );
    let bob_row = bob.get("bob-row").unwrap().expect("bob row stays");
    assert_eq!(bob_row.text, "SECRET-B");
    let result = alice
        .with_immutable_supersession_transaction(|operation| {
            let result =
                operation.claim_immutable_supersession("alice-row", "alice-successor")?;
            operation.insert_if_absent(&entry("alice-successor", "successor"))?;
            Ok(result)
        })
        .expect("same-partition claim with in-flight target remains legal");
    assert_eq!(
        result,
        crate::SupersessionCommitResult::Applied,
        "same-partition claim must install the edge"
    );
    let receipt_id = crate::SupersessionReceipt::id_for(
        crate::SUPERSESSION_ROUTE_IMMUTABLE_CLAIM,
        crate::SUPERSESSION_ROUTE_IMMUTABLE_CLAIM,
        "alice-row",
        "alice-successor",
    );
    let (receipt_json, _) = alice
        .store
        .get_state_kv(crate::SUPERSESSION_RECEIPT_NAMESPACE, &receipt_id)
        .expect("read private-partition receipt")
        .expect("private-partition claim persists a receipt");
    let receipt: crate::SupersessionReceipt =
        serde_json::from_str(&receipt_json).expect("deserialize private receipt");
    assert_eq!(
        receipt.partition_id.as_deref(),
        Some(alice.identity().partition_id.as_str())
    );
    assert!(
        !receipt.durable,
        "an in-memory private receipt is not sealed-durable"
    );
    assert_eq!(alice.identity().partition_id, alice_ctx.partition_id());
    assert_ne!(alice.identity().partition_id, bob.identity().partition_id);
}

#[test]
fn portable_profile_is_stamped_not_tachi_full() {
    let ctx = ctx(
        "subject-alice",
        RECEIPT_OK,
        &[
            PartitionCapability::Read,
            PartitionCapability::Write,
            PartitionCapability::Export,
        ],
        false,
    );
    let keys = keys();
    let part = PrivatePartition::open_in_memory(&ctx, &keys).unwrap();
    assert_eq!(part.store.profile, StoreProfile::PortableKernel);
    assert_eq!(part.identity().partition_id, ctx.partition_id());
}

#[test]
fn private_receipt_becomes_durable_only_in_the_sealed_image() {
    let root = tempfile::tempdir().unwrap();
    let ctx = ctx(
        "subject-alice",
        RECEIPT_OK,
        &[PartitionCapability::Read, PartitionCapability::Write],
        false,
    );
    let keys = keys();
    let receipt_id = crate::SupersessionReceipt::id_for(
        crate::SUPERSESSION_ROUTE_IMMUTABLE_CLAIM,
        crate::SUPERSESSION_ROUTE_IMMUTABLE_CLAIM,
        "sealed-source",
        "sealed-target",
    );
    {
        let mut part = PrivatePartition::open(root.path(), &ctx, &keys).unwrap();
        part.insert_if_absent(&entry("sealed-source", "source"))
            .unwrap();
        part.insert_if_absent(&entry("sealed-target", "target"))
            .unwrap();
        part.with_immutable_supersession_transaction(|operation| {
            operation.claim_immutable_supersession("sealed-source", "sealed-target")
        })
        .unwrap();
        let (json, _) = part
            .store
            .get_state_kv(crate::SUPERSESSION_RECEIPT_NAMESPACE, &receipt_id)
            .unwrap()
            .unwrap();
        let receipt: crate::SupersessionReceipt = serde_json::from_str(&json).unwrap();
        assert!(!receipt.durable);
        part.persist().unwrap();
        let (json, _) = part
            .store
            .get_state_kv(crate::SUPERSESSION_RECEIPT_NAMESPACE, &receipt_id)
            .unwrap()
            .unwrap();
        let receipt: crate::SupersessionReceipt = serde_json::from_str(&json).unwrap();
        assert!(
            receipt.durable,
            "the live handle must observe durability after sealing succeeds"
        );
        let sealed = root
            .path()
            .join(ctx.partition_id())
            .join("partition.sealed");
        let sealed_before_replay = fs::read(&sealed).unwrap();
        let replay = part
            .with_immutable_supersession_transaction(|operation| {
                operation.claim_immutable_supersession("sealed-source", "sealed-target")
            })
            .unwrap();
        assert_eq!(replay, crate::SupersessionCommitResult::PriorIdempotent);
        part.persist().unwrap();
        assert_eq!(
            fs::read(sealed).unwrap(),
            sealed_before_replay,
            "a no-op replay must not rewrite the sealed envelope"
        );
    }

    let reopened = PrivatePartition::open(root.path(), &ctx, &keys).unwrap();
    let (json, _) = reopened
        .store
        .get_state_kv(crate::SUPERSESSION_RECEIPT_NAMESPACE, &receipt_id)
        .unwrap()
        .unwrap();
    let receipt: crate::SupersessionReceipt = serde_json::from_str(&json).unwrap();
    assert!(receipt.durable);
    assert_eq!(
        receipt.partition_id.as_deref(),
        Some(reopened.identity.partition_id.as_str())
    );
    let event_payload: String = reopened
        .store
        .conn
        .query_row(
            "SELECT payload_json FROM tachi_events WHERE id = ?1",
            [&receipt_id],
            |row| row.get(0),
        )
        .expect("sealed receipt event exists");
    let event_receipt: crate::SupersessionReceipt =
        serde_json::from_str(&event_payload).expect("decode sealed receipt event");
    assert_eq!(
        event_receipt, receipt,
        "hard-state authority and its event projection must cross the seal boundary together"
    );
    assert!(event_receipt.durable);
}
