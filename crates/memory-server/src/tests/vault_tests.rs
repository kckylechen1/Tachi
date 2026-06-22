use super::make_server;
use crate::server_state::CachedVaultKey;
use crate::vault_crypto;
use crate::vault_ops::{
    VaultGetParams, VaultInitParams, VaultLeaseApiKeyParams, VaultListParams,
    VaultRecordKeyResultParams, VaultRemoveParams, VaultSetApiKeyPoolParams, VaultSetParams,
    VaultSetupRotationParams, VaultUnlockParams,
};
use rmcp::handler::server::wrapper::Parameters;
use serde_json::{json, Value};
use std::time::{Duration, Instant};

#[test]
fn cached_vault_key_copies_source_buffer() {
    let mut source = [7u8; 32];
    let cached = CachedVaultKey::copy_from(&source);
    vault_crypto::zero_key(&mut source);

    assert_eq!(source, [0u8; 32]);
    assert_eq!(cached.bytes(), &[7u8; 32]);
}

#[tokio::test]
async fn vault_init_set_get_lock_unlock_roundtrip() {
    let server = make_server();

    let init = server
        .vault_init(Parameters(VaultInitParams {
            password: "correct horse battery staple".to_string(),
        }))
        .await
        .expect("vault_init should succeed");
    let init_json: serde_json::Value =
        serde_json::from_str(&init).expect("vault_init response should be JSON");
    assert_eq!(init_json["initialized"], json!(true));

    server
        .vault_set(Parameters(VaultSetParams {
            name: "OPENAI_API_KEY".to_string(),
            value: "sk-test-123".to_string(),
            secret_type: "api_key".to_string(),
            description: "primary openai key".to_string(),
            allowed_agents: None,
            enable_rotation: false,
            rotation_strategy: None,
        }))
        .await
        .expect("vault_set should succeed");
    assert_eq!(
        server
            .llm
            .provider_secret_for_tests(&["OPENAI_API_KEY"])
            .as_deref(),
        Some("sk-test-123"),
        "vault_set should make provider keys available to the LLM client"
    );

    let get = server
        .vault_get(Parameters(VaultGetParams {
            name: "OPENAI_API_KEY".to_string(),
            agent_id: None,
            auto_rotate: false,
        }))
        .await
        .expect("vault_get should succeed");
    let get_json: serde_json::Value =
        serde_json::from_str(&get).expect("vault_get response should be JSON");
    assert_eq!(get_json["value"], json!("sk-test-123"));

    server
        .vault_lock()
        .await
        .expect("vault_lock should succeed");
    assert!(
        server
            .llm
            .provider_secret_for_tests(&["OPENAI_API_KEY"])
            .is_none(),
        "vault_lock should clear provider keys cached from the vault"
    );
    let locked_get = server
        .vault_get(Parameters(VaultGetParams {
            name: "OPENAI_API_KEY".to_string(),
            agent_id: None,
            auto_rotate: false,
        }))
        .await;
    assert!(locked_get.is_err(), "vault_get should fail while locked");

    let listed = server
        .vault_list(Parameters(VaultListParams { secret_type: None }))
        .await
        .expect("vault_list should succeed while locked");
    let listed_json: serde_json::Value =
        serde_json::from_str(&listed).expect("vault_list response should be JSON");
    assert_eq!(listed_json["count"], json!(1));
    assert_eq!(listed_json["secrets"][0]["name"], json!("OPENAI_API_KEY"));

    server
        .vault_unlock(Parameters(VaultUnlockParams {
            password: "correct horse battery staple".to_string(),
            password_fifo_path: None,
        }))
        .await
        .expect("vault_unlock should succeed");
    assert_eq!(
        server
            .llm
            .provider_secret_for_tests(&["OPENAI_API_KEY"])
            .as_deref(),
        Some("sk-test-123"),
        "vault_unlock should reload provider keys into the LLM client"
    );

    let status = server
        .vault_status()
        .await
        .expect("vault_status should succeed");
    let status_json: serde_json::Value =
        serde_json::from_str(&status).expect("vault_status response should be JSON");
    assert_eq!(status_json["initialized"], json!(true));
    assert_eq!(status_json["locked"], json!(false));
    assert_eq!(status_json["entry_count"], json!(1));
}

#[tokio::test]
async fn vault_provider_cache_skips_agent_scoped_api_keys() {
    std::env::remove_var("LOCKED_DOWN_API_KEY");
    let server = make_server();

    server
        .vault_init(Parameters(VaultInitParams {
            password: "correct horse battery staple".to_string(),
        }))
        .await
        .expect("vault_init should succeed");

    server
        .vault_set(Parameters(VaultSetParams {
            name: "LOCKED_DOWN_API_KEY".to_string(),
            value: "scoped-secret".to_string(),
            secret_type: "api_key".to_string(),
            description: "capability-scoped key".to_string(),
            allowed_agents: Some(vec!["mcp:allowed".to_string()]),
            enable_rotation: false,
            rotation_strategy: None,
        }))
        .await
        .expect("vault_set should succeed");

    assert!(
        server
            .llm
            .provider_secret_for_tests(&["LOCKED_DOWN_API_KEY"])
            .is_none(),
        "agent-scoped provider keys must not enter the server-wide LLM cache"
    );

    let get = server
        .vault_get(Parameters(VaultGetParams {
            name: "LOCKED_DOWN_API_KEY".to_string(),
            agent_id: Some("mcp:allowed".to_string()),
            auto_rotate: false,
        }))
        .await
        .expect("allowed agent should still be able to read the scoped key");
    let get_json: serde_json::Value = serde_json::from_str(&get).expect("vault_get JSON");
    assert_eq!(get_json["value"], json!("scoped-secret"));
}

