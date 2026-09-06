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
            agent_id: None,
            secret_type: "api_key".to_string(),
            description: "primary openai key".to_string(),
            allowed_agents: None,
            enable_rotation: false,
            rotation_strategy: None,
            rebind: false,
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
            use_keychain: false,
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
    assert_eq!(status_json["session"]["unlocked"], json!(true));
    assert!(
        status_json["provider_cache"]["secret_pool_count"]
            .as_u64()
            .is_some_and(|count| count >= 1),
        "vault_status should report provider cache materialized: {status_json:#}"
    );
    assert_eq!(status_json["resolver"]["state"], json!("unlocked"));
    assert!(status_json["secure_store"]["auto_unlock_available"].is_boolean());
}

/// The server dependency is compiled with vault-test-api but without cfg(test).
/// Its ordinary init and stored-key paths must still enforce the product KDF.
#[tokio::test]
async fn feature_enabled_product_server_uses_production_kdf() {
    let server = make_server();
    let password = "feature-unification-password";
    server
        .vault_init(Parameters(VaultInitParams {
            password: password.to_string(),
        }))
        .await
        .expect("product vault init");
    let mut config = server
        .with_global_store_read(|store| store.vault_get_config().map_err(|err| err.to_string()))
        .expect("read product config")
        .expect("initialized config");
    assert_eq!(config.kdf_params, vault_kit::active_kdf_params_json());
    assert_eq!(
        serde_json::from_str::<Value>(&config.kdf_params).expect("KDF JSON"),
        json!({"m": 65536, "t": 3, "p": 4})
    );
    let key = crate::vault_crypto::derive_verified_key_from_stored_config(&config, password)
        .expect("product stored KDF must verify the actual initialized key");
    assert!(key.bytes() == &server.unlocked_key_bytes());

    config.kdf_params = vault_kit::cheap_kdf_params_json().to_string();
    assert!(matches!(
        crate::vault_crypto::derive_verified_key_from_stored_config(&config, password),
        Err(crate::vault_crypto::StoredVaultKeyDerivationError::KdfParamsFormat(_))
    ));
}

#[cfg(unix)]
#[test]
fn feature_enabled_product_keychain_ignores_test_env() {
    let _guard = crate::utils::global_test_lock()
        .lock()
        .unwrap_or_else(|err| err.into_inner());
    let _password = crate::test_support::EnvRestore::set(
        "TACHI_TEST_KEYCHAIN_PASSWORD",
        "environment-password-must-not-be-used",
    );
    let missing = crate::test_support::EnvRestore::set("TACHI_TEST_FORCE_KEYCHAIN_MISSING", "1");

    // On macOS use a PATH-local security fixture, never the user's real Keychain.
    #[cfg(target_os = "macos")]
    let (_security_dir, _path) = {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().expect("security fixture directory");
        let security = dir.path().join("security");
        std::fs::write(&security, "#!/bin/sh\nprintf 'security-fixture-password\\n'\n")
            .expect("write security fixture");
        std::fs::set_permissions(&security, std::fs::Permissions::from_mode(0o700))
            .expect("executable security fixture");
        let path = crate::test_support::EnvRestore::set_path("PATH", dir.path());
        (dir, path)
    };

    for missing_override in [Some(missing), None] {
        let result = crate::vault_crypto::read_password_from_macos_keychain();
        #[cfg(target_os = "macos")]
        assert_eq!(result.expect("real security command path"), "security-fixture-password");
        #[cfg(not(target_os = "macos"))]
        assert!(result
            .expect_err("non-macOS product path must reject Keychain reads")
            .contains("Keychain unlock is only supported on macOS"));
        drop(missing_override);
    }
}
