use super::*;

async fn init_agent_acl_vault(password: &str) -> crate::tests::TestServer {
    let server = make_server();
    server
        .vault_init(Parameters(VaultInitParams {
            password: password.to_string(),
        }))
        .await
        .expect("vault_init should succeed");
    server
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
            agent_id: None,
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
async fn vault_set_new_entry_allows_agent_id() {
    let server = init_agent_acl_vault("golden-g1-password").await;

    server
        .vault_set(Parameters(VaultSetParams {
            name: "K1".to_string(),
            value: "new-value".to_string(),
            agent_id: Some("bob".to_string()),
            secret_type: "api_key".to_string(),
            description: "G1 new entry".to_string(),
            allowed_agents: None,
            enable_rotation: false,
            rotation_strategy: None,
        }))
        .await
        .expect("G1 set NEW K1 with agent_id=bob should succeed");

    let got = server
        .vault_get(Parameters(VaultGetParams {
            name: "K1".to_string(),
            agent_id: None,
            auto_rotate: false,
        }))
        .await
        .expect("G1 stored entry should be readable");
    let got_json: serde_json::Value = serde_json::from_str(&got).expect("G1 get JSON");
    assert_eq!(got_json["value"], json!("new-value"));
}

#[tokio::test]
async fn vault_set_denies_overwrite_when_agent_not_allowed() {
    let server = init_agent_acl_vault("golden-g2-password").await;

    server
        .vault_set(Parameters(VaultSetParams {
            name: "K1".to_string(),
            value: "alice-value".to_string(),
            agent_id: None,
            secret_type: "api_key".to_string(),
            description: "G2 restricted seed".to_string(),
            allowed_agents: Some(vec!["alice".to_string()]),
            enable_rotation: false,
            rotation_strategy: None,
        }))
        .await
        .expect("G2 seed should succeed");

    let denied = server
        .vault_set(Parameters(VaultSetParams {
            name: "K1".to_string(),
            value: "bob-value".to_string(),
            agent_id: Some("bob".to_string()),
            secret_type: "api_key".to_string(),
            description: "G2 denied overwrite".to_string(),
            allowed_agents: Some(vec!["bob".to_string()]),
            enable_rotation: false,
            rotation_strategy: None,
        }))
        .await
        .expect_err("G2 overwrite by bob should be denied");
    assert!(
        denied.contains("Access denied"),
        "G2 expected access denied error, got: {denied}"
    );

    let unchanged = server
        .vault_get(Parameters(VaultGetParams {
            name: "K1".to_string(),
            agent_id: Some("alice".to_string()),
            auto_rotate: false,
        }))
        .await
        .expect("G2 original entry should remain readable by alice");
    let unchanged_json: serde_json::Value = serde_json::from_str(&unchanged).expect("G2 get JSON");
    assert_eq!(unchanged_json["value"], json!("alice-value"));
    assert_eq!(unchanged_json["allowed_agents"], json!(["alice"]));
}

#[tokio::test]
async fn vault_set_allows_overwrite_when_agent_allowed() {
    let server = init_agent_acl_vault("golden-g3-password").await;

    server
        .vault_set(Parameters(VaultSetParams {
            name: "K1".to_string(),
            value: "old-value".to_string(),
            agent_id: None,
            secret_type: "api_key".to_string(),
            description: "G3 restricted seed".to_string(),
            allowed_agents: Some(vec!["alice".to_string()]),
            enable_rotation: false,
            rotation_strategy: None,
        }))
        .await
        .expect("G3 seed should succeed");

    server
        .vault_set(Parameters(VaultSetParams {
            name: "K1".to_string(),
            value: "new-value".to_string(),
            agent_id: Some("alice".to_string()),
            secret_type: "api_key".to_string(),
            description: "G3 allowed overwrite".to_string(),
            allowed_agents: Some(vec!["alice".to_string()]),
            enable_rotation: false,
            rotation_strategy: None,
        }))
        .await
        .expect("G3 overwrite by alice should succeed");

    let got = server
        .vault_get(Parameters(VaultGetParams {
            name: "K1".to_string(),
            agent_id: Some("alice".to_string()),
            auto_rotate: false,
        }))
        .await
        .expect("G3 overwritten entry should be readable");
    let got_json: serde_json::Value = serde_json::from_str(&got).expect("G3 get JSON");
    assert_eq!(got_json["value"], json!("new-value"));
}

#[tokio::test]
async fn vault_set_allows_unrestricted_overwrite_with_agent_id() {
    let server = init_agent_acl_vault("golden-g4-password").await;

    server
        .vault_set(Parameters(VaultSetParams {
            name: "K2".to_string(),
            value: "old-value".to_string(),
            agent_id: None,
            secret_type: "api_key".to_string(),
            description: "G4 unrestricted seed".to_string(),
            allowed_agents: None,
            enable_rotation: false,
            rotation_strategy: None,
        }))
        .await
        .expect("G4 seed should succeed");

    server
        .vault_set(Parameters(VaultSetParams {
            name: "K2".to_string(),
            value: "new-value".to_string(),
            agent_id: Some("bob".to_string()),
            secret_type: "api_key".to_string(),
            description: "G4 unrestricted overwrite".to_string(),
            allowed_agents: None,
            enable_rotation: false,
            rotation_strategy: None,
        }))
        .await
        .expect("G4 unrestricted overwrite by bob should succeed");

    let got = server
        .vault_get(Parameters(VaultGetParams {
            name: "K2".to_string(),
            agent_id: None,
            auto_rotate: false,
        }))
        .await
        .expect("G4 overwritten entry should be readable");
    let got_json: serde_json::Value = serde_json::from_str(&got).expect("G4 get JSON");
    assert_eq!(got_json["value"], json!("new-value"));
}

#[tokio::test]
async fn vault_remove_denies_restricted_entry_when_agent_not_allowed() {
    let server = init_agent_acl_vault("golden-g5-password").await;

    server
        .vault_set(Parameters(VaultSetParams {
            name: "K1".to_string(),
            value: "alice-value".to_string(),
            agent_id: None,
            secret_type: "api_key".to_string(),
            description: "G5 restricted seed".to_string(),
            allowed_agents: Some(vec!["alice".to_string()]),
            enable_rotation: false,
            rotation_strategy: None,
        }))
        .await
        .expect("G5 seed should succeed");

    let denied = server
        .vault_remove(Parameters(VaultRemoveParams {
            name: "K1".to_string(),
            agent_id: Some("bob".to_string()),
        }))
        .await
        .expect_err("G5 remove by bob should be denied");
    assert!(
        denied.contains("Access denied"),
        "G5 expected access denied error, got: {denied}"
    );

    let still_present = server
        .vault_get(Parameters(VaultGetParams {
            name: "K1".to_string(),
            agent_id: Some("alice".to_string()),
            auto_rotate: false,
        }))
        .await
        .expect("G5 entry should still exist");
    let still_present_json: serde_json::Value =
        serde_json::from_str(&still_present).expect("G5 get JSON");
    assert_eq!(still_present_json["value"], json!("alice-value"));
}

#[tokio::test]
async fn vault_remove_absent_entry_stays_graceful_without_agent_id() {
    let server = init_agent_acl_vault("golden-g6-password").await;

    let removed = server
        .vault_remove(Parameters(VaultRemoveParams {
            name: "NOPE".to_string(),
            agent_id: None,
        }))
        .await
        .expect("G6 absent remove should return graceful JSON");
    let removed_json: serde_json::Value = serde_json::from_str(&removed).expect("G6 remove JSON");
    assert_eq!(removed_json["removed"], json!(false));
}

#[tokio::test]
async fn vault_setup_rotation_records_success_audit_row() {
    let server = init_agent_acl_vault("golden-g7-password").await;

    for (name, value) in [("G7_POOL_1", "pool-key-1"), ("G7_POOL_2", "pool-key-2")] {
        server
            .vault_set(Parameters(VaultSetParams {
                name: name.to_string(),
                value: value.to_string(),
                agent_id: None,
                secret_type: "api_key".to_string(),
                description: "G7 pool member".to_string(),
                allowed_agents: None,
                enable_rotation: false,
                rotation_strategy: None,
            }))
            .await
            .expect("G7 seed pool member should succeed");
    }

    server
        .vault_setup_rotation(Parameters(VaultSetupRotationParams {
            prefix: "G7_POOL".to_string(),
            agent_id: None,
            total_keys: 2,
            strategy: "round_robin".to_string(),
        }))
        .await
        .expect("G7 setup_rotation should succeed");

    let audit_count = server
        .with_global_store_read(|store| {
            store
                .connection()
                .query_row(
                    "SELECT COUNT(*)
                     FROM vault_audit
                     WHERE operation = 'vault_setup_rotation'
                       AND secret_name = 'G7_POOL'
                       AND success = 1",
                    [],
                    |row| row.get::<_, i64>(0),
                )
                .map_err(|e| format!("query G7 audit row failed: {e}"))
        })
        .expect("G7 audit query should succeed");
    assert_eq!(audit_count, 1, "G7 expected one setup_rotation audit row");
}

#[tokio::test]
async fn vault_set_api_key_pool_denies_clobbering_restricted_member() {
    let server = init_agent_acl_vault("golden-g8-password").await;

    server
        .vault_set(Parameters(VaultSetParams {
            name: "P_1".to_string(),
            value: "alice-pool-value".to_string(),
            agent_id: None,
            secret_type: "api_key".to_string(),
            description: "G8 restricted pool member".to_string(),
            allowed_agents: Some(vec!["alice".to_string()]),
            enable_rotation: false,
            rotation_strategy: None,
        }))
        .await
        .expect("G8 seed should succeed");

    let denied = server
        .vault_set_api_key_pool(Parameters(VaultSetApiKeyPoolParams {
            prefix: "P".to_string(),
            agent_id: Some("bob".to_string()),
            values: vec!["bob-pool-value".to_string()],
            strategy: "round_robin".to_string(),
            description: "G8 denied pool replacement".to_string(),
            allowed_agents: None,
        }))
        .await
        .expect_err("G8 pool replacement by bob should be denied");
    assert!(
        denied.contains("Access denied"),
        "G8 expected access denied error, got: {denied}"
    );

    let unchanged = server
        .vault_get(Parameters(VaultGetParams {
            name: "P_1".to_string(),
            agent_id: Some("alice".to_string()),
            auto_rotate: false,
        }))
        .await
        .expect("G8 original pool member should remain readable by alice");
    let unchanged_json: serde_json::Value = serde_json::from_str(&unchanged).expect("G8 get JSON");
    assert_eq!(unchanged_json["value"], json!("alice-pool-value"));
    assert_eq!(unchanged_json["allowed_agents"], json!(["alice"]));
}

/// G10: a restricted pool member whose numeric suffix exceeds u32::MAX must NOT
/// escape the pool ACL gate. The write path (`vault_replace_api_key_pool`) resolves
/// membership with a usize-based index, so such an entry WOULD be clobbered; the gate
/// must resolve membership with the same predicate (`api_key_pool_member_index`) or it
/// fails open. Regression lock for the collect_rotation_entries (u32) divergence.
#[tokio::test]
async fn vault_set_api_key_pool_gates_member_with_suffix_over_u32_max() {
    let server = init_agent_acl_vault("golden-g10-password").await;

    // 9_999_999_999 > u32::MAX (4_294_967_295); parses as usize, not as u32.
    server
        .vault_set(Parameters(VaultSetParams {
            name: "P_9999999999".to_string(),
            value: "alice-orphan-value".to_string(),
            agent_id: None,
            secret_type: "api_key".to_string(),
            description: "G10 restricted high-suffix member".to_string(),
            allowed_agents: Some(vec!["alice".to_string()]),
            enable_rotation: false,
            rotation_strategy: None,
        }))
        .await
        .expect("G10 seed should succeed");

    let denied = server
        .vault_set_api_key_pool(Parameters(VaultSetApiKeyPoolParams {
            prefix: "P".to_string(),
            agent_id: Some("bob".to_string()),
            values: vec!["bob-pool-value".to_string()],
            strategy: "round_robin".to_string(),
            description: "G10 denied pool replacement".to_string(),
            allowed_agents: None,
        }))
        .await
        .expect_err("G10 pool replacement by bob must be denied, not fail open");
    assert!(
        denied.contains("Access denied"),
        "G10 expected access denied error, got: {denied}"
    );

    let still_present = server
        .vault_get(Parameters(VaultGetParams {
            name: "P_9999999999".to_string(),
            agent_id: Some("alice".to_string()),
            auto_rotate: false,
        }))
        .await
        .expect("G10 restricted member must survive the denied pool replace");
    let still_present_json: serde_json::Value =
        serde_json::from_str(&still_present).expect("G10 get JSON");
    assert_eq!(still_present_json["value"], json!("alice-orphan-value"));
    assert_eq!(still_present_json["allowed_agents"], json!(["alice"]));
}
