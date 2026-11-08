use super::*;
use chrono::Utc;
use memory_server_runtime::AgentProfile;

async fn init_agent_acl_vault(password: &str) -> crate::tests::TestServer {
    let server = make_server();
    server
        .seed_vault(&(password.to_string()))
        .expect("seed unlocked vault");
    server
}

#[tokio::test]
async fn live_vault_read_respects_allowed_agents() {
    let server = make_server();

    server
        .seed_vault("allowed-agents-password")
        .expect("seed unlocked vault");

    server
        .seed_vault_secret(VaultSecretFixture {
            name: "SCOPED_SECRET".to_string(),
            value: "scoped-value".to_string(),
            secret_type: "api_key".to_string(),
            description: "restricted secret".to_string(),
            allowed_agents: Some(vec!["agent-a".to_string(), "agent-b".to_string()]),
        })
        .expect("seed secret");

    let missing_agent_err = server
        .read_vault_secret("SCOPED_SECRET", None, false)
        .expect_err("vault_get should require agent_id for restricted secrets");
    assert!(
        missing_agent_err.contains("agent_id is required"),
        "expected missing agent_id error, got: {missing_agent_err}"
    );

    let denied_err = server
        .read_vault_secret("SCOPED_SECRET", Some("agent-z"), false)
        .expect_err("vault_get should reject unauthorized agents");
    assert!(
        denied_err.contains("Access denied"),
        "expected access denied error, got: {denied_err}"
    );

    let allowed = server
        .read_vault_secret("SCOPED_SECRET", Some("agent-a"), false)
        .expect("vault_get should succeed for allowed agent");
    assert_eq!(allowed, "scoped-value");
    assert_eq!(
        stored_acl(&server, "SCOPED_SECRET"),
        Some(vec!["agent-a".into(), "agent-b".into()])
    );
}

#[tokio::test]
async fn vault_acl_g1_bound_agent_allows_restricted_read_without_caller_id() {
    let server = init_agent_acl_vault("golden-g1-password").await;
    server.set_bound_agent_id_for_test(Some("alice"));

    server
        .seed_vault_secret(VaultSecretFixture {
            name: "K1".to_string(),
            value: "alice-value".to_string(),
            secret_type: "api_key".to_string(),
            description: "G1 bound read seed".to_string(),
            allowed_agents: Some(vec!["alice".to_string()]),
        })
        .expect("G1 seed should succeed");

    let got = server
        .read_vault_secret("K1", None, false)
        .expect("G1 server-bound alice should satisfy restricted read without caller agent_id");
    assert_eq!(got, "alice-value");
}

#[tokio::test]
async fn vault_acl_g2_bound_agent_rejects_mismatched_caller_on_read() {
    let server = init_agent_acl_vault("golden-g2-password").await;
    server.set_bound_agent_id_for_test(Some("alice"));

    server
        .seed_vault_secret(VaultSecretFixture {
            name: "K1".to_string(),
            value: "bob-value".to_string(),
            secret_type: "api_key".to_string(),
            description: "G2 spoofed read seed".to_string(),
            allowed_agents: Some(vec!["bob".to_string()]),
        })
        .expect("G2 seed should succeed");

    let denied = server
        .read_vault_secret("K1", Some("bob"), false)
        .expect_err("G2 caller bob must not override server-bound alice on read");
    assert!(
        denied.contains("server-bound TACHI_AGENT_ID"),
        "G2 expected bound identity mismatch error, got: {denied}"
    );
}

#[tokio::test]
async fn vault_acl_g4_unbound_server_preserves_legacy_caller_agent_id() {
    let server = init_agent_acl_vault("golden-g4-password").await;
    server.set_bound_agent_id_for_test(None);

    server
        .seed_vault_secret(VaultSecretFixture {
            name: "K2".to_string(),
            value: "alice-value".to_string(),
            secret_type: "api_key".to_string(),
            description: "G4 legacy caller seed".to_string(),
            allowed_agents: Some(vec!["alice".to_string()]),
        })
        .expect("G4 seed should succeed");

    let got = server
        .read_vault_secret("K2", Some("alice"), false)
        .expect("G4 unbound server should preserve caller-supplied agent_id behavior");
    assert_eq!(got, "alice-value");
}

#[tokio::test]
async fn vault_acl_g5_agent_register_is_not_a_vault_binding() {
    let server = init_agent_acl_vault("golden-g5-password").await;
    server.set_bound_agent_id_for_test(None);

    server
        .seed_vault_secret(VaultSecretFixture {
            name: "K1".to_string(),
            value: "alice-value".to_string(),
            secret_type: "api_key".to_string(),
            description: "G5 restricted seed".to_string(),
            allowed_agents: Some(vec!["alice".to_string()]),
        })
        .expect("G5 seed should succeed");

    server.set_agent_profile_for_test(Some(AgentProfile {
        agent_id: "alice".to_string(),
        display_name: "alice".to_string(),
        capabilities: Vec::new(),
        tool_filter: None,
        rate_limit_rpm: None,
        rate_limit_burst: None,
        registered_at: Utc::now().to_rfc3339(),
    }));

    let denied = server
        .read_vault_secret("K1", None, false)
        .expect_err("G5 agent_register must not satisfy vault ACL without TACHI_AGENT_ID");
    assert!(
        denied.contains("agent_id is required"),
        "G5 expected missing caller agent_id error, got: {denied}"
    );
}

