use super::*;

#[tokio::test]
async fn vault_provider_cache_skips_agent_scoped_api_keys() {
    std::env::remove_var("LOCKED_DOWN_API_KEY");
    let server = make_server();

    server
        .seed_vault("correct horse battery staple")
        .expect("seed unlocked vault");

    server
        .seed_vault_secret(VaultSecretFixture {
            name: "LOCKED_DOWN_API_KEY".to_string(),
            value: "scoped-secret".to_string(),
            secret_type: "api_key".to_string(),
            description: "capability-scoped key".to_string(),
            allowed_agents: Some(vec!["mcp:allowed".to_string()]),
        })
        .expect("seed secret");
    server
        .refresh_llm_provider_secrets_from_vault()
        .expect("publish fixture credential");

    assert!(
        server
            .llm
            .provider_secret_for_tests(&["LOCKED_DOWN_API_KEY"])
            .is_none(),
        "agent-scoped provider keys must not enter the server-wide LLM cache"
    );

    let get = server
        .read_vault_secret("LOCKED_DOWN_API_KEY", Some("mcp:allowed"), false)
        .expect("allowed agent should still be able to read the scoped key");
    assert_eq!(get, "scoped-secret");
}
