use super::*;
use memcore::vault::{VaultEntry, VaultKeyHealth};
use tachi_llm::AliasSkipClass;

async fn fixture() -> MemoryServer {
    let db_path = crate::utils::test_fixture_path(format!(
        "vault-slot-materialization-{}.sqlite",
        uuid::Uuid::new_v4()
    ));
    let server = MemoryServer::new(db_path, None).expect("server");
    crate::vault_test_api::seed_vault(&server, "slot-materialization").expect("init");
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

/// Durable integration for the front-line provider selector
/// (`EXTRACT_PROVIDER`/`SUMMARY_PROVIDER=deepseek`): the server's LlmClient is built from env
/// by the normal constructor (selector active, canonical key absent from
/// env), then the real Vault refresh materializes synthetic pools from the
/// isolated test DB. The selected lane must keep its canonical chain —
/// endpoint/model from the DeepSeek descriptor, credential only from the
/// materialized `DEEPSEEK_API_KEY` pool — while a valid stale lane-alias pool
/// for SiliconFlow coexists in the same Vault but is never consulted by either
/// selected lane.
#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn selected_deepseek_front_line_keeps_canonical_chain_through_durable_vault_refresh() {
    let _lock = crate::utils::global_test_lock()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let _selector = EnvRestore::set("EXTRACT_PROVIDER", "deepseek");
    let _summary_selector = EnvRestore::set("SUMMARY_PROVIDER", "deepseek");
    let _no_canonical_env = EnvRestore::remove("DEEPSEEK_API_KEY");
    let _no_stale_env = EnvRestore::remove("EXTRACT_API_KEY");
    let _no_base = EnvRestore::remove("EXTRACT_BASE_URL");
    let _no_model = EnvRestore::remove("EXTRACT_MODEL");
    let _no_sf = EnvRestore::remove("SILICONFLOW_API_KEY");

    let server = fixture().await;
    seed(&server, "DEEPSEEK_API_KEY", "durable-deepseek-secret");
    seed(&server, "SILICONFLOW_API_KEY", "durable-siliconflow-secret");
    seed(&server, "EXTRACT_API_KEY", "vault:SILICONFLOW_API_KEY");
    server
        .refresh_llm_provider_secrets_from_vault()
        .expect("durable vault refresh with a selected front-line provider");

    let runtime = server.llm.runtime_config();
    assert_eq!(
        runtime.extract.api_key_envs,
        vec!["DEEPSEEK_API_KEY"],
        "selected lane chain must stay canonical after durable materialization"
    );
    assert_eq!(
        runtime.extract.base_url,
        "https://api.deepseek.com/chat/completions"
    );
    assert_eq!(runtime.extract.model, "deepseek-v4-flash");
    assert_eq!(runtime.summary.api_key_envs, vec!["DEEPSEEK_API_KEY"]);
    assert_eq!(
        runtime.summary.base_url,
        "https://api.deepseek.com/chat/completions"
    );
    assert_eq!(
        server
            .llm
            .provider_key_id_for_tests(&runtime.extract.api_key_envs)
            .as_deref(),
        Some("DEEPSEEK_API_KEY"),
        "the materialized canonical pool must serve the selected lane"
    );
    assert_eq!(
        server
            .llm
            .provider_secret_for_tests(&["DEEPSEEK_API_KEY"])
            .as_deref(),
        Some("durable-deepseek-secret")
    );
    assert_eq!(
        server
            .llm
            .provider_key_id_for_tests(&["EXTRACT_API_KEY"])
            .as_deref(),
        Some("SILICONFLOW_API_KEY"),
        "the valid stale alias pool is published but selected lanes never consult it"
    );
}

