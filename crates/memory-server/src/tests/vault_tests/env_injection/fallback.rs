use super::*;

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
