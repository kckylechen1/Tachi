use super::*;

async fn fixture() -> MemoryServer {
    let db_path = crate::utils::test_fixture_path(format!(
        "vault-slot-binding-acl-{}.sqlite",
        uuid::Uuid::new_v4()
    ));
    let server = MemoryServer::new(db_path, None).expect("create test server");
    crate::vault_test_api::seed_vault(&server, "slot-binding-acl").expect("seed unlocked vault");
    server
}

fn corrupt_entry(server: &MemoryServer, name: &str) {
    with_vault_key(server, |_key| {
        server.with_global_store(|store| {
            let mut entry = store
                .vault_get_entry(name)
                .map_err(|error| error.to_string())?
                .unwrap_or_else(|| panic!("{name} missing"));
            entry.encrypted_value = "malformed-ciphertext".to_string();
            entry.nonce = "malformed-nonce".to_string();
            store
                .vault_upsert_entry(&entry)
                .map_err(|error| error.to_string())
        })
    })
    .expect("corrupt fixture entry");
}

fn entry(server: &MemoryServer, name: &str) -> memcore::vault::VaultEntry {
    server
        .with_global_store(|store| {
            store
                .vault_get_entry(name)
                .map_err(|error| error.to_string())?
                .ok_or_else(|| format!("{name} missing"))
        })
        .expect("read fixture entry")
}

#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn domain_slot_binding_acl_precedes_decrypt_and_preserves_row() {
    let _lock = crate::utils::global_test_lock()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let server = fixture().await;

    crate::vault_test_api::seed_vault_secret(
        &server,
        crate::vault_test_api::VaultSecretFixture {
            name: "DEEPSEEK_API_KEY".into(),
            value: "target-bob-secret".into(),
            secret_type: "api_key".into(),
            description: String::new(),
            allowed_agents: Some(vec!["agent-bob".into()]),
        },
    )
    .expect("restricted target account");
    crate::vault_test_api::owner_set_vault_secret(
        &server,
        "SILICONFLOW_API_KEY",
        "control-secret",
        None,
        "",
        false,
    )
    .await
    .expect("control account");
    crate::vault_test_api::bind_vault_slot(
        &server,
        "EXTRACT_API_KEY",
        "vault:SILICONFLOW_API_KEY",
        false,
        "",
        None,
        Some("agent-alice"),
    )
    .expect("initial slot binding");
    let before = entry(&server, "EXTRACT_API_KEY");

    corrupt_entry(&server, "DEEPSEEK_API_KEY");
    let error = crate::vault_test_api::bind_vault_slot(
        &server,
        "EXTRACT_API_KEY",
        "vault:DEEPSEEK_API_KEY",
        true,
        "",
        None,
        Some("agent-alice"),
    )
    .expect_err("unauthorized target must be denied before decrypt");
    assert!(error.contains("Access denied"), "{error}");
    assert!(error.contains("allowed list"), "{error}");
    assert!(!error.to_ascii_lowercase().contains("decrypt"), "{error}");
    assert!(!error.contains("malformed-ciphertext"), "{error}");

    let after = entry(&server, "EXTRACT_API_KEY");
    assert_eq!(after.encrypted_value, before.encrypted_value);
    assert_eq!(after.nonce, before.nonce);
    assert_eq!(after.allowed_agents, before.allowed_agents);
}

#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn domain_slot_raw_match_skips_unauthorized_healthy_target() {
    let _lock = crate::utils::global_test_lock()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let server = fixture().await;

    crate::vault_test_api::seed_vault_secret(
        &server,
        crate::vault_test_api::VaultSecretFixture {
            name: "DEEPSEEK_API_KEY".into(),
            value: "target-bob-secret".into(),
            secret_type: "api_key".into(),
            description: String::new(),
            allowed_agents: Some(vec!["agent-bob".into()]),
        },
    )
    .expect("restricted target account");

    let error = crate::vault_test_api::bind_vault_slot(
        &server,
        "SUMMARY_API_KEY",
        "target-bob-secret",
        false,
        "",
        None,
        Some("agent-alice"),
    )
    .expect_err("raw matching must not infer an unauthorized target");
    assert!(
        error.contains("second copy") || error.contains("provider account"),
        "{error}"
    );
    assert!(!error.to_ascii_lowercase().contains("decrypt"), "{error}");
    assert!(!error.contains("DEEPSEEK_API_KEY"), "{error}");
    assert!(!error.contains("target-bob-secret"), "{error}");
    let summary = server
        .with_global_store(|store| {
            store
                .vault_get_entry("SUMMARY_API_KEY")
                .map_err(|error| error.to_string())
        })
        .expect("read rejected slot");
    assert!(
        summary.is_none(),
        "rejected raw match must not create a slot"
    );
}

#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn domain_slot_binding_authorized_valid_target_control_succeeds() {
    let _lock = crate::utils::global_test_lock()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let server = fixture().await;

    crate::vault_test_api::seed_vault_secret(
        &server,
        crate::vault_test_api::VaultSecretFixture {
            name: "DEEPSEEK_API_KEY".into(),
            value: "target-bob-secret".into(),
            secret_type: "api_key".into(),
            description: String::new(),
            allowed_agents: Some(vec!["agent-bob".into()]),
        },
    )
    .expect("restricted target account");

    let response = crate::vault_test_api::bind_vault_slot(
        &server,
        "SUMMARY_API_KEY",
        "vault:DEEPSEEK_API_KEY",
        false,
        "",
        None,
        Some("agent-bob"),
    )
    .expect("authorized valid target bind");
    assert_eq!(response.account, "DEEPSEEK_API_KEY");
}