/// A readable Vault refresh is transactional: selected-provider transport
/// provenance must reject an HTTP or non-443 overlay before either the catalog
/// or provider pools replace a healthy snapshot.
#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn selected_deepseek_vault_overlay_rejects_transport_downgrade_without_publication() {
    let _lock = crate::utils::global_test_lock()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let _selector = EnvRestore::set("EXTRACT_PROVIDER", "deepseek");
    let _no_canonical_env = EnvRestore::remove("DEEPSEEK_API_KEY");
    let _no_base = EnvRestore::remove("EXTRACT_BASE_URL");
    let _no_model = EnvRestore::remove("EXTRACT_MODEL");
    let _no_sf = EnvRestore::remove("SILICONFLOW_API_KEY");
    let server = fixture().await;
    seed(&server, "DEEPSEEK_API_KEY", "durable-deepseek-secret");
    server
        .refresh_llm_provider_secrets_from_vault()
        .expect("healthy selected-provider snapshot");
    let before = server.llm.runtime_config();
    assert_eq!(
        server
            .llm
            .provider_secret_for_tests(&["DEEPSEEK_API_KEY"])
            .as_deref(),
        Some("durable-deepseek-secret")
    );

    seed(
        &server,
        "EXTRACT_BASE_URL",
        "https://api.deepseek.com:8443/chat/completions",
    );
    let error = server
        .refresh_llm_provider_secrets_from_vault()
        .expect_err("selected-provider non-443 Vault overlay must be rejected");
    assert!(
        error.contains("https://api.deepseek.com")
            && error.contains("refusing credential-bearing request"),
        "unexpected rejection error: {error}"
    );
    assert_eq!(
        server.llm.runtime_config(),
        before,
        "rejected overlay must not publish an endpoint or catalog projection"
    );
    assert_eq!(
        server
            .llm
            .provider_secret_for_tests(&["DEEPSEEK_API_KEY"])
            .as_deref(),
        Some("durable-deepseek-secret"),
        "rejected refresh must retain the healthy canonical provider pool"
    );
}

/// The selected transport fence is independent of the pool's current
/// availability: an empty initial Vault must still reject a bad overlay before
/// it can publish a catalog entry that a later key materialization would use.
#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn selected_deepseek_empty_vault_rejects_transport_downgrade_before_later_key() {
    let _lock = crate::utils::global_test_lock()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let _selector = EnvRestore::set("EXTRACT_PROVIDER", "deepseek");
    let _no_canonical_env = EnvRestore::remove("DEEPSEEK_API_KEY");
    let _no_base = EnvRestore::remove("EXTRACT_BASE_URL");
    let _no_model = EnvRestore::remove("EXTRACT_MODEL");
    let _no_sf = EnvRestore::remove("SILICONFLOW_API_KEY");
    let server = fixture().await;
    seed(
        &server,
        "EXTRACT_BASE_URL",
        "http://api.deepseek.com/chat/completions",
    );

    let error = server
        .refresh_llm_provider_secrets_from_vault()
        .expect_err("selected-provider HTTP overlay must fail without a key pool");
    assert!(
        error.contains("https://api.deepseek.com")
            && error.contains("refusing credential-bearing request"),
        "unexpected rejection error: {error}"
    );
    assert!(
        server
            .llm
            .provider_secret_for_tests(&["DEEPSEEK_API_KEY"])
            .is_none(),
        "no pool may be published before a later canonical key materializes"
    );
}

