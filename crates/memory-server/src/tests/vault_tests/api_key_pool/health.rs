use super::*;

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