#[tokio::test]
async fn vault_rotation_prefix_get_round_robin_works() {
    let server = make_server();

    server
        .vault_init(Parameters(VaultInitParams {
            password: "hunter2-hunter2".to_string(),
        }))
        .await
        .expect("vault_init should succeed");

    for (name, value) in [
        ("GEMINI_API_KEY_1", "gemini-key-1"),
        ("GEMINI_API_KEY_2", "gemini-key-2"),
    ] {
        server
            .vault_set(Parameters(VaultSetParams {
                name: name.to_string(),
                value: value.to_string(),
                secret_type: "api_key".to_string(),
                description: "rotated key".to_string(),
                allowed_agents: None,
                enable_rotation: false,
                rotation_strategy: None,
            }))
            .await
            .expect("vault_set rotation key should succeed");
    }

    server
        .vault_setup_rotation(Parameters(VaultSetupRotationParams {
            prefix: "GEMINI_API_KEY".to_string(),
            total_keys: 2,
            strategy: "round_robin".to_string(),
        }))
        .await
        .expect("vault_setup_rotation should succeed");

    let first = server
        .vault_get(Parameters(VaultGetParams {
            name: "GEMINI_API_KEY".to_string(),
            agent_id: None,
            auto_rotate: false,
        }))
        .await
        .expect("first rotated vault_get should succeed");
    let first_json: serde_json::Value =
        serde_json::from_str(&first).expect("first vault_get response should be JSON");

    let second = server
        .vault_get(Parameters(VaultGetParams {
            name: "GEMINI_API_KEY".to_string(),
            agent_id: None,
            auto_rotate: false,
        }))
        .await
        .expect("second rotated vault_get should succeed");
    let second_json: serde_json::Value =
        serde_json::from_str(&second).expect("second vault_get response should be JSON");

    assert_eq!(first_json["name"], json!("GEMINI_API_KEY_1"));
    assert_eq!(first_json["value"], json!("gemini-key-1"));
    assert_eq!(second_json["name"], json!("GEMINI_API_KEY_2"));
    assert_eq!(second_json["value"], json!("gemini-key-2"));
}

#[tokio::test]
async fn vault_rotation_materializes_provider_pool_under_logical_key() {
    let server = make_server();

    server
        .vault_init(Parameters(VaultInitParams {
            password: "hunter2-hunter2".to_string(),
        }))
        .await
        .expect("vault_init should succeed");

    for (name, value) in [
        ("VOYAGE_API_KEY_1", "voyage-key-1"),
        ("VOYAGE_API_KEY_2", "voyage-key-2"),
    ] {
        server
            .vault_set(Parameters(VaultSetParams {
                name: name.to_string(),
                value: value.to_string(),
                secret_type: "api_key".to_string(),
                description: "rotated voyage key".to_string(),
                allowed_agents: None,
                enable_rotation: false,
                rotation_strategy: None,
            }))
            .await
            .expect("vault_set rotation key should succeed");
    }

    server
        .vault_setup_rotation(Parameters(VaultSetupRotationParams {
            prefix: "VOYAGE_API_KEY".to_string(),
            total_keys: 2,
            strategy: "round_robin".to_string(),
        }))
        .await
        .expect("vault_setup_rotation should refresh provider pool");

    assert_eq!(
        server
            .llm
            .provider_key_id_for_tests(&["VOYAGE_API_KEY"])
            .as_deref(),
        Some("VOYAGE_API_KEY_1")
    );
    assert_eq!(
        server
            .llm
            .provider_key_id_for_tests(&["VOYAGE_API_KEY"])
            .as_deref(),
        Some("VOYAGE_API_KEY_2"),
        "provider pools should rotate in-process between healthy concrete keys"
    );

    server
        .llm
        .mark_provider_key_rate_limited_for_tests("VOYAGE_API_KEY_1", Some(60));

    assert_eq!(
        server
            .llm
            .provider_key_id_for_tests(&["VOYAGE_API_KEY"])
            .as_deref(),
        Some("VOYAGE_API_KEY_2"),
        "rate-limited concrete keys should be skipped inside the same logical pool"
    );
}

#[tokio::test]
async fn vault_get_auto_rotate_does_not_advance_rotation_on_decrypt_failure() {
    let server = make_server();

    server
        .vault_init(Parameters(VaultInitParams {
            password: "rotation-failure-password".to_string(),
        }))
        .await
        .expect("vault_init should succeed");

    for (name, value) in [
        ("BROKEN_API_KEY_1", "broken-key-1"),
        ("BROKEN_API_KEY_2", "broken-key-2"),
    ] {
        server
            .vault_set(Parameters(VaultSetParams {
                name: name.to_string(),
                value: value.to_string(),
                secret_type: "api_key".to_string(),
                description: "rotated key".to_string(),
                allowed_agents: None,
                enable_rotation: false,
                rotation_strategy: None,
            }))
            .await
            .expect("vault_set rotation key should succeed");
    }

    server
        .vault_setup_rotation(Parameters(VaultSetupRotationParams {
            prefix: "BROKEN_API_KEY".to_string(),
            total_keys: 2,
            strategy: "round_robin".to_string(),
        }))
        .await
        .expect("vault_setup_rotation should succeed");

    server
        .with_global_store(|store| {
            let mut entry = store
                .vault_get_entry("BROKEN_API_KEY_1")
                .map_err(|e| e.to_string())?
                .expect("rotation member should exist");
            entry.encrypted_value = "not valid base64".to_string();
            store.vault_upsert_entry(&entry).map_err(|e| e.to_string())
        })
        .expect("corrupt rotation member");

    let err = server
        .vault_get(Parameters(VaultGetParams {
            name: "BROKEN_API_KEY".to_string(),
            agent_id: None,
            auto_rotate: true,
        }))
        .await
        .expect_err("corrupted selected key should fail before rotation advances");
    assert!(err.contains("Bad ciphertext base64"), "{err}");

    let current_index = server
        .with_global_store_read(|store| {
            store
                .vault_get_rotation("BROKEN_API_KEY")
                .map_err(|e| e.to_string())
                .map(|rotation| rotation.expect("rotation should exist").current_index)
        })
        .expect("read rotation");
    assert_eq!(
        current_index, 1,
        "failed decrypt must not skip the selected rotation member"
    );
}

