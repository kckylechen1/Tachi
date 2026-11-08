use super::*;

#[tokio::test]
async fn vault_rotation_prefix_get_round_robin_works() {
    let server = make_server();

    server
        .seed_vault("hunter2-hunter2")
        .expect("seed unlocked vault");

    for (name, value) in [
        ("GEMINI_API_KEY_1", "gemini-key-1"),
        ("GEMINI_API_KEY_2", "gemini-key-2"),
    ] {
        server
            .seed_vault_secret(VaultSecretFixture {
                name: name.to_string(),
                value: value.to_string(),
                secret_type: "api_key".to_string(),
                description: "rotated key".to_string(),
                allowed_agents: None,
            })
            .expect("seed rotation member");
    }

    server
        .vault_setup_rotation(Parameters(VaultSetupRotationParams {
            prefix: "GEMINI_API_KEY".to_string(),
            agent_id: None,
            total_keys: 2,
            strategy: "round_robin".to_string(),
        }))
        .await
        .expect("vault_setup_rotation should succeed");

    let first = server
        .read_vault_secret("GEMINI_API_KEY", None, false)
        .expect("first rotated vault_get should succeed");

    let second = server
        .read_vault_secret("GEMINI_API_KEY", None, false)
        .expect("second rotated vault_get should succeed");

    assert_eq!(first, "gemini-key-1");
    assert_eq!(second, "gemini-key-2");
}
