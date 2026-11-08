use super::*;

#[tokio::test]
async fn vault_api_key_lease_skips_unusable_health_members() {
    let server = make_server();

    server
        .seed_vault("pool-health-password")
        .expect("seed unlocked vault");
    server
        .owner_set_vault_pool(
            "TRANSIT_API_KEY",
            &(vec!["transit-key-1".to_string(), "transit-key-2".to_string()]),
            "round_robin",
        )
        .await
        .expect("owner pool write should succeed");

    server.llm.mark_provider_key_rate_limited_for_tests(
        "TRANSIT_API_KEY",
        "TRANSIT_API_KEY_1",
        Some(60),
    );

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
async fn owner_key_result_acknowledges_durable_health_and_lease_selection() {
    let server = make_server();

    server
        .seed_vault("record-health-password")
        .expect("seed unlocked vault");
    server
        .owner_set_vault_pool(
            "BROKER_API_KEY",
            &(vec!["broker-key-1".to_string(), "broker-key-2".to_string()]),
            "round_robin",
        )
        .await
        .expect("owner pool write should succeed");

    // Raw MCP management is retired. This native fixture exercises the owner
    // handler's full-row acknowledgement; real router tests reject all raw names.
    let assert_owner_ack = |response: &str| {
        assert!(!response.contains("broker-key-1"));
        assert!(!response.contains("broker-key-2"));
        let body: serde_json::Value = serde_json::from_str(response).expect("owner JSON");
        let health: memcore::vault::VaultKeyHealth =
            serde_json::from_value(body["health"].clone()).expect("complete owner health row");
        let durable = server
            .with_global_store_read(|store| {
                store
                    .vault_get_key_health(&health.logical_name, &health.key_id)
                    .map_err(|error| error.to_string())
            })
            .expect("read acknowledged health")
            .expect("acknowledged health must be durable");
        assert!(
            chrono::DateTime::parse_from_rfc3339(&durable.updated_at).unwrap()
                >= chrono::DateTime::parse_from_rfc3339(&health.updated_at).unwrap()
        );
        let mut acknowledged = serde_json::to_value(&health).unwrap();
        let mut persisted = serde_json::to_value(&durable).unwrap();
        acknowledged.as_object_mut().unwrap().remove("updated_at");
        persisted.as_object_mut().unwrap().remove("updated_at");
        assert_eq!(
            acknowledged, persisted,
            "all acknowledged health fields must persist"
        );
    };

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
    assert_owner_ack(&rate_limited);
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
    assert_owner_ack(&auth_failed);

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
    assert_owner_ack(&success);
}