#[tokio::test]
async fn vault_api_key_pool_sets_and_leases_rotated_env() {
    let server = make_server();

    server
        .vault_init(Parameters(VaultInitParams {
            password: "pool-password".to_string(),
        }))
        .await
        .expect("vault_init should succeed");

    let stored = server
        .vault_set_api_key_pool(Parameters(VaultSetApiKeyPoolParams {
            prefix: "ROUTER_API_KEY".to_string(),
            values: vec!["router-key-1".to_string(), "router-key-2".to_string()],
            strategy: "round_robin".to_string(),
            description: "router pool".to_string(),
            allowed_agents: None,
        }))
        .await
        .expect("vault_set_api_key_pool should succeed");
    let stored_json: serde_json::Value = serde_json::from_str(&stored).expect("stored JSON");
    assert_eq!(stored_json["logical_name"], json!("ROUTER_API_KEY"));
    assert_eq!(stored_json["total_keys"], json!(2));

    let first = server
        .vault_lease_api_key(Parameters(VaultLeaseApiKeyParams {
            name: "ROUTER_API_KEY".to_string(),
            env_name: None,
            agent_id: None,
        }))
        .await
        .expect("first lease should succeed");
    let first_json: serde_json::Value = serde_json::from_str(&first).expect("first lease JSON");
    assert_eq!(first_json["key_id"], json!("ROUTER_API_KEY_1"));
    assert_eq!(first_json["env"]["ROUTER_API_KEY"], json!("router-key-1"));

    let second = server
        .vault_lease_api_key(Parameters(VaultLeaseApiKeyParams {
            name: "ROUTER_API_KEY".to_string(),
            env_name: Some("OPENAI_API_KEY".to_string()),
            agent_id: None,
        }))
        .await
        .expect("second lease should succeed");
    let second_json: serde_json::Value = serde_json::from_str(&second).expect("second lease JSON");
    assert_eq!(second_json["key_id"], json!("ROUTER_API_KEY_2"));
    assert_eq!(second_json["env_name"], json!("OPENAI_API_KEY"));
    assert_eq!(second_json["env"]["OPENAI_API_KEY"], json!("router-key-2"));

    let lease_audit_details = server
        .with_global_store_read(|store| {
            let mut stmt = store
                .connection()
                .prepare(
                    "SELECT detail
                     FROM vault_audit
                     WHERE operation = 'vault_lease_api_key'
                     ORDER BY id ASC",
                )
                .map_err(|e| format!("prepare lease audit query failed: {e}"))?;
            let rows = stmt
                .query_map([], |row| row.get::<_, Option<String>>(0))
                .map_err(|e| format!("query lease audit rows failed: {e}"))?;
            let mut details = Vec::new();
            for row in rows {
                details.push(row.map_err(|e| format!("read lease audit row failed: {e}"))?);
            }
            Ok(details)
        })
        .expect("lease audit query should succeed");
    assert_eq!(lease_audit_details.len(), 2);
    let detail = lease_audit_details[1]
        .as_deref()
        .expect("successful lease audit detail");
    let detail_json: serde_json::Value = serde_json::from_str(detail).expect("audit detail JSON");
    assert_eq!(detail_json["logical_name"], json!("ROUTER_API_KEY"));
    assert_eq!(detail_json["key_id"], json!("ROUTER_API_KEY_2"));
    assert_eq!(detail_json["env_name"], json!("OPENAI_API_KEY"));
    assert!(
        !detail.contains("router-key-1") && !detail.contains("router-key-2"),
        "lease audit detail must not include raw secret values: {detail}"
    );
}

#[tokio::test]
async fn vault_api_key_lease_does_not_decrypt_unrelated_provider_secrets() {
    let server = make_server();

    server
        .vault_init(Parameters(VaultInitParams {
            password: "pool-scope-password".to_string(),
        }))
        .await
        .expect("vault_init should succeed");

    server
        .vault_set_api_key_pool(Parameters(VaultSetApiKeyPoolParams {
            prefix: "ROUTER_API_KEY".to_string(),
            values: vec!["router-key-1".to_string()],
            strategy: "round_robin".to_string(),
            description: "router pool".to_string(),
            allowed_agents: None,
        }))
        .await
        .expect("vault_set_api_key_pool should succeed");

    server
        .vault_set(Parameters(VaultSetParams {
            name: "UNRELATED_API_KEY".to_string(),
            value: "unrelated-secret".to_string(),
            secret_type: "api_key".to_string(),
            description: "unrelated provider key".to_string(),
            allowed_agents: None,
            enable_rotation: false,
            rotation_strategy: None,
        }))
        .await
        .expect("vault_set unrelated key should succeed");
    server
        .with_global_store(|store| {
            let mut entry = store
                .vault_get_entry("UNRELATED_API_KEY")
                .map_err(|e| e.to_string())?
                .expect("unrelated provider key should exist");
            entry.encrypted_value = "not valid base64".to_string();
            store.vault_upsert_entry(&entry).map_err(|e| e.to_string())
        })
        .expect("corrupt unrelated provider key");

    let leased = server
        .vault_lease_api_key(Parameters(VaultLeaseApiKeyParams {
            name: "ROUTER_API_KEY".to_string(),
            env_name: None,
            agent_id: None,
        }))
        .await
        .expect("lease should ignore unrelated provider key ciphertext");
    let leased_json: serde_json::Value = serde_json::from_str(&leased).expect("lease JSON");
    assert_eq!(leased_json["key_id"], json!("ROUTER_API_KEY_1"));
    assert_eq!(leased_json["env"]["ROUTER_API_KEY"], json!("router-key-1"));
}

#[tokio::test]
async fn vault_api_key_pool_shrink_removes_orphaned_members() {
    let server = make_server();

    server
        .vault_init(Parameters(VaultInitParams {
            password: "pool-shrink-password".to_string(),
        }))
        .await
        .expect("vault_init should succeed");

    server
        .vault_set_api_key_pool(Parameters(VaultSetApiKeyPoolParams {
            prefix: "SHRINK_API_KEY".to_string(),
            values: vec![
                "shrink-key-1".to_string(),
                "shrink-key-2".to_string(),
                "shrink-key-3".to_string(),
            ],
            strategy: "round_robin".to_string(),
            description: "shrink pool".to_string(),
            allowed_agents: None,
        }))
        .await
        .expect("initial pool set should succeed");

    let shrunk = server
        .vault_set_api_key_pool(Parameters(VaultSetApiKeyPoolParams {
            prefix: "SHRINK_API_KEY".to_string(),
            values: vec!["shrink-key-1b".to_string()],
            strategy: "round_robin".to_string(),
            description: "shrunk pool".to_string(),
            allowed_agents: None,
        }))
        .await
        .expect("shrinking pool should succeed");
    let shrunk_json: serde_json::Value = serde_json::from_str(&shrunk).expect("shrunk JSON");
    assert_eq!(shrunk_json["members"], json!(["SHRINK_API_KEY_1"]));
    assert_eq!(
        shrunk_json["removed_members"],
        json!(["SHRINK_API_KEY_2", "SHRINK_API_KEY_3"])
    );

    let listed = server
        .vault_list(Parameters(VaultListParams {
            secret_type: Some("api_key".to_string()),
        }))
        .await
        .expect("vault list should succeed");
    let listed_json: serde_json::Value = serde_json::from_str(&listed).expect("list JSON");
    let names = listed_json["secrets"]
        .as_array()
        .expect("secrets array")
        .iter()
        .filter_map(|entry| entry["name"].as_str())
        .collect::<Vec<_>>();
    assert!(names.contains(&"SHRINK_API_KEY_1"), "{names:?}");
    assert!(!names.contains(&"SHRINK_API_KEY_2"), "{names:?}");
    assert!(!names.contains(&"SHRINK_API_KEY_3"), "{names:?}");

    let leased = server
        .vault_lease_api_key(Parameters(VaultLeaseApiKeyParams {
            name: "SHRINK_API_KEY".to_string(),
            env_name: None,
            agent_id: None,
        }))
        .await
        .expect("lease should use remaining key");
    let leased_json: serde_json::Value = serde_json::from_str(&leased).expect("lease JSON");
    assert_eq!(leased_json["key_id"], json!("SHRINK_API_KEY_1"));
    assert_eq!(leased_json["env"]["SHRINK_API_KEY"], json!("shrink-key-1b"));
}

