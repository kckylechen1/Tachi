use super::*;

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
            password_fifo_path: None,
        }))
        .await
        .expect_err("wrong password should fail");
    assert!(unlock_err.contains("Wrong password"));

    server
        .vault_unlock(Parameters(VaultUnlockParams {
            password: "audit-password".to_string(),
            password_fifo_path: None,
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
async fn vault_lock_reports_audit_persistence_failure() {
    let server = make_server();

    server
        .vault_init(Parameters(VaultInitParams {
            password: "audit-warning-password".to_string(),
        }))
        .await
        .expect("vault_init should succeed");

    server
        .with_global_store(|store| {
            store
                .connection()
                .execute("DROP TABLE vault_audit", [])
                .map(|_| ())
                .map_err(|e| format!("drop vault_audit: {e}"))
        })
        .expect("drop vault_audit");

    let response = server
        .vault_lock()
        .await
        .expect("vault_lock business result should still succeed");
    let value: serde_json::Value =
        serde_json::from_str(&response).expect("vault_lock response should be JSON");

    assert_eq!(value["locked"], json!(true));
    let warning = value["vault_audit_warning"]
        .as_str()
        .expect("audit warning should be visible");
    assert!(warning.contains("vault_lock"), "{warning}");
    assert!(
        warning.contains("audit record was not persisted"),
        "{warning}"
    );
    assert!(
        !warning.contains("audit-warning-password"),
        "warning must not leak vault password: {warning}"
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