#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn lane_slot_set_binds_account_instead_of_copying_ciphertext() {
    let _lock = crate::utils::global_test_lock()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let db_path = crate::utils::test_fixture_path(format!(
        "memory-server-vault-account-bind-{}.sqlite",
        uuid::Uuid::new_v4()
    ));
    let server = MemoryServer::new(db_path, None).expect("create test server");
    crate::vault_test_api::seed_vault(&server, "account-bind").expect("seed unlocked vault");

    crate::vault_test_api::owner_set_vault_secret(
        &server,
        "DEEPSEEK_API_KEY",
        "deepseek-secret-bytes",
        None,
        "",
        false,
    )
    .await
    .expect("account write");

    let bound = crate::vault_test_api::bind_vault_slot(
        &server,
        "EXTRACT_API_KEY",
        "deepseek-secret-bytes",
        false,
        "",
        None,
        None,
    )
    .expect("slot bind");
    assert_eq!(bound.account, "DEEPSEEK_API_KEY");
    assert_ne!(bound.fingerprint, "");

    let got = crate::vault_ops::read_unlocked_vault_secret(&server, "EXTRACT_API_KEY", None, false)
        .expect("get slot");
    assert_eq!(got, "deepseek-secret-bytes");
    assert_eq!(
        fixture_vault_plaintext(&server, "EXTRACT_API_KEY"),
        "vault:DEEPSEEK_API_KEY"
    );
    assert_ne!(
        fixture_vault_plaintext(&server, "EXTRACT_API_KEY"),
        "deepseek-secret-bytes"
    );

    let pools = crate::vault_ops::load_unlocked_api_key_secret_pools(&server).expect("pools");
    let extract = pools
        .get("EXTRACT_API_KEY")
        .expect("slot pool must resolve through the account");
    assert_eq!(extract[0].value, "deepseek-secret-bytes");
    assert_eq!(extract[0].key_id, "DEEPSEEK_API_KEY");

    let orphan = crate::vault_test_api::bind_vault_slot(
        &server,
        "DISTILL_API_KEY",
        "glm-orphan-secret",
        true,
        "",
        None,
        None,
    )
    .expect_err("unmatched bytes must not copy into a slot even with rebind");
    assert!(
        orphan.contains("second copy") || orphan.contains("provider account"),
        "{orphan}"
    );
    assert!(!orphan.contains("glm-orphan-secret"), "{orphan}");

    crate::vault_test_api::owner_set_vault_secret(
        &server,
        "SILICONFLOW_API_KEY",
        "siliconflow-secret-bytes",
        None,
        "",
        false,
    )
    .await
    .expect("second account");
    let refused = crate::vault_test_api::bind_vault_slot(
        &server,
        "EXTRACT_API_KEY",
        "siliconflow-secret-bytes",
        false,
        "",
        None,
        None,
    )
    .expect_err("family change needs rebind");
    assert!(
        refused.contains("--rebind") || refused.contains("rebind=true"),
        "{refused}"
    );

    let rebound = crate::vault_test_api::bind_vault_slot(
        &server,
        "EXTRACT_API_KEY",
        "siliconflow-secret-bytes",
        true,
        "",
        None,
        None,
    )
    .expect("rebind pointer");
    assert_eq!(rebound.account, "SILICONFLOW_API_KEY");
    assert!(rebound.rebound);

    let got_rebind =
        crate::vault_ops::read_unlocked_vault_secret(&server, "EXTRACT_API_KEY", None, false)
            .expect("get rebound slot");
    assert_eq!(got_rebind, "siliconflow-secret-bytes");
    assert_eq!(
        fixture_vault_plaintext(&server, "EXTRACT_API_KEY"),
        "vault:SILICONFLOW_API_KEY"
    );

    crate::vault_test_api::owner_set_vault_secret(
        &server,
        "DEEPSEEK_API_KEY",
        "deepseek-rotated-bytes",
        None,
        "",
        false,
    )
    .await
    .expect("account rotation does not need rebind");
}

#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn lane_slot_same_pointer_write_persists_allowed_agents() {
    let _lock = crate::utils::global_test_lock()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let db_path = crate::utils::test_fixture_path(format!(
        "memory-server-vault-slot-metadata-{}.sqlite",
        uuid::Uuid::new_v4()
    ));
    let server = MemoryServer::new(db_path, None).expect("create test server");
    crate::vault_test_api::seed_vault(&server, "slot-metadata").expect("seed unlocked vault");
    crate::vault_test_api::owner_set_vault_secret(
        &server,
        "DEEPSEEK_API_KEY",
        "deepseek-secret-bytes",
        None,
        "",
        false,
    )
    .await
    .expect("account write");
    crate::vault_test_api::bind_vault_slot(
        &server,
        "EXTRACT_API_KEY",
        "deepseek-secret-bytes",
        false,
        "",
        None,
        None,
    )
    .expect("first bind");

    let stored = crate::vault_test_api::bind_vault_slot(
        &server,
        "EXTRACT_API_KEY",
        "vault:DEEPSEEK_API_KEY",
        false,
        "extract lane",
        Some(vec!["lane-bot".to_string()]),
        None,
    )
    .expect("metadata write");
    assert!(stored.noop);

    let got = crate::vault_ops::read_unlocked_vault_secret(
        &server,
        "EXTRACT_API_KEY",
        Some("lane-bot"),
        false,
    )
    .expect("get slot");
    assert_eq!(got, "deepseek-secret-bytes");
    assert_eq!(
        fixture_vault_plaintext(&server, "EXTRACT_API_KEY"),
        "vault:DEEPSEEK_API_KEY"
    );
    let entry = fixture_vault_entry(&server, "EXTRACT_API_KEY");
    assert_eq!(entry.description, "extract lane");
    assert_eq!(entry.allowed_agents, Some(vec!["lane-bot".into()]));
}