#[tokio::test]
async fn vault_api_key_lease_skips_unusable_health_members() {
    let server = make_server();

    server
        .vault_init(Parameters(VaultInitParams {
            password: "pool-health-password".to_string(),
        }))
        .await
        .expect("vault_init should succeed");
    server
        .vault_set_api_key_pool(Parameters(VaultSetApiKeyPoolParams {
            prefix: "TRANSIT_API_KEY".to_string(),
            values: vec!["transit-key-1".to_string(), "transit-key-2".to_string()],
            strategy: "round_robin".to_string(),
            description: "transit pool".to_string(),
            allowed_agents: None,
        }))
        .await
        .expect("vault_set_api_key_pool should succeed");

    server
        .llm
        .mark_provider_key_rate_limited_for_tests("TRANSIT_API_KEY_1", Some(60));

    let leased = server
        .vault_lease_api_key(Parameters(VaultLeaseApiKeyParams {
            name: "TRANSIT_API_KEY".to_string(),
            env_name: None,
            agent_id: None,
        }))
        .await
        .expect("lease should skip rate-limited first key");
    let leased_json: serde_json::Value = serde_json::from_str(&leased).expect("lease JSON");
    assert_eq!(leased_json["key_id"], json!("TRANSIT_API_KEY_2"));
    assert_eq!(
        leased_json["env"]["TRANSIT_API_KEY"],
        json!("transit-key-2")
    );
}

#[tokio::test]
async fn vault_record_key_result_updates_health_and_lease_selection() {
    let server = make_server();

    server
        .vault_init(Parameters(VaultInitParams {
            password: "record-health-password".to_string(),
        }))
        .await
        .expect("vault_init should succeed");
    server
        .vault_set_api_key_pool(Parameters(VaultSetApiKeyPoolParams {
            prefix: "BROKER_API_KEY".to_string(),
            values: vec!["broker-key-1".to_string(), "broker-key-2".to_string()],
            strategy: "round_robin".to_string(),
            description: "broker pool".to_string(),
            allowed_agents: None,
        }))
        .await
        .expect("vault_set_api_key_pool should succeed");

    let rate_limited = server
        .vault_record_key_result(Parameters(VaultRecordKeyResultParams {
            logical_name: "BROKER_API_KEY".to_string(),
            key_id: "BROKER_API_KEY_1".to_string(),
            status_code: Some(429),
            outcome: None,
            retry_after_secs: Some(120),
            reason: Some("provider 429".to_string()),
        }))
        .await
        .expect("record 429 should succeed");
    let rate_limited_json: serde_json::Value =
        serde_json::from_str(&rate_limited).expect("record JSON");
    assert_eq!(rate_limited_json["health"]["status"], json!("rate_limited"));
    assert!(rate_limited_json["health"].get("last_error").is_none());
    assert!(rate_limited_json["health"].get("metadata").is_none());
    assert!(rate_limited_json["health"].get("updated_at").is_none());
    assert_eq!(rate_limited_json["skipped_by_lease"], json!(true));

    let leased = server
        .vault_lease_api_key(Parameters(VaultLeaseApiKeyParams {
            name: "BROKER_API_KEY".to_string(),
            env_name: None,
            agent_id: None,
        }))
        .await
        .expect("lease should skip 429 key");
    let leased_json: serde_json::Value = serde_json::from_str(&leased).expect("lease JSON");
    assert_eq!(leased_json["key_id"], json!("BROKER_API_KEY_2"));

    let auth_failed = server
        .vault_record_key_result(Parameters(VaultRecordKeyResultParams {
            logical_name: "BROKER_API_KEY".to_string(),
            key_id: "BROKER_API_KEY_2".to_string(),
            status_code: Some(401),
            outcome: None,
            retry_after_secs: None,
            reason: Some("provider auth failed".to_string()),
        }))
        .await
        .expect("record 401 should succeed");
    let auth_failed_json: serde_json::Value =
        serde_json::from_str(&auth_failed).expect("auth record JSON");
    assert_eq!(auth_failed_json["health"]["auth_failed"], json!(true));
    assert!(auth_failed_json["health"].get("last_error").is_none());

    let no_key = server
        .vault_lease_api_key(Parameters(VaultLeaseApiKeyParams {
            name: "BROKER_API_KEY".to_string(),
            env_name: None,
            agent_id: None,
        }))
        .await
        .expect_err("all unhealthy keys should fail lease");
    assert!(no_key.contains("No usable API key"), "{no_key}");

    let success = server
        .vault_record_key_result(Parameters(VaultRecordKeyResultParams {
            logical_name: "BROKER_API_KEY".to_string(),
            key_id: "BROKER_API_KEY_2".to_string(),
            status_code: Some(200),
            outcome: None,
            retry_after_secs: None,
            reason: None,
        }))
        .await
        .expect("record success should succeed");
    let success_json: serde_json::Value = serde_json::from_str(&success).expect("success JSON");
    assert_eq!(success_json["health"]["status"], json!("ok"));
    assert_eq!(success_json["health"]["auth_failed"], json!(false));
    assert!(success_json["health"].get("last_success").is_none());
}

#[tokio::test]
async fn dispatch_env_injection_uses_logical_rotation_key_not_member_names() {
    let server = make_server();

    server
        .vault_init(Parameters(VaultInitParams {
            password: "hunter2-hunter2".to_string(),
        }))
        .await
        .expect("vault_init should succeed");

    for (name, value) in [
        ("TAVILY_API_KEY_1", "tavily-key-1"),
        ("TAVILY_API_KEY_2", "tavily-key-2"),
    ] {
        server
            .vault_set(Parameters(VaultSetParams {
                name: name.to_string(),
                value: value.to_string(),
                secret_type: "api_key".to_string(),
                description: "rotated tavily key".to_string(),
                allowed_agents: None,
                enable_rotation: false,
                rotation_strategy: None,
            }))
            .await
            .expect("vault_set rotation key should succeed");
    }

    server
        .vault_setup_rotation(Parameters(VaultSetupRotationParams {
            prefix: "TAVILY_API_KEY".to_string(),
            total_keys: 2,
            strategy: "round_robin".to_string(),
        }))
        .await
        .expect("vault_setup_rotation should succeed");

    let mut cmd = tokio::process::Command::new("sh");
    cmd.arg("-c").arg(
        "printf '%s|%s|%s' \"$TAVILY_API_KEY\" \"${TAVILY_API_KEY_1-unset}\" \"${TAVILY_API_KEY_2-unset}\"",
    );
    let injected = crate::dispatch_ops::apply_unlocked_vault_env(&mut cmd, &server, None);
    assert!(injected >= 1);
    let output = cmd.output().await.expect("run env inspection command");
    assert!(output.status.success());
    let stdout = String::from_utf8(output.stdout).expect("stdout utf8");

    assert!(
        stdout == "tavily-key-1|unset|unset" || stdout == "tavily-key-2|unset|unset",
        "expected logical TAVILY_API_KEY only, got {stdout}"
    );
}

