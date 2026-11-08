use super::*;

#[tokio::test]
async fn owner_remove_deletes_secret() {
    let server = make_server();

    // Initialize vault
    server
        .seed_vault("test-password")
        .expect("seed unlocked vault");

    // Set a secret
    server
        .seed_vault_secret(VaultSecretFixture {
            name: "DELETE_ME".to_string(),
            value: "secret-value".to_string(),
            secret_type: "api_key".to_string(),
            description: "to be deleted".to_string(),
            allowed_agents: None,
        })
        .expect("seed secret");

    // Verify secret exists
    let get_result = server.read_vault_secret("DELETE_ME", None, false);
    assert!(get_result.is_ok(), "secret should exist before removal");

    // Remove the secret
    server
        .owner_remove_vault_secret("DELETE_ME")
        .await
        .expect("vault_remove should succeed");

    // Verify secret no longer exists
    let get_after = server.read_vault_secret("DELETE_ME", None, false);
    assert!(get_after.is_err(), "secret should not exist after removal");
}

#[tokio::test]
async fn vault_list_filters_by_secret_type() {
    let server = make_server();

    // Initialize vault
    server
        .seed_vault("test-password")
        .expect("seed unlocked vault");

    // Set secrets of different types
    server
        .seed_vault_secret(VaultSecretFixture {
            name: "API_KEY_1".to_string(),
            value: "api-value".to_string(),
            secret_type: "api_key".to_string(),
            description: "API key".to_string(),
            allowed_agents: None,
        })
        .expect("vault_set api_key should succeed");

    server
        .seed_vault_secret(VaultSecretFixture {
            name: "OAUTH_TOKEN".to_string(),
            value: "oauth-value".to_string(),
            secret_type: "oauth_token".to_string(),
            description: "OAuth token".to_string(),
            allowed_agents: None,
        })
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
