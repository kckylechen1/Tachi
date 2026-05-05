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
async fn vault_auto_lock_expires_cached_key() {
    let mut server = make_server();
    server.vault_auto_lock_after_secs = 1;

    server
        .vault_init(Parameters(VaultInitParams {
            password: "auto-lock-password".to_string(),
        }))
        .await
        .expect("vault_init should succeed");

    server
        .vault_set(Parameters(VaultSetParams {
            name: "AUTO_LOCK_SECRET".to_string(),
            value: "secret-value".to_string(),
            secret_type: "api_key".to_string(),
            description: "auto lock test secret".to_string(),
            allowed_agents: None,
            enable_rotation: false,
            rotation_strategy: None,
        }))
        .await
        .expect("vault_set should succeed");

    *write_or_recover(&server.vault_unlock_time, "vault_unlock_time") =
        Some(Instant::now() - Duration::from_secs(2));

    let err = server
        .vault_get(Parameters(VaultGetParams {
            name: "AUTO_LOCK_SECRET".to_string(),
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
        }))
        .await
        .expect_err("correct password should still be blocked during lockout");
    assert!(
        blocked_err.contains("temporarily locked"),
        "expected temporary lockout error, got: {blocked_err}"
    );

    *lock_or_recover(&server.vault_failed_attempts, "vault_failed_attempts") =
        (5, Some(Instant::now() - Duration::from_secs(1)));

    server
        .vault_unlock(Parameters(VaultUnlockParams {
            password: "correct-password".to_string(),
        }))
        .await
        .expect("vault_unlock should succeed after lockout expiry");

    let state = *lock_or_recover(&server.vault_failed_attempts, "vault_failed_attempts");
    assert_eq!(state.0, 0);
    assert!(
        state.1.is_none(),
        "lockout should clear on successful unlock"
    );
}

#[tokio::test]
async fn vault_get_respects_allowed_agents() {
    let server = make_server();

    server
        .vault_init(Parameters(VaultInitParams {
            password: "allowed-agents-password".to_string(),
        }))
        .await
        .expect("vault_init should succeed");

    server
        .vault_set(Parameters(VaultSetParams {
            name: "SCOPED_SECRET".to_string(),
            value: "scoped-value".to_string(),
            secret_type: "api_key".to_string(),
            description: "restricted secret".to_string(),
            allowed_agents: Some(vec!["agent-a".to_string(), "agent-b".to_string()]),
            enable_rotation: false,
            rotation_strategy: None,
        }))
        .await
        .expect("vault_set should succeed");

    let missing_agent_err = server
        .vault_get(Parameters(VaultGetParams {
            name: "SCOPED_SECRET".to_string(),
            agent_id: None,
            auto_rotate: false,
        }))
        .await
        .expect_err("vault_get should require agent_id for restricted secrets");
    assert!(
        missing_agent_err.contains("agent_id is required"),
        "expected missing agent_id error, got: {missing_agent_err}"
    );

    let denied_err = server
        .vault_get(Parameters(VaultGetParams {
            name: "SCOPED_SECRET".to_string(),
            agent_id: Some("agent-z".to_string()),
            auto_rotate: false,
        }))
        .await
        .expect_err("vault_get should reject unauthorized agents");
    assert!(
        denied_err.contains("Access denied"),
        "expected access denied error, got: {denied_err}"
    );

    let allowed = server
        .vault_get(Parameters(VaultGetParams {
            name: "SCOPED_SECRET".to_string(),
            agent_id: Some("agent-a".to_string()),
            auto_rotate: false,
        }))
        .await
        .expect("vault_get should succeed for allowed agent");
    let allowed_json: serde_json::Value =
        serde_json::from_str(&allowed).expect("vault_get response should be JSON");
    assert_eq!(allowed_json["value"], json!("scoped-value"));
    assert_eq!(allowed_json["allowed_agents"][0], json!("agent-a"));
}

