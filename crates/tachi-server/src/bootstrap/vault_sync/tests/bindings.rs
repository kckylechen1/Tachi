use super::*;

fn bound_account_fixture() -> (PathBuf, [u8; 32], memcore::ProviderAccount) {
    let target_db = temp_db_path();
    let key = [7u8; 32];
    import_validated_vault_bundle(
        &target_db,
        &sample_config(),
        &[
            encrypted_entry("DEEPSEEK_API_KEY", "original-account-material", &key),
            encrypted_entry("EXTRACT_API_KEY", "vault:DEEPSEEK_API_KEY", &key),
        ],
        &[],
        Some(&key),
    )
    .unwrap();
    let store = open_cli_store_read_only(&target_db).unwrap();
    let accounts = memcore::db::list_provider_accounts(store.connection()).unwrap();
    assert_eq!(accounts.len(), 1);
    (target_db, key, accounts[0].clone())
}

fn account_only_bundle(path: &Path, key: &[u8; 32]) {
    let source_db = temp_db_path();
    let source = open_cli_store(&source_db).unwrap();
    source.vault_set_config(&sample_config()).unwrap();
    source
        .vault_upsert_entry(&encrypted_entry(
            "DEEPSEEK_API_KEY",
            "rotated-account-material",
            key,
        ))
        .unwrap();
    export_vault_bundle(&source_db, path, false, false, key).unwrap();
}

#[test]
fn signed_account_only_import_advances_bound_identity_and_event_atomically() {
    let (target_db, key, before) = bound_account_fixture();
    let dir = tempfile::tempdir().unwrap();
    let bundle = dir.path().join("account-only.json");
    account_only_bundle(&bundle, &key);
    let report = import_vault_bundle(&target_db, &bundle, Some(&key), false).unwrap();
    assert_eq!(report.entries_imported, 1);
    let store = open_cli_store_read_only(&target_db).unwrap();
    let accounts = memcore::db::list_provider_accounts(store.connection()).unwrap();
    assert_eq!(accounts.len(), 1);
    let after = &accounts[0];
    let fingerprint_key = memcore::vault::fingerprint::FingerprintKey::derive_from_master_key(&key);
    let member = fingerprint_key.key_fingerprint("deepseek", "rotated-account-material");
    assert_eq!(after.account_id, before.account_id);
    assert_eq!(after.revision, before.revision + 1);
    assert_eq!(
        after.account_fingerprint,
        fingerprint_key.account_fingerprint_from_members([&member])
    );
    let events =
        memcore::db::list_provider_account_events(store.connection(), &before.account_id).unwrap();
    let event = events
        .iter()
        .find(|event| event.event_kind == "fingerprint_observed")
        .expect("account-only import must record its rotation");
    let evidence: serde_json::Value = serde_json::from_str(&event.evidence).unwrap();
    assert_eq!(evidence["old_fingerprint"], before.account_fingerprint);
    assert_eq!(evidence["new_fingerprint"], after.account_fingerprint);
    assert!(events
        .iter()
        .all(|event| !event.evidence.contains("account-material")));
    let account = store.vault_get_entry("DEEPSEEK_API_KEY").unwrap().unwrap();
    assert_eq!(
        crate::vault_crypto::decrypt(&key, &account.encrypted_value, &account.nonce).unwrap(),
        b"rotated-account-material"
    );
    let slot = store.vault_get_entry("EXTRACT_API_KEY").unwrap().unwrap();
    assert_eq!(
        crate::vault_crypto::decrypt(&key, &slot.encrypted_value, &slot.nonce).unwrap(),
        b"vault:DEEPSEEK_API_KEY"
    );
}