#[tokio::test]
async fn vault_auto_lock_expires_cached_key() {
    let server = make_server();
    // Use a longer timeout than the slowest CI step between init/set so the
    // setup itself does not race the auto-lock; the test then forces
    // expiration by rewinding `vault_unlock_time` below.
    server.vault_write().auto_lock_after_secs = 30;

    server
        .vault_init(Parameters(VaultInitParams {
            password: "auto-lock-password".to_string(),
        }))
        .await
        .expect("vault_init should succeed");

    server
        .vault_set(Parameters(VaultSetParams {
            name: "OPENAI_API_KEY".to_string(),
            value: "secret-value".to_string(),
            secret_type: "api_key".to_string(),
            description: "auto lock test secret".to_string(),
            allowed_agents: None,
            enable_rotation: false,
            rotation_strategy: None,
        }))
        .await
        .expect("vault_set should succeed");
    assert_eq!(
        server
            .llm
            .provider_secret_for_tests(&["OPENAI_API_KEY"])
            .as_deref(),
        Some("secret-value"),
        "vault_set should refresh provider cache before auto-lock"
    );

    server.vault_write().unlock_time = Some(Instant::now() - Duration::from_secs(60));

    let err = server
        .vault_get(Parameters(VaultGetParams {
            name: "OPENAI_API_KEY".to_string(),
            agent_id: None,
            auto_rotate: false,
        }))
        .await
        .expect_err("vault_get should fail after auto-lock timeout");
    assert!(
        err.contains("Vault auto-locked"),
        "expected auto-lock error, got: {err}"
    );

    let status = server
        .vault_status()
        .await
        .expect("vault_status should succeed");
    let status_json: serde_json::Value =
        serde_json::from_str(&status).expect("vault_status response should be JSON");
    assert_eq!(status_json["locked"], json!(true));
    assert!(
        server
            .llm
            .provider_secret_for_tests(&["OPENAI_API_KEY"])
            .is_none(),
        "auto-lock should clear provider keys cached from the vault"
    );
}

#[tokio::test]
async fn vault_unlock_enforces_bruteforce_lockout_and_resets_on_success() {
    let server = make_server();

    server
        .vault_init(Parameters(VaultInitParams {
            password: "correct-password".to_string(),
        }))
        .await
        .expect("vault_init should succeed");
    server
        .vault_lock()
        .await
        .expect("vault_lock should succeed");

    for attempt in 1..5 {
        let err = server
            .vault_unlock(Parameters(VaultUnlockParams {
                password: format!("wrong-password-{attempt}"),
                password_fifo_path: None,
            }))
            .await
            .expect_err("wrong password should fail");
        assert!(
            err.contains("Wrong password"),
            "expected wrong password error on attempt {attempt}, got: {err}"
        );
    }

    let lockout_err = server
        .vault_unlock(Parameters(VaultUnlockParams {
            password: "still-wrong".to_string(),
            password_fifo_path: None,
        }))
        .await
        .expect_err("fifth failed attempt should trigger lockout");
    assert!(
        lockout_err.contains("Too many failed vault unlock attempts"),
        "expected lockout error, got: {lockout_err}"
    );

    let blocked_err = server
        .vault_unlock(Parameters(VaultUnlockParams {
            password: "correct-password".to_string(),
            password_fifo_path: None,
        }))
        .await
        .expect_err("correct password should still be blocked during lockout");
    assert!(
        blocked_err.contains("temporarily locked"),
        "expected temporary lockout error, got: {blocked_err}"
    );

    server.vault_write().failed_attempts = (5, Some(Instant::now() - Duration::from_secs(1)));

    server
        .vault_unlock(Parameters(VaultUnlockParams {
            password: "correct-password".to_string(),
            password_fifo_path: None,
        }))
        .await
        .expect("vault_unlock should succeed after lockout expiry");

    let state = server.vault_read().failed_attempts;
    assert_eq!(state.0, 0);
    assert!(
        state.1.is_none(),
        "lockout should clear on successful unlock"
    );
}

#[tokio::test]
async fn vault_unlock_does_not_reset_failed_attempts_before_password_verification() {
    let server = make_server();

    server
        .vault_init(Parameters(VaultInitParams {
            password: "correct-password".to_string(),
        }))
        .await
        .expect("vault_init should succeed");
    server
        .vault_lock()
        .await
        .expect("vault_lock should succeed");

    server.vault_write().failed_attempts = (5, Some(Instant::now() - Duration::from_secs(1)));

    let err = server
        .vault_unlock(Parameters(VaultUnlockParams {
            password: "still-wrong-after-expiry".to_string(),
            password_fifo_path: None,
        }))
        .await
        .expect_err("wrong password after lockout expiry should relock immediately");
    assert!(
        err.contains("Too many failed vault unlock attempts"),
        "expired lockout must not grant a fresh burst before verification: {err}"
    );

    let state = server.vault_read().failed_attempts;
    assert!(state.0 >= 5);
    assert!(
        state.1.is_some_and(|until| until > Instant::now()),
        "wrong post-expiry attempt should install a new lockout window"
    );
}

