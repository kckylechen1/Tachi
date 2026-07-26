use super::*;

#[tokio::test]
async fn vault_rotation_materializes_provider_pool_under_logical_key() {
    let server = make_server();

    server
        .vault_init(Parameters(VaultInitParams {
            password: "hunter2-hunter2".to_string(),
        }))
        .await
        .expect("vault_init should succeed");

    for (name, value) in [
        ("VOYAGE_API_KEY_1", "voyage-key-1"),
        ("VOYAGE_API_KEY_2", "voyage-key-2"),
    ] {
        server
            .vault_set(Parameters(VaultSetParams {
                name: name.to_string(),
                value: value.to_string(),
                agent_id: None,
                secret_type: "api_key".to_string(),
                description: "rotated voyage key".to_string(),
                allowed_agents: None,
                enable_rotation: false,
                rotation_strategy: None,
            }))
            .await
            .expect("vault_set rotation key should succeed");
    }

    server
        .vault_setup_rotation(Parameters(VaultSetupRotationParams {
            prefix: "VOYAGE_API_KEY".to_string(),
            agent_id: None,
            total_keys: 2,
            strategy: "round_robin".to_string(),
        }))
        .await
        .expect("vault_setup_rotation should refresh provider pool");

    assert_eq!(
        server
            .llm
            .provider_key_id_for_tests(&["VOYAGE_API_KEY"])
            .as_deref(),
        Some("VOYAGE_API_KEY_1")
    );
    assert_eq!(
        server
            .llm
            .provider_key_id_for_tests(&["VOYAGE_API_KEY"])
            .as_deref(),
        Some("VOYAGE_API_KEY_2"),
        "provider pools should rotate in-process between healthy concrete keys"
    );

    server.llm.mark_provider_key_rate_limited_for_tests(
        "VOYAGE_API_KEY",
        "VOYAGE_API_KEY_1",
        Some(60),
    );

    assert_eq!(
        server
            .llm
            .provider_key_id_for_tests(&["VOYAGE_API_KEY"])
            .as_deref(),
        Some("VOYAGE_API_KEY_2"),
        "rate-limited concrete keys should be skipped inside the same logical pool"
    );
}