#[test]
fn unsigned_account_only_import_cannot_desynchronize_bound_identity() {
    let (target_db, key, account) = bound_account_fixture();
    let before = open_cli_store_read_only(&target_db)
        .unwrap()
        .vault_get_entry("DEEPSEEK_API_KEY")
        .unwrap()
        .unwrap();
    let error = import_validated_vault_bundle(
        &target_db,
        &sample_config(),
        &[encrypted_entry(
            "DEEPSEEK_API_KEY",
            "rotated-account-material",
            &key,
        )],
        &[],
        None,
    )
    .expect_err("tracked account rotation requires verified fingerprint evidence")
    .to_string();
    assert!(
        error.contains("account") && error.contains("password"),
        "{error}"
    );
    let store = open_cli_store_read_only(&target_db).unwrap();
    let after = store.vault_get_entry("DEEPSEEK_API_KEY").unwrap().unwrap();
    assert_eq!(after.encrypted_value, before.encrypted_value);
    assert_eq!(after.nonce, before.nonce);
    assert_eq!(
        memcore::db::get_provider_account(store.connection(), &account.account_id)
            .unwrap()
            .unwrap()
            .account_fingerprint,
        account.account_fingerprint
    );
}

#[test]
fn signed_account_only_import_rolls_back_when_account_event_cannot_persist() {
    let (target_db, key, account) = bound_account_fixture();
    let before = open_cli_store_read_only(&target_db)
        .unwrap()
        .vault_get_entry("DEEPSEEK_API_KEY")
        .unwrap()
        .unwrap();
    let dir = tempfile::tempdir().unwrap();
    let bundle = dir.path().join("account-only.json");
    account_only_bundle(&bundle, &key);
    crate::test_support::with_unrestricted_fixture_connection(&target_db, |connection| {
        // Keep the table present so opening the CLI store cannot recreate it.
        // Only event insertion fails, after its account fingerprint update.
        connection.execute_batch(
            "ALTER TABLE provider_account_events RENAME COLUMN evidence TO fixture_evidence",
        )
    })
    .unwrap();
    let error = import_vault_bundle(&target_db, &bundle, Some(&key), false)
        .expect_err("ciphertext and rotation event are one atomic mutation")
        .to_string();
    assert!(error.contains("record provider account"), "{error}");
    let store = open_cli_store_read_only(&target_db).unwrap();
    let after = store.vault_get_entry("DEEPSEEK_API_KEY").unwrap().unwrap();
    assert_eq!(after.encrypted_value, before.encrypted_value);
    assert_eq!(after.nonce, before.nonce);
    let after_account = memcore::db::get_provider_account(store.connection(), &account.account_id)
        .unwrap()
        .unwrap();
    assert_eq!(after_account.revision, account.revision);
    assert_eq!(
        after_account.account_fingerprint,
        account.account_fingerprint
    );
}

#[test]
fn slot_rotation_bundle_is_refused_before_database_creation() {
    let target_db = temp_db_path();
    let rotation = VaultKeyRotation {
        prefix: "EXTRACT_API_KEY".into(),
        total_keys: 1,
        current_index: 1,
        rotation_strategy: "round_robin".into(),
        created_at: String::new(),
        updated_at: String::new(),
    };
    let error = import_validated_vault_bundle(
        &target_db,
        &sample_config(),
        &[encrypted_entry(
            "EXTRACT_API_KEY_1",
            "obsolete-secret",
            &[7u8; 32],
        )],
        &[rotation],
        Some(&[7u8; 32]),
    )
    .expect_err("slots are bindings, never rotation pools")
    .to_string();
    assert!(
        error.contains("cannot be an API-key rotation pool"),
        "{error}"
    );
    assert!(!target_db.exists());
}

