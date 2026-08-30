use super::*;

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
            agent_id: None,
            secret_type: "api_key".to_string(),
            description: "capability-scoped key".to_string(),
            allowed_agents: Some(vec!["mcp:allowed".to_string()]),
            enable_rotation: false,
            rotation_strategy: None,
            rebind: false,
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
