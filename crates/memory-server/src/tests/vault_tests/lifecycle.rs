use super::*;

#[tokio::test]
async fn vault_init_set_get_lock_unlock_roundtrip() {
    let server = make_server();

    let init = server
        .vault_init(Parameters(VaultInitParams {
            password: "correct horse battery staple".to_string(),
        }))
        .await
        .expect("vault_init should succeed");
    let init_json: serde_json::Value =
        serde_json::from_str(&init).expect("vault_init response should be JSON");
    assert_eq!(init_json["initialized"], json!(true));

    server
        .vault_set(Parameters(VaultSetParams {
            name: "OPENAI_API_KEY".to_string(),
            value: "sk-test-123".to_string(),
            secret_type: "api_key".to_string(),
            description: "primary openai key".to_string(),
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
        Some("sk-test-123"),
        "vault_set should make provider keys available to the LLM client"
    );

    let get = server
        .vault_get(Parameters(VaultGetParams {
            name: "OPENAI_API_KEY".to_string(),
            agent_id: None,
            auto_rotate: false,
        }))
        .await
        .expect("vault_get should succeed");
    let get_json: serde_json::Value =
        serde_json::from_str(&get).expect("vault_get response should be JSON");
    assert_eq!(get_json["value"], json!("sk-test-123"));

    server
        .vault_lock()
        .await
        .expect("vault_lock should succeed");
    assert!(
        server
            .llm
            .provider_secret_for_tests(&["OPENAI_API_KEY"])
            .is_none(),
        "vault_lock should clear provider keys cached from the vault"
    );
    let locked_get = server
        .vault_get(Parameters(VaultGetParams {
            name: "OPENAI_API_KEY".to_string(),
            agent_id: None,
            auto_rotate: false,
        }))
        .await;
    assert!(locked_get.is_err(), "vault_get should fail while locked");

    let listed = server
        .vault_list(Parameters(VaultListParams { secret_type: None }))
        .await
        .expect("vault_list should succeed while locked");
    let listed_json: serde_json::Value =
        serde_json::from_str(&listed).expect("vault_list response should be JSON");
    assert_eq!(listed_json["count"], json!(1));
    assert_eq!(listed_json["secrets"][0]["name"], json!("OPENAI_API_KEY"));

    server
        .vault_unlock(Parameters(VaultUnlockParams {
            password: "correct horse battery staple".to_string(),
            password_fifo_path: None,
        }))
        .await
        .expect("vault_unlock should succeed");
    assert_eq!(
        server
            .llm
            .provider_secret_for_tests(&["OPENAI_API_KEY"])
            .as_deref(),
        Some("sk-test-123"),
        "vault_unlock should reload provider keys into the LLM client"
    );

    let status = server
        .vault_status()
        .await
        .expect("vault_status should succeed");
    let status_json: serde_json::Value =
        serde_json::from_str(&status).expect("vault_status response should be JSON");
    assert_eq!(status_json["initialized"], json!(true));
    assert_eq!(status_json["locked"], json!(false));
    assert_eq!(status_json["entry_count"], json!(1));
}

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
    assert!(
        server
            .llm
            .provider_secret_for_tests(&["OPENAI_API_KEY"])
            .is_none(),
        "auto-lock should clear provider keys cached from the vault"
    );
}

#[tokio::test]
async fn vault_unlock_enforces_bruteforce_lockout_and_resets_on_success() {
    let server = make_server();

    server
        .vault_init(Parameters(VaultInitParams {
            password: "correct-password".to_string(),
        }))
        .await
        .expect("vault_init should succeed");
    server
        .vault_lock()
        .await
        .expect("vault_lock should succeed");

    for attempt in 1..5 {
        let err = server
            .vault_unlock(Parameters(VaultUnlockParams {
                password: format!("wrong-password-{attempt}"),
                password_fifo_path: None,
            }))
            .await
            .expect_err("wrong password should fail");
        assert!(
            err.contains("Wrong password"),
            "expected wrong password error on attempt {attempt}, got: {err}"
        );
    }

    let lockout_err = server
        .vault_unlock(Parameters(VaultUnlockParams {
            password: "still-wrong".to_string(),
            password_fifo_path: None,
        }))
        .await
        .expect_err("fifth failed attempt should trigger lockout");
    assert!(
        lockout_err.contains("Too many failed vault unlock attempts"),
        "expected lockout error, got: {lockout_err}"
    );

    let blocked_err = server
        .vault_unlock(Parameters(VaultUnlockParams {
            password: "correct-password".to_string(),
            password_fifo_path: None,
        }))
        .await
        .expect_err("correct password should still be blocked during lockout");
    assert!(
        blocked_err.contains("temporarily locked"),
        "expected temporary lockout error, got: {blocked_err}"
    );

    server.vault_write().failed_attempts = (5, Some(Instant::now() - Duration::from_secs(1)));

    server
        .vault_unlock(Parameters(VaultUnlockParams {
            password: "correct-password".to_string(),
            password_fifo_path: None,
        }))
        .await
        .expect("vault_unlock should succeed after lockout expiry");

    let state = server.vault_read().failed_attempts;
    assert_eq!(state.0, 0);
    assert!(
        state.1.is_none(),
        "lockout should clear on successful unlock"
    );
}

#[tokio::test]
async fn vault_unlock_does_not_reset_failed_attempts_before_password_verification() {
    let server = make_server();

    server
        .vault_init(Parameters(VaultInitParams {
            password: "correct-password".to_string(),
        }))
        .await
        .expect("vault_init should succeed");
    server
        .vault_lock()
        .await
        .expect("vault_lock should succeed");

    server.vault_write().failed_attempts = (5, Some(Instant::now() - Duration::from_secs(1)));

    let err = server
        .vault_unlock(Parameters(VaultUnlockParams {
            password: "still-wrong-after-expiry".to_string(),
            password_fifo_path: None,
        }))
        .await
        .expect_err("wrong password after lockout expiry should relock immediately");
    assert!(
        err.contains("Too many failed vault unlock attempts"),
        "expired lockout must not grant a fresh burst before verification: {err}"
    );

    let state = server.vault_read().failed_attempts;
    assert!(state.0 >= 5);
    assert!(
        state.1.is_some_and(|until| until > Instant::now()),
        "wrong post-expiry attempt should install a new lockout window"
    );
}