#[test]
fn signed_slot_import_preserves_existing_binding_and_local_account() {
    let target_db = temp_db_path();
    let key = [7u8; 32];
    let account = encrypted_entry("DEEPSEEK_API_KEY", "existing-account-secret", &key);
    let slot = encrypted_entry("EXTRACT_API_KEY", "vault:DEEPSEEK_API_KEY", &key);
    let target = open_cli_store(&target_db).expect("target store");
    target.vault_set_config(&sample_config()).expect("config");
    target.vault_upsert_entry(&account).expect("account");
    target.vault_upsert_entry(&slot).expect("slot");
    drop(target);

    let initialized = import_validated_vault_bundle(
        &target_db,
        &sample_config(),
        std::slice::from_ref(&slot),
        &[],
        Some(&key),
    )
    .expect("a slot-only sync may retain a binding to a local account");
    assert!(!initialized);
    let target = open_cli_store_read_only(&target_db).expect("read target");
    let stored = target.vault_get_entry("EXTRACT_API_KEY").unwrap().unwrap();
    assert_eq!(stored.encrypted_value, slot.encrypted_value);
    assert_eq!(stored.nonce, slot.nonce);
    let stored_account = target.vault_get_entry("DEEPSEEK_API_KEY").unwrap().unwrap();
    assert_eq!(stored_account.encrypted_value, account.encrypted_value);
    assert_eq!(stored_account.nonce, account.nonce);
}

#[test]
fn signed_slot_import_cannot_silently_rebind_existing_account() {
    let target_db = temp_db_path();
    let key = [7u8; 32];
    let old_account = encrypted_entry("DEEPSEEK_API_KEY", "old-account-secret", &key);
    let old_slot = encrypted_entry("EXTRACT_API_KEY", "vault:DEEPSEEK_API_KEY", &key);
    let target = open_cli_store(&target_db).expect("target store");
    target.vault_set_config(&sample_config()).expect("config");
    target.vault_upsert_entry(&old_account).expect("account");
    target.vault_upsert_entry(&old_slot).expect("slot");
    drop(target);

    let new_account = encrypted_entry("SILICONFLOW_API_KEY", "new-account-secret", &key);
    let new_slot = encrypted_entry("EXTRACT_API_KEY", "vault:SILICONFLOW_API_KEY", &key);
    let error = import_validated_vault_bundle(
        &target_db,
        &sample_config(),
        &[new_account, new_slot],
        &[],
        Some(&key),
    )
    .expect_err("sync has no explicit rebind authority")
    .to_string();
    assert!(error.contains("rebind"), "{error}");
    assert!(error.contains("fp1:"), "{error}");
    assert!(!error.contains("old-account-secret"), "{error}");
    assert!(!error.contains("new-account-secret"), "{error}");

    let target = open_cli_store_read_only(&target_db).expect("read target");
    let stored = target.vault_get_entry("EXTRACT_API_KEY").unwrap().unwrap();
    assert_eq!(stored.encrypted_value, old_slot.encrypted_value);
    assert_eq!(stored.nonce, old_slot.nonce);
    assert!(target
        .vault_get_entry("SILICONFLOW_API_KEY")
        .unwrap()
        .is_none());
}

#[test]
fn signed_slot_import_can_migrate_matching_local_account_bytes() {
    let target_db = temp_db_path();
    let key = [7u8; 32];
    let account = encrypted_entry("DEEPSEEK_API_KEY", "existing-account-secret", &key);
    let slot = encrypted_entry("EXTRACT_API_KEY", "existing-account-secret", &key);
    let target = open_cli_store(&target_db).expect("target store");
    target.vault_set_config(&sample_config()).expect("config");
    target.vault_upsert_entry(&account).expect("account");
    target.vault_upsert_entry(&slot).expect("legacy raw slot");
    drop(target);

    import_validated_vault_bundle(&target_db, &sample_config(), &[slot], &[], Some(&key))
        .expect("same-account migration does not change account family");
    let target = open_cli_store_read_only(&target_db).expect("read target");
    let stored = target.vault_get_entry("EXTRACT_API_KEY").unwrap().unwrap();
    let plain = crate::vault_crypto::decrypt(&key, &stored.encrypted_value, &stored.nonce)
        .expect("decrypt slot");
    assert_eq!(plain, b"vault:DEEPSEEK_API_KEY");
    assert_eq!(
        target
            .vault_get_entry("DEEPSEEK_API_KEY")
            .unwrap()
            .unwrap()
            .encrypted_value,
        account.encrypted_value
    );
}