#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn lane_slot_pool_skips_restricted_or_unhealthy_target() {
    let _lock = crate::utils::global_test_lock()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let db_path = crate::utils::test_fixture_path(format!(
        "memory-server-vault-slot-target-policy-{}.sqlite",
        uuid::Uuid::new_v4()
    ));
    let server = MemoryServer::new(db_path, None).expect("create test server");
    crate::vault_test_api::seed_vault(&server, "slot-target-policy").expect("seed unlocked vault");
    crate::vault_test_api::seed_vault_secret(
        &server,
        crate::vault_test_api::VaultSecretFixture {
            name: "DEEPSEEK_API_KEY".to_string(),
            value: "deepseek-secret-bytes".to_string(),
            secret_type: "api_key".into(),
            description: String::new(),
            allowed_agents: Some(vec!["owner-bot".to_string()]),
        },
    )
    .expect("restricted account");
    crate::vault_test_api::bind_vault_slot(
        &server,
        "EXTRACT_API_KEY",
        "deepseek-secret-bytes",
        false,
        "",
        None,
        Some("owner-bot"),
    )
    .expect("unrestricted slot bind");

    let pools = crate::vault_ops::load_unlocked_api_key_secret_pools(&server).expect("pools");
    assert!(
        !pools.contains_key("EXTRACT_API_KEY"),
        "unrestricted slot must not leak a restricted account: {:?}",
        pools.keys().collect::<Vec<_>>()
    );
    let leased = handle_vault_lease_api_key(
        &server,
        VaultLeaseApiKeyParams {
            name: "EXTRACT_API_KEY".to_string(),
            env_name: None,
            agent_id: None,
        },
    )
    .await
    .expect_err("restricted target must not lease through the slot");
    assert!(
        leased.contains("No usable API key") || leased.contains("not"),
        "{leased}"
    );

    crate::vault_test_api::owner_set_vault_secret(
        &server,
        "SILICONFLOW_API_KEY",
        "siliconflow-secret-bytes",
        None,
        "",
        false,
    )
    .await
    .expect("healthy account");
    crate::vault_test_api::bind_vault_slot(
        &server,
        "SUMMARY_API_KEY",
        "siliconflow-secret-bytes",
        false,
        "",
        None,
        None,
    )
    .expect("summary bind");
    server
        .with_global_store(|store| {
            store
                .vault_upsert_key_health(&memcore::vault::VaultKeyHealth {
                    logical_name: "SUMMARY_API_KEY".to_string(),
                    key_id: "SILICONFLOW_API_KEY".to_string(),
                    status: "disabled".to_string(),
                    disabled: true,
                    updated_at: chrono::Utc::now().to_rfc3339(),
                    ..Default::default()
                })
                .map_err(|e| e.to_string())
        })
        .expect("persist slot-target health");
    let pools = crate::vault_ops::load_unlocked_api_key_secret_pools(&server).expect("pools");
    assert!(
        !pools.contains_key("SUMMARY_API_KEY"),
        "disabled (slot, target) health must skip materialization: {:?}",
        pools.keys().collect::<Vec<_>>()
    );

    server
        .with_global_store(|store| {
            store
                .vault_upsert_key_health(&memcore::vault::VaultKeyHealth {
                    logical_name: "SUMMARY_API_KEY".to_string(),
                    key_id: "SILICONFLOW_API_KEY".to_string(),
                    status: "ok".to_string(),
                    disabled: false,
                    updated_at: chrono::Utc::now().to_rfc3339(),
                    ..Default::default()
                })
                .map_err(|e| e.to_string())?;
            let mut entry = store
                .vault_get_entry("SILICONFLOW_API_KEY")
                .map_err(|e| e.to_string())?
                .expect("account");
            entry.secret_type = memcore::SECRET_TYPE_CONFIG.to_string();
            store.vault_upsert_entry(&entry).map_err(|e| e.to_string())
        })
        .expect("healthy but non-api_key target");
    let pools = crate::vault_ops::load_unlocked_api_key_secret_pools(&server).expect("pools");
    assert!(
        !pools.contains_key("SUMMARY_API_KEY"),
        "non-api_key target must not materialize through the slot: {:?}",
        pools.keys().collect::<Vec<_>>()
    );
}