#[tokio::test]
async fn vault_get_respects_allowed_agents() {
    let server = make_server();

    server
        .vault_init(Parameters(VaultInitParams {
            password: "allowed-agents-password".to_string(),
        }))
        .await
        .expect("vault_init should succeed");

    server
        .vault_set(Parameters(VaultSetParams {
            name: "SCOPED_SECRET".to_string(),
            value: "scoped-value".to_string(),
            secret_type: "api_key".to_string(),
            description: "restricted secret".to_string(),
            allowed_agents: Some(vec!["agent-a".to_string(), "agent-b".to_string()]),
            enable_rotation: false,
            rotation_strategy: None,
        }))
        .await
        .expect("vault_set should succeed");

    let missing_agent_err = server
        .vault_get(Parameters(VaultGetParams {
            name: "SCOPED_SECRET".to_string(),
            agent_id: None,
            auto_rotate: false,
        }))
        .await
        .expect_err("vault_get should require agent_id for restricted secrets");
    assert!(
        missing_agent_err.contains("agent_id is required"),
        "expected missing agent_id error, got: {missing_agent_err}"
    );

    let denied_err = server
        .vault_get(Parameters(VaultGetParams {
            name: "SCOPED_SECRET".to_string(),
            agent_id: Some("agent-z".to_string()),
            auto_rotate: false,
        }))
        .await
        .expect_err("vault_get should reject unauthorized agents");
    assert!(
        denied_err.contains("Access denied"),
        "expected access denied error, got: {denied_err}"
    );

    let allowed = server
        .vault_get(Parameters(VaultGetParams {
            name: "SCOPED_SECRET".to_string(),
            agent_id: Some("agent-a".to_string()),
            auto_rotate: false,
        }))
        .await
        .expect("vault_get should succeed for allowed agent");
    let allowed_json: serde_json::Value =
        serde_json::from_str(&allowed).expect("vault_get response should be JSON");
    assert_eq!(allowed_json["value"], json!("scoped-value"));
    assert_eq!(allowed_json["allowed_agents"][0], json!("agent-a"));
}

#[tokio::test]
async fn vault_operations_record_audit_entries() {
    let server = make_server();

    server
        .vault_init(Parameters(VaultInitParams {
            password: "audit-password".to_string(),
        }))
        .await
        .expect("vault_init should succeed");

    server
        .vault_set(Parameters(VaultSetParams {
            name: "AUDIT_SECRET".to_string(),
            value: "audit-value".to_string(),
            secret_type: "api_key".to_string(),
            description: "audit trail secret".to_string(),
            allowed_agents: None,
            enable_rotation: false,
            rotation_strategy: None,
        }))
        .await
        .expect("vault_set should succeed");

    server
        .vault_get(Parameters(VaultGetParams {
            name: "AUDIT_SECRET".to_string(),
            agent_id: None,
            auto_rotate: false,
        }))
        .await
        .expect("vault_get should succeed");

    server
        .vault_lock()
        .await
        .expect("vault_lock should succeed");

    let unlock_err = server
        .vault_unlock(Parameters(VaultUnlockParams {
            password: "wrong-audit-password".to_string(),
            password_fifo_path: None,
        }))
        .await
        .expect_err("wrong password should fail");
    assert!(unlock_err.contains("Wrong password"));

    server
        .vault_unlock(Parameters(VaultUnlockParams {
            password: "audit-password".to_string(),
            password_fifo_path: None,
        }))
        .await
        .expect("vault_unlock should succeed");

    server
        .vault_remove(Parameters(VaultRemoveParams {
            name: "AUDIT_SECRET".to_string(),
        }))
        .await
        .expect("vault_remove should succeed");

    let rows = server
        .with_global_store_read(|store| {
            let mut stmt = store
                .connection()
                .prepare(
                    "SELECT operation, secret_name, success
                     FROM vault_audit
                     ORDER BY id ASC",
                )
                .map_err(|e| format!("prepare vault audit query failed: {e}"))?;
            let rows = stmt
                .query_map([], |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, Option<String>>(1)?,
                        row.get::<_, i64>(2)?,
                    ))
                })
                .map_err(|e| format!("query vault audit rows failed: {e}"))?;
            let mut rows_out = Vec::new();
            for row in rows {
                rows_out.push(row.map_err(|e| format!("read vault audit row failed: {e}"))?);
            }
            Ok(rows_out)
        })
        .expect("vault audit query should succeed");

    assert!(
        rows.contains(&("vault_init".to_string(), None, 1)),
        "expected vault_init audit row"
    );
    assert!(
        rows.contains(&("vault_set".to_string(), Some("AUDIT_SECRET".to_string()), 1)),
        "expected vault_set audit row"
    );
    assert!(
        rows.contains(&("vault_get".to_string(), Some("AUDIT_SECRET".to_string()), 1)),
        "expected vault_get audit row"
    );
    assert!(
        rows.contains(&("vault_lock".to_string(), None, 1)),
        "expected vault_lock audit row"
    );
    assert!(
        rows.contains(&("vault_unlock".to_string(), None, 0)),
        "expected failed vault_unlock audit row"
    );
    assert!(
        rows.iter()
            .filter(|(op, secret_name, success)| op == "vault_unlock"
                && secret_name.is_none()
                && *success == 1)
            .count()
            >= 1,
        "expected successful vault_unlock audit row"
    );
    assert!(
        rows.contains(&(
            "vault_remove".to_string(),
            Some("AUDIT_SECRET".to_string()),
            1
        )),
        "expected vault_remove audit row"
    );
}

#[tokio::test]
async fn vault_lock_reports_audit_persistence_failure() {
    let server = make_server();

    server
        .vault_init(Parameters(VaultInitParams {
            password: "audit-warning-password".to_string(),
        }))
        .await
        .expect("vault_init should succeed");

    server
        .with_global_store(|store| {
            store
                .connection()
                .execute("DROP TABLE vault_audit", [])
                .map(|_| ())
                .map_err(|e| format!("drop vault_audit: {e}"))
        })
        .expect("drop vault_audit");

    let response = server
        .vault_lock()
        .await
        .expect("vault_lock business result should still succeed");
    let value: serde_json::Value =
        serde_json::from_str(&response).expect("vault_lock response should be JSON");

    assert_eq!(value["locked"], json!(true));
    let warning = value["vault_audit_warning"]
        .as_str()
        .expect("audit warning should be visible");
    assert!(warning.contains("vault_lock"), "{warning}");
    assert!(
        warning.contains("audit record was not persisted"),
        "{warning}"
    );
    assert!(
        !warning.contains("audit-warning-password"),
        "warning must not leak vault password: {warning}"
    );
}

#[tokio::test]
async fn vault_remove_deletes_secret_and_audit_records() {
    let server = make_server();

    // Initialize vault
    server
        .vault_init(Parameters(VaultInitParams {
            password: "test-password".to_string(),
        }))
        .await
        .expect("vault_init should succeed");

    // Set a secret
    server
        .vault_set(Parameters(VaultSetParams {
            name: "DELETE_ME".to_string(),
            value: "secret-value".to_string(),
            secret_type: "api_key".to_string(),
            description: "to be deleted".to_string(),
            allowed_agents: None,
            enable_rotation: false,
            rotation_strategy: None,
        }))
        .await
        .expect("vault_set should succeed");

    // Verify secret exists
    let get_result = server
        .vault_get(Parameters(VaultGetParams {
            name: "DELETE_ME".to_string(),
            agent_id: None,
            auto_rotate: false,
        }))
        .await;
    assert!(get_result.is_ok(), "secret should exist before removal");

    // Remove the secret
    server
        .vault_remove(Parameters(VaultRemoveParams {
            name: "DELETE_ME".to_string(),
        }))
        .await
        .expect("vault_remove should succeed");

    // Verify secret no longer exists
    let get_after = server
        .vault_get(Parameters(VaultGetParams {
            name: "DELETE_ME".to_string(),
            agent_id: None,
            auto_rotate: false,
        }))
        .await;
    assert!(get_after.is_err(), "secret should not exist after removal");
}