#[tokio::test]
async fn vault_setup_rotation_records_success_audit_row() {
    let server = init_agent_acl_vault("golden-g7-password").await;

    for (name, value) in [("G7_POOL_1", "pool-key-1"), ("G7_POOL_2", "pool-key-2")] {
        server
            .seed_vault_secret(VaultSecretFixture {
                name: name.to_string(),
                value: value.to_string(),
                secret_type: "api_key".to_string(),
                description: "G7 pool member".to_string(),
                allowed_agents: None,
            })
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
async fn vault_setup_rotation_denies_restricted_pool_member_when_agent_not_allowed() {
    let server = init_agent_acl_vault("golden-g9-password").await;

    for (name, value, allowed_agents) in [
        (
            "G9_POOL_1",
            "alice-pool-value",
            Some(vec!["alice".to_string()]),
        ),
        ("G9_POOL_2", "shared-pool-value", None),
    ] {
        server
            .seed_vault_secret(VaultSecretFixture {
                name: name.to_string(),
                value: value.to_string(),
                secret_type: "api_key".to_string(),
                description: "G9 pool member".to_string(),
                allowed_agents: allowed_agents,
            })
            .expect("G9 seed pool member should succeed");
    }

    let denied = server
        .vault_setup_rotation(Parameters(VaultSetupRotationParams {
            prefix: "G9_POOL".to_string(),
            agent_id: Some("bob".to_string()),
            total_keys: 2,
            strategy: "round_robin".to_string(),
        }))
        .await
        .expect_err("G9 setup_rotation by bob should be denied");
    assert!(
        denied.contains("Access denied"),
        "G9 expected access denied error, got: {denied}"
    );

    let rotation = server
        .with_global_store_read(|store| {
            store
                .vault_get_rotation("G9_POOL")
                .map_err(|e| format!("query G9 rotation failed: {e}"))
        })
        .expect("G9 rotation query should succeed");
    assert!(
        rotation.is_none(),
        "G9 denied setup_rotation must not write rotation config"
    );

    let still_present = server
        .read_vault_secret("G9_POOL_1", Some("alice"), false)
        .expect("G9 restricted member should remain readable by alice");
    assert_eq!(still_present, "alice-pool-value");
    assert_eq!(stored_acl(&server, "G9_POOL_1"), Some(vec!["alice".into()]));
}

#[tokio::test]
async fn owner_set_pool_denies_clobbering_restricted_member() {
    let server = init_agent_acl_vault("golden-g8-password").await;

    server
        .seed_vault_secret(VaultSecretFixture {
            name: "P_1".to_string(),
            value: "alice-pool-value".to_string(),
            secret_type: "api_key".to_string(),
            description: "G8 restricted pool member".to_string(),
            allowed_agents: Some(vec!["alice".to_string()]),
        })
        .expect("G8 seed should succeed");

    let denied = server
        .owner_set_vault_pool("P", &(vec!["bob-pool-value".to_string()]), "round_robin")
        .await
        .expect_err("G8 identityless owner pool replacement should be denied");
    assert!(
        denied.contains("Access denied") && denied.contains("agent-restricted pool member 'P_1'"),
        "G8 expected owner ACL refusal, got: {denied}"
    );

    let unchanged = server
        .read_vault_secret("P_1", Some("alice"), false)
        .expect("G8 original pool member should remain readable by alice");
    assert_eq!(unchanged, "alice-pool-value");
    assert_eq!(stored_acl(&server, "P_1"), Some(vec!["alice".into()]));
}

/// G10: a restricted pool member whose numeric suffix exceeds u32::MAX must NOT
/// escape the pool ACL gate. The write path (`vault_replace_api_key_pool`) resolves
/// membership with a usize-based index, so such an entry WOULD be clobbered; the gate
/// must resolve membership with the same predicate (`api_key_pool_member_index`) or it
/// fails open. Regression lock for the collect_rotation_entries (u32) divergence.
#[tokio::test]
async fn owner_set_pool_gates_member_with_suffix_over_u32_max() {
    let server = init_agent_acl_vault("golden-g10-password").await;

    // 9_999_999_999 > u32::MAX (4_294_967_295); parses as usize, not as u32.
    server
        .seed_vault_secret(VaultSecretFixture {
            name: "P_9999999999".to_string(),
            value: "alice-orphan-value".to_string(),
            secret_type: "api_key".to_string(),
            description: "G10 restricted high-suffix member".to_string(),
            allowed_agents: Some(vec!["alice".to_string()]),
        })
        .expect("G10 seed should succeed");

    let denied = server
        .owner_set_vault_pool("P", &(vec!["bob-pool-value".to_string()]), "round_robin")
        .await
        .expect_err("G10 identityless owner pool replacement must be denied, not fail open");
    assert!(
        denied.contains("Access denied")
            && denied.contains("agent-restricted pool member 'P_9999999999'"),
        "G10 expected owner ACL refusal, got: {denied}"
    );

    let still_present = server
        .read_vault_secret("P_9999999999", Some("alice"), false)
        .expect("G10 restricted member must survive the denied pool replace");
    assert_eq!(still_present, "alice-orphan-value");
    assert_eq!(
        stored_acl(&server, "P_9999999999"),
        Some(vec!["alice".into()])
    );
}

fn stored_acl(server: &TestServer, name: &str) -> Option<Vec<String>> {
    server
        .with_global_store_read(|store| {
            Ok(store
                .vault_get_entry(name)
                .map_err(|e| e.to_string())?
                .expect("stored ACL fixture")
                .allowed_agents)
        })
        .expect("read persisted ACL")
}