#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn leftover_slot_ciphertext_is_not_materialized() {
    let _lock = crate::utils::global_test_lock()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let db_path = crate::utils::test_fixture_path(format!(
        "memory-server-vault-slot-leftover-{}.sqlite",
        uuid::Uuid::new_v4()
    ));
    let server = MemoryServer::new(db_path, None).expect("create test server");
    crate::vault_test_api::seed_vault(&server, "slot-leftover").expect("seed unlocked vault");
    with_vault_key(&server, |key| {
        let (encrypted_value, nonce) = crate::vault_crypto::encrypt(key, b"leftover-slot-bytes")?;
        let now = chrono::Utc::now().to_rfc3339();
        server.with_global_store(|store| {
            store
                .vault_upsert_entry(&memcore::vault::VaultEntry {
                    name: "EXTRACT_API_KEY".to_string(),
                    encrypted_value,
                    nonce,
                    secret_type: memcore::SECRET_TYPE_API_KEY.to_string(),
                    description: String::new(),
                    allowed_agents: None,
                    created_at: now.clone(),
                    updated_at: now,
                    accessed_at: String::new(),
                    access_count: 0,
                })
                .map_err(|e| e.to_string())
        })
    })
    .expect("seed leftover ciphertext");
    let pools = crate::vault_ops::load_unlocked_api_key_secret_pools(&server).expect("pools");
    assert!(
        !pools.contains_key("EXTRACT_API_KEY"),
        "leftover slot ciphertext must not enter the API-key pool: {:?}",
        pools.keys().collect::<Vec<_>>()
    );
}

#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn lane_slot_cannot_be_a_rotation_pool() {
    let _lock = crate::utils::global_test_lock()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let db_path = crate::utils::test_fixture_path(format!(
        "memory-server-vault-slot-pool-{}.sqlite",
        uuid::Uuid::new_v4()
    ));
    let server = MemoryServer::new(db_path, None).expect("create test server");
    crate::vault_test_api::seed_vault(&server, "slot-pool").expect("seed unlocked vault");
    let pool_err = crate::vault_test_api::owner_set_vault_pool(
        &server,
        "EXTRACT_API_KEY",
        &["k1".to_string(), "k2".to_string()],
        "round_robin",
    )
    .await
    .expect_err("slot prefix must not become a pool");
    assert!(
        pool_err.contains("Lane slot") || pool_err.contains("rotation pool"),
        "{pool_err}"
    );
    let rotation_err = handle_vault_setup_rotation(
        &server,
        VaultSetupRotationParams {
            prefix: "DISTILL_API_KEY".to_string(),
            total_keys: 2,
            agent_id: None,
            strategy: "round_robin".to_string(),
        },
    )
    .await
    .expect_err("slot prefix must not attach rotation");
    assert!(
        rotation_err.contains("Lane slot") || rotation_err.contains("rotation pool"),
        "{rotation_err}"
    );
}

