use super::*;

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
