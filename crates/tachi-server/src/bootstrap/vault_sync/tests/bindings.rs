use super::*;

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
