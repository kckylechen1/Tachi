use super::*;

#[tokio::test]
async fn sandbox_check_respects_access_rules() {
    let server = make_server();

    // Set up a sandbox rule allowing read access for a specific role
    server
        .sandbox_set_rule(Parameters(SandboxSetRuleParams {
            agent_role: "test-role".to_string(),
            path_pattern: "/test/*".to_string(),
            access_level: "read".to_string(),
        }))
        .await
        .expect("sandbox_set_rule should succeed");

    // Check read access - should be allowed
    let check_read = server
        .sandbox_check(Parameters(SandboxCheckParams {
            agent_role: "test-role".to_string(),
            path: "/test/something".to_string(),
            operation: "read".to_string(),
        }))
        .await
        .expect("sandbox_check should succeed");

    let read_json: Value = serde_json::from_str(&check_read).unwrap();
    assert!(read_json["allowed"].as_bool().unwrap());

    // Check write access on read-only path - should be denied
    let check_write = server
        .sandbox_check(Parameters(SandboxCheckParams {
            agent_role: "test-role".to_string(),
            path: "/test/something".to_string(),
            operation: "write".to_string(),
        }))
        .await
        .expect("sandbox_check should succeed");

    let write_json: Value = serde_json::from_str(&check_write).unwrap();
    assert!(!write_json["allowed"].as_bool().unwrap());
}

#[tokio::test]
async fn sandbox_set_rule_updates_existing_rule() {
    let server = make_server();

    // Set initial rule with read access
    server
        .sandbox_set_rule(Parameters(SandboxSetRuleParams {
            agent_role: "update-role".to_string(),
            path_pattern: "/sensitive/*".to_string(),
            access_level: "read".to_string(),
        }))
        .await
        .expect("sandbox_set_rule should succeed");

    // Update to write access
    server
        .sandbox_set_rule(Parameters(SandboxSetRuleParams {
            agent_role: "update-role".to_string(),
            path_pattern: "/sensitive/*".to_string(),
            access_level: "write".to_string(),
        }))
        .await
        .expect("sandbox_set_rule update should succeed");

    // Verify write access is now allowed
    let check_write = server
        .sandbox_check(Parameters(SandboxCheckParams {
            agent_role: "update-role".to_string(),
            path: "/sensitive/data".to_string(),
            operation: "write".to_string(),
        }))
        .await
        .expect("sandbox_check should succeed");

    let write_json: Value = serde_json::from_str(&check_write).unwrap();
    assert!(write_json["allowed"].as_bool().unwrap());
}
