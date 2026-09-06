use super::*;

#[cfg(unix)]
#[tokio::test]
async fn direct_cli_account_removal_preserves_active_custody_and_retires_slot_alias() {
    use crate::bootstrap::vault_cli::keys::{
        vault_init_with_password, vault_upsert_secret_with_key,
    };
    use std::os::unix::fs::PermissionsExt;

    let temp = tempfile::tempdir().unwrap();
    let db_path = temp.path().join("memory.db");
    let password_file = temp.path().join("fixture-password");
    std::fs::write(&password_file, b"removal-fixture\n").unwrap();
    std::fs::set_permissions(&password_file, std::fs::Permissions::from_mode(0o600)).unwrap();
    let key = vault_init_with_password(&db_path, "removal-fixture".into()).unwrap();
    for (name, value) in [
        ("DEEPSEEK_API_KEY", "removal-fixture-material"),
        ("EXTRACT_API_KEY", "vault:DEEPSEEK_API_KEY"),
    ] {
        vault_upsert_secret_with_key(&db_path, &key, name, "api_key", "fixture", value.into())
            .unwrap();
    }
    let remove = |name: &str| VaultAction::Remove {
        name: name.into(),
        stdin_password: false,
        keychain: false,
        password_file: Some(password_file.clone()),
        insecure_password_file: false,
    };
    let before = open_cli_store_read_only(&db_path)
        .unwrap()
        .vault_get_entry("DEEPSEEK_API_KEY")
        .unwrap()
        .unwrap();
    let error = run_secret_action_with_reader(
        &db_path,
        temp.path(),
        remove("DEEPSEEK_API_KEY"),
        &mut Cursor::new(Vec::<u8>::new()),
    )
    .await
    .expect_err("plain deletion must not retire an active account implicitly")
    .to_string();
    assert!(error.contains("custody"), "{error}");
    assert!(!error.contains("removal-fixture-material"), "{error}");
    run_secret_action_with_reader(
        &db_path,
        temp.path(),
        remove("EXTRACT_API_KEY"),
        &mut Cursor::new(Vec::<u8>::new()),
    )
    .await
    .unwrap();
    let store = open_cli_store_read_only(&db_path).unwrap();
    let after = store.vault_get_entry("DEEPSEEK_API_KEY").unwrap().unwrap();
    assert_eq!(after.encrypted_value, before.encrypted_value);
    assert_eq!(after.nonce, before.nonce);
    assert!(store.vault_get_entry("EXTRACT_API_KEY").unwrap().is_none());
    let accounts = memcore::db::list_provider_accounts(store.connection()).unwrap();
    assert_eq!(accounts.len(), 1);
    assert_eq!(
        accounts[0].status,
        memcore::vault::accounts::ACCOUNT_STATUS_ACTIVE
    );
    let aliases =
        memcore::db::list_provider_account_aliases(store.connection(), &accounts[0].account_id)
            .unwrap();
    assert!(aliases
        .iter()
        .any(|alias| alias.alias_name == "EXTRACT_API_KEY" && alias.retired));
    assert!(aliases
        .iter()
        .any(|alias| alias.alias_name == "DEEPSEEK_API_KEY" && !alias.retired));
    let events =
        memcore::db::list_provider_account_events(store.connection(), &accounts[0].account_id)
            .unwrap();
    assert!(events
        .iter()
        .any(|event| event.event_kind == "alias_retired"));
}