#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn child_env_all_mode_does_not_inject_leftover_slot_bytes() {
    let _lock = crate::utils::global_test_lock()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let _child_env = EnvRestore::set("TACHI_VAULT_CHILD_ENV", "all");
    let db_path = crate::utils::test_fixture_path(format!(
        "memory-server-vault-slot-child-env-{}.sqlite",
        uuid::Uuid::new_v4()
    ));
    let server = MemoryServer::new(db_path, None).expect("create test server");
    crate::vault_test_api::seed_vault(&server, "slot-child-env").expect("seed unlocked vault");
    with_vault_key(&server, |key| {
        let (encrypted_value, nonce) = crate::vault_crypto::encrypt(key, b"leftover-slot-bytes")?;
        let now = chrono::Utc::now().to_rfc3339();
        server.with_global_store(|store| {
            store
                .vault_upsert_entry(&memcore::vault::VaultEntry {
                    name: "EXTRACT_API_KEY".to_string(),
                    encrypted_value,
                    nonce,
                    secret_type: memcore::SECRET_TYPE_API_KEY.to_string(),
                    description: String::new(),
                    allowed_agents: None,
                    created_at: now.clone(),
                    updated_at: now,
                    accessed_at: String::new(),
                    access_count: 0,
                })
                .map_err(|e| e.to_string())
        })
    })
    .expect("seed leftover ciphertext");
    let secrets = crate::vault_ops::load_unlocked_env_secrets_for_child_env(&server, None)
        .expect("child env");
    assert!(
        secrets
            .iter()
            .all(|(name, value)| name != "EXTRACT_API_KEY" && value != "leftover-slot-bytes"),
        "{secrets:?}"
    );

    crate::vault_test_api::owner_set_vault_secret(
        &server,
        "DEEPSEEK_API_KEY",
        "deepseek-secret-bytes",
        None,
        "",
        false,
    )
    .await
    .expect("account");
    crate::vault_test_api::bind_vault_slot(
        &server,
        "EXTRACT_API_KEY",
        "deepseek-secret-bytes",
        true,
        "",
        None,
        None,
    )
    .expect("bind leftover to account");
    let secrets = crate::vault_ops::load_unlocked_env_secrets_for_child_env(&server, None)
        .expect("child env");
    let extract = secrets
        .iter()
        .find(|(name, _)| name == "EXTRACT_API_KEY")
        .expect("bound slot must enter child env through the resolved pool");
    assert_eq!(extract.1, "deepseek-secret-bytes");
    assert_ne!(extract.1, "vault:DEEPSEEK_API_KEY");
}

#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn project_binding_resolves_lane_slot_pointer() {
    let _lock = crate::utils::global_test_lock()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let dir = tempfile::tempdir().expect("tempdir");
    let db_path = crate::utils::test_fixture_path(format!(
        "memory-server-vault-slot-project-bind-{}.sqlite",
        uuid::Uuid::new_v4()
    ));
    let server = MemoryServer::new(db_path, None).expect("create test server");
    crate::vault_test_api::seed_vault(&server, "slot-project-bind").expect("seed unlocked vault");
    crate::vault_test_api::owner_set_vault_secret(
        &server,
        "DEEPSEEK_API_KEY",
        "deepseek-secret-bytes",
        None,
        "",
        false,
    )
    .await
    .expect("account");
    crate::vault_test_api::bind_vault_slot(
        &server,
        "EXTRACT_API_KEY",
        "deepseek-secret-bytes",
        false,
        "",
        None,
        None,
    )
    .expect("bind");
    let tachi_dir = dir.path().join(".tachi");
    std::fs::create_dir_all(&tachi_dir).expect("mkdir");
    std::fs::write(
        tachi_dir.join("vault.env"),
        "MY_KEY=vault:EXTRACT_API_KEY\n",
    )
    .expect("write bindings");
    let secrets =
        crate::vault_ops::load_unlocked_env_secrets_for_child_env(&server, Some(dir.path()))
            .expect("child env");
    let injected = secrets
        .iter()
        .find(|(name, _)| name == "MY_KEY")
        .expect("project binding");
    assert_eq!(injected.1, "deepseek-secret-bytes");
    assert_ne!(injected.1, "vault:DEEPSEEK_API_KEY");
    assert_ne!(injected.1, "vault:EXTRACT_API_KEY");
}

