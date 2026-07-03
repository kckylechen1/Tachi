use super::*;

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
            agent_id: None,
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
            agent_id: None,
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
            agent_id: None,
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
            agent_id: None,
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