#[tokio::test]
async fn vault_list_filters_by_secret_type() {
    let server = make_server();

    // Initialize vault
    server
        .vault_init(Parameters(VaultInitParams {
            password: "test-password".to_string(),
        }))
        .await
        .expect("vault_init should succeed");

    // Set secrets of different types
    server
        .vault_set(Parameters(VaultSetParams {
            name: "API_KEY_1".to_string(),
            value: "api-value".to_string(),
            secret_type: "api_key".to_string(),
            description: "API key".to_string(),
            allowed_agents: None,
            enable_rotation: false,
            rotation_strategy: None,
        }))
        .await
        .expect("vault_set api_key should succeed");

    server
        .vault_set(Parameters(VaultSetParams {
            name: "OAUTH_TOKEN".to_string(),
            value: "oauth-value".to_string(),
            secret_type: "oauth_token".to_string(),
            description: "OAuth token".to_string(),
            allowed_agents: None,
            enable_rotation: false,
            rotation_strategy: None,
        }))
        .await
        .expect("vault_set oauth_token should succeed");

    // List all secrets (no filter)
    let all = server
        .vault_list(Parameters(VaultListParams { secret_type: None }))
        .await
        .expect("vault_list all should succeed");
    let all_json: Value = serde_json::from_str(&all).unwrap();
    assert_eq!(all_json["secrets"].as_array().unwrap().len(), 2);

    // List only api_key type
    let api_only = server
        .vault_list(Parameters(VaultListParams {
            secret_type: Some("api_key".to_string()),
        }))
        .await
        .expect("vault_list api_key should succeed");
    let api_json: Value = serde_json::from_str(&api_only).unwrap();
    assert_eq!(api_json["secrets"].as_array().unwrap().len(), 1);
    assert_eq!(api_json["secrets"][0]["name"], "API_KEY_1");
}

#[tokio::test]
async fn vault_lock_preserves_env_provider_fallback() {
    std::env::set_var("TACHI_ENV_FALLBACK_API_KEY", "env-secret");
    let server = make_server();

    server
        .vault_init(Parameters(VaultInitParams {
            password: "env-fallback-password".to_string(),
        }))
        .await
        .expect("vault_init should succeed");
    server
        .vault_set(Parameters(VaultSetParams {
            name: "TACHI_ENV_FALLBACK_API_KEY".to_string(),
            value: "vault-secret".to_string(),
            secret_type: "api_key".to_string(),
            description: "env fallback test".to_string(),
            allowed_agents: None,
            enable_rotation: false,
            rotation_strategy: None,
        }))
        .await
        .expect("vault_set should succeed");

    assert_eq!(
        server
            .llm
            .provider_secret_for_tests(&["TACHI_ENV_FALLBACK_API_KEY"])
            .as_deref(),
        Some("vault-secret")
    );

    server
        .vault_lock()
        .await
        .expect("vault_lock should succeed");

    assert_eq!(
        server
            .llm
            .provider_secret_for_tests(&["TACHI_ENV_FALLBACK_API_KEY"])
            .as_deref(),
        Some("env-secret"),
        "locking vault must clear only vault overrides and preserve env fallback"
    );
    std::env::remove_var("TACHI_ENV_FALLBACK_API_KEY");
}

#[tokio::test]
#[allow(clippy::await_holding_lock)] // serializes process-wide env across async vault setup + subprocess spawn
async fn dispatch_vault_env_injection_does_not_export_all_secrets_by_default() {
    let _guard = crate::utils::global_test_lock()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    std::env::remove_var("TACHI_CHILD_ONLY_API_KEY");
    std::env::remove_var("LONGPORT_APP_SECRET");
    std::env::remove_var("TACHI_VAULT_CHILD_ENV");
    std::env::set_var("TACHI_EXISTING_API_KEY", "env-value");
    let server = make_server();

    server
        .vault_init(Parameters(VaultInitParams {
            password: "child-env-default-password".to_string(),
        }))
        .await
        .expect("vault_init should succeed");
    for (name, value) in [
        ("TACHI_CHILD_ONLY_API_KEY", "child-vault-value"),
        ("TACHI_EXISTING_API_KEY", "vault-overrides-env"),
        ("LONGPORT_APP_SECRET", "longport-secret"),
    ] {
        server
            .vault_set(Parameters(VaultSetParams {
                name: name.to_string(),
                value: value.to_string(),
                secret_type: "api_key".to_string(),
                description: "child env default injection test".to_string(),
                allowed_agents: None,
                enable_rotation: false,
                rotation_strategy: None,
            }))
            .await
            .expect("vault_set should succeed");
    }

    let mut cmd = tokio::process::Command::new("sh");
    cmd.arg("-c").arg(
        "printf '%s|%s|%s|' \"$TACHI_CHILD_ONLY_API_KEY\" \"$TACHI_EXISTING_API_KEY\" \"$LONGPORT_APP_SECRET\"",
    );
    let injected = crate::dispatch_ops::apply_unlocked_vault_env(&mut cmd, &server, None);
    assert_eq!(injected, 0);

    let output = cmd.output().await.expect("env probe command should run");
    assert!(output.status.success());
    let stdout = String::from_utf8(output.stdout).expect("env probe output should be utf8");
    assert_eq!(stdout, "|env-value||");

    std::env::remove_var("TACHI_CHILD_ONLY_API_KEY");
    std::env::remove_var("LONGPORT_APP_SECRET");
    std::env::remove_var("TACHI_EXISTING_API_KEY");
    std::env::remove_var("TACHI_VAULT_CHILD_ENV");
}

