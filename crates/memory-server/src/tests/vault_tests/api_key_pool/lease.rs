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
            agent_id: None,
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
