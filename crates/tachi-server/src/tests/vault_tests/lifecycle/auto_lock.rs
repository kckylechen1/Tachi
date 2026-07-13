use super::*;

#[tokio::test]
async fn vault_auto_lock_expires_cached_key() {
    let server = make_server();
    // Use a longer timeout than the slowest CI step between init/set so the
    // setup itself does not race the auto-lock; the test then forces
    // expiration by rewinding `vault_unlock_time` below.
    server.vault_write().auto_lock_after_secs = 30;

    server
        .vault_init(Parameters(VaultInitParams {
            password: "auto-lock-password".to_string(),
        }))
        .await
        .expect("vault_init should succeed");

    server
        .vault_set(Parameters(VaultSetParams {
            name: "OPENAI_API_KEY".to_string(),
            value: "secret-value".to_string(),
            agent_id: None,
            secret_type: "api_key".to_string(),
            description: "auto lock test secret".to_string(),
            allowed_agents: None,
            enable_rotation: false,
            rotation_strategy: None,
        }))
        .await
        .expect("vault_set should succeed");
    assert_eq!(
        server
            .llm
            .provider_secret_for_tests(&["OPENAI_API_KEY"])
            .as_deref(),
        Some("secret-value"),
        "vault_set should refresh provider cache before auto-lock"
    );

    server.vault_write().unlock_time = Some(Instant::now() - Duration::from_secs(60));

    let err = server
        .vault_get(Parameters(VaultGetParams {
            name: "OPENAI_API_KEY".to_string(),
            agent_id: None,
            auto_rotate: false,
        }))
        .await
        .expect_err("vault_get should fail after auto-lock timeout");
    assert!(
        err.contains("Vault auto-locked"),
        "expected auto-lock error, got: {err}"
    );

    let status = server
        .vault_status()
        .await
        .expect("vault_status should succeed");
    let status_json: serde_json::Value =
        serde_json::from_str(&status).expect("vault_status response should be JSON");
    assert_eq!(status_json["locked"], json!(true));
    assert_eq!(status_json["session"]["locked"], json!(true));
    assert!(matches!(
        status_json["resolver"]["state"].as_str(),
        Some("locked" | "locked_keychain_available")
    ));
    assert!(
        status_json["provider_cache"]["secret_pool_count"]
            .as_u64()
            .is_some(),
        "vault_status should report provider cache count: {status_json:#}"
    );
    assert!(
        server
            .llm
            .provider_secret_for_tests(&["OPENAI_API_KEY"])
            .is_some(),
        "auto-lock must NOT clear provider secrets — they survive auto-lock (#28)"
    );
}