#[tokio::test]
#[allow(clippy::await_holding_lock)] // serializes process-wide env across async vault setup + subprocess spawn
async fn dispatch_vault_env_injection_overrides_existing_env_when_all_configured() {
    let _guard = crate::utils::global_test_lock()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    std::env::remove_var("TACHI_CHILD_ONLY_API_KEY");
    std::env::remove_var("LONGPORT_APP_SECRET");
    std::env::remove_var("NOT-A-SHELL-NAME");
    std::env::set_var("TACHI_VAULT_CHILD_ENV", "all");
    std::env::set_var("TACHI_EXISTING_API_KEY", "env-value");
    let server = make_server();

    server
        .vault_init(Parameters(VaultInitParams {
            password: "child-env-password".to_string(),
        }))
        .await
        .expect("vault_init should succeed");
    for (name, value) in [
        ("TACHI_CHILD_ONLY_API_KEY", "child-vault-value"),
        ("TACHI_EXISTING_API_KEY", "vault-overrides-env"),
        ("LONGPORT_APP_SECRET", "longport-secret"),
        ("NOT-A-SHELL-NAME", "must-not-inject"),
    ] {
        server
            .vault_set(Parameters(VaultSetParams {
                name: name.to_string(),
                value: value.to_string(),
                secret_type: "api_key".to_string(),
                description: "child env injection test".to_string(),
                allowed_agents: None,
                enable_rotation: false,
                rotation_strategy: None,
            }))
            .await
            .expect("vault_set should succeed");
    }

    let mut cmd = tokio::process::Command::new("sh");
    cmd.arg("-c")
        .arg("printf '%s|%s|%s|' \"$TACHI_CHILD_ONLY_API_KEY\" \"$TACHI_EXISTING_API_KEY\" \"$LONGPORT_APP_SECRET\"; env | grep -q '^NOT-A-SHELL-NAME=' && printf bad || true");
    let injected = crate::dispatch_ops::apply_unlocked_vault_env(&mut cmd, &server, None);
    assert_eq!(injected, 3);

    let output = cmd.output().await.expect("env probe command should run");
    assert!(output.status.success());
    let stdout = String::from_utf8(output.stdout).expect("env probe output should be utf8");
    assert_eq!(
        stdout,
        "child-vault-value|vault-overrides-env|longport-secret|"
    );

    std::env::remove_var("TACHI_CHILD_ONLY_API_KEY");
    std::env::remove_var("LONGPORT_APP_SECRET");
    std::env::remove_var("NOT-A-SHELL-NAME");
    std::env::remove_var("TACHI_EXISTING_API_KEY");
    std::env::remove_var("TACHI_VAULT_CHILD_ENV");

    std::env::remove_var("TACHI_FILL_CHILD_ONLY_API_KEY");
    std::env::remove_var("TACHI_FILL_LONGPORT_APP_SECRET");
    std::env::set_var("TACHI_FILL_EXISTING_API_KEY", "env-value");
    std::env::set_var("TACHI_VAULT_CHILD_ENV", "fill_missing");
    let server = make_server();

    server
        .vault_init(Parameters(VaultInitParams {
            password: "child-env-fill-password".to_string(),
        }))
        .await
        .expect("vault_init should succeed");
    for (name, value) in [
        ("TACHI_FILL_CHILD_ONLY_API_KEY", "child-vault-value"),
        ("TACHI_FILL_EXISTING_API_KEY", "vault-preserved"),
        ("TACHI_FILL_LONGPORT_APP_SECRET", "longport-secret"),
    ] {
        server
            .vault_set(Parameters(VaultSetParams {
                name: name.to_string(),
                value: value.to_string(),
                secret_type: "api_key".to_string(),
                description: "child env fill-missing test".to_string(),
                allowed_agents: None,
                enable_rotation: false,
                rotation_strategy: None,
            }))
            .await
            .expect("vault_set should succeed");
    }

    let mut cmd = tokio::process::Command::new("sh");
    cmd.arg("-c").arg(
        "printf '%s|%s|%s|' \"$TACHI_FILL_CHILD_ONLY_API_KEY\" \"$TACHI_FILL_EXISTING_API_KEY\" \"$TACHI_FILL_LONGPORT_APP_SECRET\"",
    );
    let injected = crate::dispatch_ops::apply_unlocked_vault_env(&mut cmd, &server, None);
    assert_eq!(injected, 2);

    let output = cmd.output().await.expect("env probe command should run");
    assert!(output.status.success());
    let stdout = String::from_utf8(output.stdout).expect("env probe output should be utf8");
    assert_eq!(stdout, "child-vault-value|env-value|longport-secret|");

    std::env::remove_var("TACHI_FILL_CHILD_ONLY_API_KEY");
    std::env::remove_var("TACHI_FILL_LONGPORT_APP_SECRET");
    std::env::remove_var("TACHI_FILL_EXISTING_API_KEY");
    std::env::remove_var("TACHI_VAULT_CHILD_ENV");
}

#[tokio::test]
async fn dispatch_vault_env_injection_resolves_project_bindings_from_cwd() {
    let server = make_server();

    server
        .vault_init(Parameters(VaultInitParams {
            password: "project-env-password".to_string(),
        }))
        .await
        .expect("vault_init should succeed");
    for (name, value) in [
        ("longbridge.1.secret", "longbridge-secret"),
        ("project.override.secret", "project-override"),
        ("PROJECT_SHARED_API_KEY", "global-shared"),
    ] {
        server
            .vault_set(Parameters(VaultSetParams {
                name: name.to_string(),
                value: value.to_string(),
                secret_type: "api_key".to_string(),
                description: "project env binding test".to_string(),
                allowed_agents: None,
                enable_rotation: false,
                rotation_strategy: None,
            }))
            .await
            .expect("vault_set should succeed");
    }

    let temp = tempfile::tempdir().expect("create temp project");
    let project = temp.path().join("project");
    let nested = project.join("src/nested");
    std::fs::create_dir_all(project.join(".tachi")).expect("create .tachi dir");
    std::fs::create_dir_all(&nested).expect("create nested cwd");
    std::fs::write(
        project.join(".tachi/vault.env"),
        "\
# Project-local aliases, not plaintext secrets.
PROJECT_LONGPORT_SECRET=vault:longbridge.1.secret
PROJECT_SHARED_API_KEY=vault:project.override.secret
BAD-NAME=vault:project.override.secret
PROJECT_LITERAL=not-a-vault-alias
PROJECT_MISSING=vault:missing.secret
",
    )
    .expect("write project vault env");

    let mut cmd = tokio::process::Command::new("sh");
    cmd.arg("-c").arg(
        "printf '%s|%s|%s|' \"$PROJECT_LONGPORT_SECRET\" \"$PROJECT_SHARED_API_KEY\" \"$PROJECT_LITERAL\"",
    );
    let injected = crate::dispatch_ops::apply_unlocked_vault_env(&mut cmd, &server, Some(&nested));
    assert_eq!(injected, 2);

    let output = cmd.output().await.expect("env probe command should run");
    assert!(output.status.success());
    let stdout = String::from_utf8(output.stdout).expect("env probe output should be utf8");
    assert_eq!(stdout, "longbridge-secret|project-override||");
}
