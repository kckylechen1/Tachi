use super::*;

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
            agent_id: None,
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
            agent_id: None,
            secret_type: "api_key".to_string(),
            description: "unrelated provider key".to_string(),
            allowed_agents: None,
            enable_rotation: false,
            rotation_strategy: None,
            rebind: false,
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
async fn vault_api_key_pool_shrink_refuses_to_delete_surplus_members() {
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
            agent_id: None,
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

    let err = server
        .vault_set_api_key_pool(Parameters(VaultSetApiKeyPoolParams {
            prefix: "SHRINK_API_KEY".to_string(),
            agent_id: None,
            values: vec!["shrink-key-1b".to_string()],
            strategy: "round_robin".to_string(),
            description: "shrunk pool".to_string(),
            allowed_agents: None,
        }))
        .await
        .expect_err("default pool replacement must refuse to delete surplus members");
    assert!(err.contains("SHRINK_API_KEY_2"), "{err}");
    assert!(err.contains("SHRINK_API_KEY_3"), "{err}");

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
    assert!(names.contains(&"SHRINK_API_KEY_2"), "{names:?}");
    assert!(names.contains(&"SHRINK_API_KEY_3"), "{names:?}");

    let leased = server
        .vault_lease_api_key(Parameters(VaultLeaseApiKeyParams {
            name: "SHRINK_API_KEY".to_string(),
            env_name: None,
            agent_id: None,
        }))
        .await
        .expect("lease should use the intact original pool");
    let leased_json: serde_json::Value = serde_json::from_str(&leased).expect("lease JSON");
    assert_eq!(leased_json["key_id"], json!("SHRINK_API_KEY_1"));
    assert_eq!(leased_json["env"]["SHRINK_API_KEY"], json!("shrink-key-1"));
}
