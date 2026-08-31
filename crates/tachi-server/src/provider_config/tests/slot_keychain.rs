use super::*;

#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn keychain_slot_pool_keeps_the_real_account_key_id() {
    let _guard = crate::utils::global_test_lock()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let _password = EnvRestore::set("TACHI_TEST_KEYCHAIN_PASSWORD", "slot-account-key-id");
    let dir = tempfile::tempdir().expect("tempdir");
    let db_path = dir.path().join("memory.db");
    let server = MemoryServer::new(db_path.clone(), None).expect("server");
    crate::vault_ops::handle_vault_init(
        &server,
        crate::vault_ops::VaultInitParams {
            password: "slot-account-key-id".into(),
        },
    )
    .await
    .expect("init vault");
    for (name, value) in [
        ("DEEPSEEK_API_KEY", "account-secret"),
        ("EXTRACT_API_KEY", "vault:DEEPSEEK_API_KEY"),
    ] {
        crate::vault_ops::handle_vault_set(
            &server,
            crate::vault_ops::VaultSetParams {
                name: name.into(),
                value: value.into(),
                agent_id: None,
                secret_type: "api_key".into(),
                description: String::new(),
                allowed_agents: None,
                enable_rotation: false,
                rotation_strategy: None,
                rebind: false,
            },
        )
        .await
        .expect("write fixture");
    }
    drop(server);
    let source = vault_api_key_load_from_keychain(&db_path).expect("actual Keychain source load");
    assert_eq!(source.load.availability, VaultSourceAvailability::Readable);
    let slot = source.load.pools.get("EXTRACT_API_KEY").expect("slot pool");
    assert_eq!(slot.len(), 1);
    assert_eq!(slot[0].key_id, "DEEPSEEK_API_KEY");
    assert_eq!(slot[0].value, "account-secret");
}
