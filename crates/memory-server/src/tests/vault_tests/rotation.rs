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
            secret_type: "api_key".to_string(),
            description: "capability-scoped key".to_string(),
            allowed_agents: Some(vec!["mcp:allowed".to_string()]),
            enable_rotation: false,
            rotation_strategy: None,
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

#[tokio::test]
async fn vault_rotation_prefix_get_round_robin_works() {
    let server = make_server();

    server
        .vault_init(Parameters(VaultInitParams {
            password: "hunter2-hunter2".to_string(),
        }))
        .await
        .expect("vault_init should succeed");

    for (name, value) in [
        ("GEMINI_API_KEY_1", "gemini-key-1"),
        ("GEMINI_API_KEY_2", "gemini-key-2"),
    ] {
        server
            .vault_set(Parameters(VaultSetParams {
                name: name.to_string(),
                value: value.to_string(),
                secret_type: "api_key".to_string(),
                description: "rotated key".to_string(),
                allowed_agents: None,
                enable_rotation: false,
                rotation_strategy: None,
            }))
            .await
            .expect("vault_set rotation key should succeed");
    }

    server
        .vault_setup_rotation(Parameters(VaultSetupRotationParams {
            prefix: "GEMINI_API_KEY".to_string(),
            total_keys: 2,
            strategy: "round_robin".to_string(),
        }))
        .await
        .expect("vault_setup_rotation should succeed");

    let first = server
        .vault_get(Parameters(VaultGetParams {
            name: "GEMINI_API_KEY".to_string(),
            agent_id: None,
            auto_rotate: false,
        }))
        .await
        .expect("first rotated vault_get should succeed");
    let first_json: serde_json::Value =
        serde_json::from_str(&first).expect("first vault_get response should be JSON");

    let second = server
        .vault_get(Parameters(VaultGetParams {
            name: "GEMINI_API_KEY".to_string(),
            agent_id: None,
            auto_rotate: false,
        }))
        .await
        .expect("second rotated vault_get should succeed");
    let second_json: serde_json::Value =
        serde_json::from_str(&second).expect("second vault_get response should be JSON");

    assert_eq!(first_json["name"], json!("GEMINI_API_KEY_1"));
    assert_eq!(first_json["value"], json!("gemini-key-1"));
    assert_eq!(second_json["name"], json!("GEMINI_API_KEY_2"));
    assert_eq!(second_json["value"], json!("gemini-key-2"));
}

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

    server
        .llm
        .mark_provider_key_rate_limited_for_tests("VOYAGE_API_KEY_1", Some(60));

    assert_eq!(
        server
            .llm
            .provider_key_id_for_tests(&["VOYAGE_API_KEY"])
            .as_deref(),
        Some("VOYAGE_API_KEY_2"),
        "rate-limited concrete keys should be skipped inside the same logical pool"
    );
}

#[tokio::test]
async fn vault_get_auto_rotate_does_not_advance_rotation_on_decrypt_failure() {
    let server = make_server();

    server
        .vault_init(Parameters(VaultInitParams {
            password: "rotation-failure-password".to_string(),
        }))
        .await
        .expect("vault_init should succeed");

    for (name, value) in [
        ("BROKEN_API_KEY_1", "broken-key-1"),
        ("BROKEN_API_KEY_2", "broken-key-2"),
    ] {
        server
            .vault_set(Parameters(VaultSetParams {
                name: name.to_string(),
                value: value.to_string(),
                secret_type: "api_key".to_string(),
                description: "rotated key".to_string(),
                allowed_agents: None,
                enable_rotation: false,
                rotation_strategy: None,
            }))
            .await
            .expect("vault_set rotation key should succeed");
    }

    server
        .vault_setup_rotation(Parameters(VaultSetupRotationParams {
            prefix: "BROKEN_API_KEY".to_string(),
            total_keys: 2,
            strategy: "round_robin".to_string(),
        }))
        .await
        .expect("vault_setup_rotation should succeed");

    server
        .with_global_store(|store| {
            let mut entry = store
                .vault_get_entry("BROKEN_API_KEY_1")
                .map_err(|e| e.to_string())?
                .expect("rotation member should exist");
            entry.encrypted_value = "not valid base64".to_string();
            store.vault_upsert_entry(&entry).map_err(|e| e.to_string())
        })
        .expect("corrupt rotation member");

    let err = server
        .vault_get(Parameters(VaultGetParams {
            name: "BROKEN_API_KEY".to_string(),
            agent_id: None,
            auto_rotate: true,
        }))
        .await
        .expect_err("corrupted selected key should fail before rotation advances");
    assert!(err.contains("Bad ciphertext base64"), "{err}");

    let current_index = server
        .with_global_store_read(|store| {
            store
                .vault_get_rotation("BROKEN_API_KEY")
                .map_err(|e| e.to_string())
                .map(|rotation| rotation.expect("rotation should exist").current_index)
        })
        .expect("read rotation");
    assert_eq!(
        current_index, 1,
        "failed decrypt must not skip the selected rotation member"
    );
}