#[tokio::test]
async fn vault_operations_record_audit_entries() {
    let server = make_server();

    server
        .vault_init(Parameters(VaultInitParams {
            password: "audit-password".to_string(),
        }))
        .await
        .expect("vault_init should succeed");

    server
        .vault_set(Parameters(VaultSetParams {
            name: "AUDIT_SECRET".to_string(),
            value: "audit-value".to_string(),
            secret_type: "api_key".to_string(),
            description: "audit trail secret".to_string(),
            allowed_agents: None,
            enable_rotation: false,
            rotation_strategy: None,
        }))
        .await
        .expect("vault_set should succeed");

    server
        .vault_get(Parameters(VaultGetParams {
            name: "AUDIT_SECRET".to_string(),
            agent_id: None,
            auto_rotate: false,
        }))
        .await
        .expect("vault_get should succeed");

    server
        .vault_lock()
        .await
        .expect("vault_lock should succeed");

    let unlock_err = server
        .vault_unlock(Parameters(VaultUnlockParams {
            password: "wrong-audit-password".to_string(),
        }))
        .await
        .expect_err("wrong password should fail");
    assert!(unlock_err.contains("Wrong password"));

    server
        .vault_unlock(Parameters(VaultUnlockParams {
            password: "audit-password".to_string(),
        }))
        .await
        .expect("vault_unlock should succeed");

    server
        .vault_remove(Parameters(VaultRemoveParams {
            name: "AUDIT_SECRET".to_string(),
        }))
        .await
        .expect("vault_remove should succeed");

    let rows = server
        .with_global_store_read(|store| {
            let mut stmt = store
                .connection()
                .prepare(
                    "SELECT operation, secret_name, success
                     FROM vault_audit
                     ORDER BY id ASC",
                )
                .map_err(|e| format!("prepare vault audit query failed: {e}"))?;
            let rows = stmt
                .query_map([], |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, Option<String>>(1)?,
                        row.get::<_, i64>(2)?,
                    ))
                })
                .map_err(|e| format!("query vault audit rows failed: {e}"))?;
            let mut rows_out = Vec::new();
            for row in rows {
                rows_out.push(row.map_err(|e| format!("read vault audit row failed: {e}"))?);
            }
            Ok(rows_out)
        })
        .expect("vault audit query should succeed");

    assert!(
        rows.contains(&("vault_init".to_string(), None, 1)),
        "expected vault_init audit row"
    );
    assert!(
        rows.contains(&("vault_set".to_string(), Some("AUDIT_SECRET".to_string()), 1)),
        "expected vault_set audit row"
    );
    assert!(
        rows.contains(&("vault_get".to_string(), Some("AUDIT_SECRET".to_string()), 1)),
        "expected vault_get audit row"
    );
    assert!(
        rows.contains(&("vault_lock".to_string(), None, 1)),
        "expected vault_lock audit row"
    );
    assert!(
        rows.contains(&("vault_unlock".to_string(), None, 0)),
        "expected failed vault_unlock audit row"
    );
    assert!(
        rows.iter()
            .filter(|(op, secret_name, success)| op == "vault_unlock"
                && secret_name.is_none()
                && *success == 1)
            .count()
            >= 1,
        "expected successful vault_unlock audit row"
    );
    assert!(
        rows.contains(&(
            "vault_remove".to_string(),
            Some("AUDIT_SECRET".to_string()),
            1
        )),
        "expected vault_remove audit row"
    );
}

#[tokio::test]
async fn vault_remove_deletes_secret_and_audit_records() {
    let server = make_server();

    // Initialize vault
    server
        .vault_init(Parameters(VaultInitParams {
            password: "test-password".to_string(),
        }))
        .await
        .expect("vault_init should succeed");

    // Set a secret
    server
        .vault_set(Parameters(VaultSetParams {
            name: "DELETE_ME".to_string(),
            value: "secret-value".to_string(),
            secret_type: "api_key".to_string(),
            description: "to be deleted".to_string(),
            allowed_agents: None,
            enable_rotation: false,
            rotation_strategy: None,
        }))
        .await
        .expect("vault_set should succeed");

    // Verify secret exists
    let get_result = server
        .vault_get(Parameters(VaultGetParams {
            name: "DELETE_ME".to_string(),
            agent_id: None,
            auto_rotate: false,
        }))
        .await;
    assert!(get_result.is_ok(), "secret should exist before removal");

    // Remove the secret
    server
        .vault_remove(Parameters(VaultRemoveParams {
            name: "DELETE_ME".to_string(),
        }))
        .await
        .expect("vault_remove should succeed");

    // Verify secret no longer exists
    let get_after = server
        .vault_get(Parameters(VaultGetParams {
            name: "DELETE_ME".to_string(),
            agent_id: None,
            auto_rotate: false,
        }))
        .await;
    assert!(get_after.is_err(), "secret should not exist after removal");
}

#[tokio::test]
async fn vault_list_filters_by_secret_type() {
    let server = make_server();

    // Initialize vault
    server
        .vault_init(Parameters(VaultInitParams {
            password: "test-password".to_string(),
        }))
        .await
        .expect("vault_init should succeed");

    // Set secrets of different types
    server
        .vault_set(Parameters(VaultSetParams {
            name: "API_KEY_1".to_string(),
            value: "api-value".to_string(),
            secret_type: "api_key".to_string(),
            description: "API key".to_string(),
            allowed_agents: None,
            enable_rotation: false,
            rotation_strategy: None,
        }))
        .await
        .expect("vault_set api_key should succeed");

    server
        .vault_set(Parameters(VaultSetParams {
            name: "OAUTH_TOKEN".to_string(),
            value: "oauth-value".to_string(),
            secret_type: "oauth_token".to_string(),
            description: "OAuth token".to_string(),
            allowed_agents: None,
            enable_rotation: false,
            rotation_strategy: None,
        }))
        .await
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