#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn read_unlocked_vault_secret_resolves_lane_slot_pointer() {
    let _lock = crate::utils::global_test_lock()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let db_path = crate::utils::test_fixture_path(format!(
        "memory-server-vault-slot-usable-read-{}.sqlite",
        uuid::Uuid::new_v4()
    ));
    let server = MemoryServer::new(db_path, None).expect("create test server");
    crate::vault_test_api::seed_vault(&server, "slot-usable-read").expect("seed unlocked vault");
    crate::vault_test_api::owner_set_vault_secret(
        &server,
        "DEEPSEEK_API_KEY",
        "deepseek-secret-bytes",
        None,
        "",
        false,
    )
    .await
    .expect("account");
    crate::vault_test_api::bind_vault_slot(
        &server,
        "EXTRACT_API_KEY",
        "deepseek-secret-bytes",
        false,
        "",
        None,
        None,
    )
    .expect("bind");
    let usable =
        crate::vault_ops::read_unlocked_vault_secret(&server, "EXTRACT_API_KEY", None, false)
            .expect("usable slot");
    assert_eq!(usable, "deepseek-secret-bytes");
    let got = crate::vault_ops::read_unlocked_vault_secret(&server, "EXTRACT_API_KEY", None, false)
        .expect("inventory get");
    assert_eq!(got, "deepseek-secret-bytes");
    assert_eq!(
        fixture_vault_plaintext(&server, "EXTRACT_API_KEY"),
        "vault:DEEPSEEK_API_KEY"
    );

    server
        .with_global_store(|store| {
            store
                .vault_upsert_key_health(&memcore::vault::VaultKeyHealth {
                    logical_name: "DEEPSEEK_API_KEY".to_string(),
                    key_id: "DEEPSEEK_API_KEY".to_string(),
                    status: "disabled".to_string(),
                    disabled: true,
                    updated_at: chrono::Utc::now().to_rfc3339(),
                    ..Default::default()
                })
                .map_err(|e| e.to_string())
        })
        .expect("disable target");
    let err = crate::vault_ops::read_unlocked_vault_secret(&server, "EXTRACT_API_KEY", None, false)
        .expect_err("disabled target must not be a usable slot secret");
    assert!(
        err.contains("unusable") || err.contains("Lane slot"),
        "{err}"
    );
}

#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn lane_slot_read_ignores_legacy_rotation_members() {
    let _lock = crate::utils::global_test_lock()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let db_path = crate::utils::test_fixture_path(format!(
        "memory-server-vault-slot-legacy-rotation-{}.sqlite",
        uuid::Uuid::new_v4()
    ));
    let server = MemoryServer::new(db_path, None).expect("create test server");
    crate::vault_test_api::seed_vault(&server, "slot-legacy-rotation")
        .expect("seed unlocked vault");
    crate::vault_test_api::owner_set_vault_secret(
        &server,
        "DEEPSEEK_API_KEY",
        "deepseek-secret-bytes",
        None,
        "",
        false,
    )
    .await
    .expect("account");
    crate::vault_test_api::bind_vault_slot(
        &server,
        "EXTRACT_API_KEY",
        "deepseek-secret-bytes",
        false,
        "",
        None,
        None,
    )
    .expect("bind");
    with_vault_key(&server, |key| {
        let (encrypted_value, nonce) =
            crate::vault_crypto::encrypt(key, b"leftover-rotation-member")?;
        let now = chrono::Utc::now().to_rfc3339();
        server.with_global_store(|store| {
            store
                .vault_upsert_entry(&memcore::vault::VaultEntry {
                    name: "EXTRACT_API_KEY_1".to_string(),
                    encrypted_value,
                    nonce,
                    secret_type: memcore::SECRET_TYPE_API_KEY.to_string(),
                    description: String::new(),
                    allowed_agents: None,
                    created_at: now.clone(),
                    updated_at: now.clone(),
                    accessed_at: String::new(),
                    access_count: 0,
                })
                .map_err(|e| e.to_string())?;
            store
                .vault_set_rotation(&memcore::vault::VaultKeyRotation {
                    prefix: "EXTRACT_API_KEY".to_string(),
                    current_index: 1,
                    total_keys: 1,
                    rotation_strategy: "round_robin".to_string(),
                    created_at: now.clone(),
                    updated_at: now,
                })
                .map_err(|e| e.to_string())
        })
    })
    .expect("seed leftover slot rotation");
    let usable =
        crate::vault_ops::read_unlocked_vault_secret(&server, "EXTRACT_API_KEY", None, true)
            .expect("slot must ignore leftover rotation");
    assert_eq!(usable, "deepseek-secret-bytes");
    assert_ne!(usable, "leftover-rotation-member");
}
