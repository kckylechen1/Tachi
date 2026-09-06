use super::*;
use memcore::vault::{VaultEntry, VaultKeyHealth};
use tachi_llm::AliasSkipClass;

async fn fixture() -> MemoryServer {
    let db_path = crate::utils::test_fixture_path(format!(
        "vault-slot-materialization-{}.sqlite",
        uuid::Uuid::new_v4()
    ));
    let server = MemoryServer::new(db_path, None).expect("server");
    handle_vault_init(
        &server,
        VaultInitParams {
            password: "slot-materialization".into(),
        },
    )
    .await
    .expect("init");
    server
}

fn seed(server: &MemoryServer, name: &str, value: &str) {
    with_vault_key(server, |key| {
        let (encrypted_value, nonce) = crate::vault_crypto::encrypt(key, value.as_bytes())?;
        server.with_global_store(|store| {
            store
                .vault_upsert_entry(&VaultEntry {
                    name: name.into(),
                    encrypted_value,
                    nonce,
                    secret_type: "api_key".into(),
                    updated_at: chrono::Utc::now().to_rfc3339(),
                    ..Default::default()
                })
                .map_err(|e| e.to_string())
        })
    })
    .expect("seed fixture row");
}

#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn slot_scan_honors_account_health_and_keeps_drop_reason() {
    let _lock = crate::utils::global_test_lock()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let server = fixture().await;
    seed(&server, "DEEPSEEK_API_KEY", "account-secret");
    seed(&server, "EXTRACT_API_KEY", "vault:DEEPSEEK_API_KEY");
    let healthy = crate::vault_ops::load_unlocked_api_key_secret_pools_with_drops(&server).unwrap();
    assert_eq!(
        healthy.pools["EXTRACT_API_KEY"][0].key_id,
        "DEEPSEEK_API_KEY"
    );
    assert_eq!(healthy.pools["EXTRACT_API_KEY"][0].value, "account-secret");

    server
        .with_global_store(|store| {
            store
                .vault_upsert_key_health(&VaultKeyHealth {
                    logical_name: "DEEPSEEK_API_KEY".into(),
                    key_id: "DEEPSEEK_API_KEY".into(),
                    disabled: true,
                    status: "disabled".into(),
                    updated_at: chrono::Utc::now().to_rfc3339(),
                    ..Default::default()
                })
                .map_err(|e| e.to_string())
        })
        .unwrap();
    let scan = crate::vault_ops::load_unlocked_api_key_secret_pools_with_drops(&server).unwrap();
    assert!(
        !scan.pools.contains_key("EXTRACT_API_KEY"),
        "disabled account leaked through slot"
    );
    assert_eq!(
        scan.dropped.get("EXTRACT_API_KEY"),
        Some(&AliasSkipClass::ListedUnusableDisabled)
    );
}

#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn slot_lease_honors_memory_only_account_and_slot_health() {
    let _lock = crate::utils::global_test_lock()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let _persist = EnvRestore::set("TACHI_TEST_DISABLE_PROVIDER_KEY_HEALTH_PERSIST", "1");
    for logical_name in ["DEEPSEEK_API_KEY", "EXTRACT_API_KEY"] {
        let server = fixture().await;
        seed(&server, "DEEPSEEK_API_KEY", "account-secret");
        seed(&server, "EXTRACT_API_KEY", "vault:DEEPSEEK_API_KEY");
        let allowed =
            super::super::access::lease_authorized_api_key(&server, "EXTRACT_API_KEY", None)
                .expect("healthy lease");
        assert_eq!(allowed.key_id, "DEEPSEEK_API_KEY");
        assert_eq!(allowed.value, "account-secret");
        assert_eq!(
            crate::vault_ops::read_unlocked_vault_secret(&server, "EXTRACT_API_KEY", None, false)
                .unwrap(),
            "account-secret"
        );
        server.llm.record_provider_key_result(
            logical_name,
            "DEEPSEEK_API_KEY",
            Some(401),
            None,
            None,
            None,
        );
        let denied =
            super::super::access::lease_authorized_api_key(&server, "EXTRACT_API_KEY", None);
        assert!(
            denied.is_err(),
            "memory-only auth failure under {logical_name} must revoke slot lease"
        );
        assert!(
            crate::vault_ops::read_unlocked_vault_secret(&server, "EXTRACT_API_KEY", None, false)
                .is_err(),
            "memory-only auth failure must also revoke direct slot materialization"
        );
    }
}

#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn slot_scan_preserves_invalid_binding_and_target_classification() {
    let _lock = crate::utils::global_test_lock()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let server = fixture().await;
    seed(&server, "DEEPSEEK_API_KEY", "allowed-secret");
    seed(&server, "SUMMARY_API_KEY", "vault:DEEPSEEK_API_KEY");
    seed(&server, "EXTRACT_API_KEY", "orphan-secret");
    seed(&server, "DISTILL_API_KEY", "vault:MISSING_API_KEY");
    seed(&server, "TAVILY_API_KEY", "search-secret");
    seed(&server, "REASONING_API_KEY", "vault:TAVILY_API_KEY");
    let scan = crate::vault_ops::load_unlocked_api_key_secret_pools_with_drops(&server).unwrap();
    assert_eq!(scan.pools["SUMMARY_API_KEY"][0].value, "allowed-secret");
    for name in ["EXTRACT_API_KEY", "DISTILL_API_KEY", "REASONING_API_KEY"] {
        assert!(
            !scan.pools.contains_key(name),
            "invalid binding admitted as {name}"
        );
        let class = scan
            .dropped
            .get(name)
            .expect("listed slot must keep a drop reason");
        assert!(class.is_listed_integrity());
        assert!(!class
            .operator_reason(name)
            .contains("absent from a readable Vault"));
    }
    assert_eq!(
        scan.dropped.get("REASONING_API_KEY"),
        Some(&AliasSkipClass::ListedNotModelProvider)
    );
}

#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn slot_scan_ignores_invalid_legacy_slot_rotation_without_admitting_members() {
    let _lock = crate::utils::global_test_lock()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let server = fixture().await;
    seed(&server, "DEEPSEEK_API_KEY", "allowed-secret");
    seed(&server, "EXTRACT_API_KEY", "vault:DEEPSEEK_API_KEY");
    seed(&server, "EXTRACT_API_KEY_1", "obsolete-member-secret");
    server
        .with_global_store(|store| {
            store
                .vault_set_rotation(&memcore::vault::VaultKeyRotation {
                    prefix: "EXTRACT_API_KEY".into(),
                    total_keys: 0,
                    current_index: 0,
                    rotation_strategy: "round_robin".into(),
                    created_at: String::new(),
                    updated_at: String::new(),
                })
                .map_err(|e| e.to_string())
        })
        .unwrap();
    let pools = crate::vault_ops::load_unlocked_api_key_secret_pools(&server)
        .expect("obsolete slot pool metadata must not block a valid account binding");
    assert_eq!(pools["EXTRACT_API_KEY"][0].key_id, "DEEPSEEK_API_KEY");
    assert!(!pools.contains_key("EXTRACT_API_KEY_1"));
}
