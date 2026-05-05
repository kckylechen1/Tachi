use super::*;

#[tokio::test]
async fn sandbox_policy_tool_roundtrip() {
    let server = make_server();

    let set_resp = server
        .sandbox_set_policy(Parameters(SandboxSetPolicyParams {
            capability_id: "mcp:exa".to_string(),
            runtime_type: "process".to_string(),
            env_allowlist: vec!["EXA_API_KEY".to_string()],
            fs_read_roots: vec!["/tmp".to_string()],
            fs_write_roots: vec!["/tmp".to_string()],
            cwd_roots: vec!["/tmp".to_string()],
            max_startup_ms: 5000,
            max_tool_ms: 7000,
            max_concurrency: 2,
            enabled: true,
        }))
        .await
        .expect("sandbox_set_policy should succeed");
    let set_json: serde_json::Value =
        serde_json::from_str(&set_resp).expect("sandbox_set_policy response should be JSON");
    assert_eq!(set_json["status"], json!("ok"));

    let get_resp = server
        .sandbox_get_policy(Parameters(SandboxGetPolicyParams {
            capability_id: "mcp:exa".to_string(),
        }))
        .await
        .expect("sandbox_get_policy should succeed");
    let get_json: serde_json::Value =
        serde_json::from_str(&get_resp).expect("sandbox_get_policy response should be JSON");
    assert_eq!(get_json["capability_id"], json!("mcp:exa"));
    assert_eq!(get_json["max_tool_ms"], json!(7000));
    assert_eq!(get_json["max_concurrency"], json!(2));

    let list_resp = server
        .sandbox_list_policies(Parameters(SandboxListPoliciesParams {
            enabled_only: true,
            limit: 10,
        }))
        .await
        .expect("sandbox_list_policies should succeed");
    let list_json: serde_json::Value =
        serde_json::from_str(&list_resp).expect("sandbox_list_policies response should be JSON");
    assert!(
        list_json["count"].as_u64().unwrap_or(0) >= 1,
        "expected at least one policy"
    );
}

#[tokio::test]
async fn shell_and_section9_alias_tools_work() {
    let server = make_server();

    let shell_set = server
        .shell_set_policy(Parameters(SandboxSetPolicyParams {
            capability_id: "mcp:alias-shell".to_string(),
            runtime_type: "process".to_string(),
            env_allowlist: vec![],
            fs_read_roots: vec![],
            fs_write_roots: vec![],
            cwd_roots: vec![],
            max_startup_ms: 2000,
            max_tool_ms: 3000,
            max_concurrency: 1,
            enabled: true,
        }))
        .await
        .expect("shell_set_policy should succeed");
    let shell_set_json: serde_json::Value =
        serde_json::from_str(&shell_set).expect("shell_set_policy response should be JSON");
    assert_eq!(shell_set_json["status"], json!("ok"));

    let shell_get = server
        .shell_get_policy(Parameters(SandboxGetPolicyParams {
            capability_id: "mcp:alias-shell".to_string(),
        }))
        .await
        .expect("shell_get_policy should succeed");
    let shell_get_json: serde_json::Value =
        serde_json::from_str(&shell_get).expect("shell_get_policy response should be JSON");
    assert_eq!(shell_get_json["capability_id"], json!("mcp:alias-shell"));

    let shell_list = server
        .shell_list_policies(Parameters(SandboxListPoliciesParams {
            enabled_only: false,
            limit: 20,
        }))
        .await
        .expect("shell_list_policies should succeed");
    let shell_list_json: serde_json::Value =
        serde_json::from_str(&shell_list).expect("shell_list_policies response should be JSON");
    assert!(
        shell_list_json["count"].as_u64().unwrap_or(0) >= 1,
        "expected shell policy rows"
    );

    let shell_audit = server
        .shell_exec_audit(Parameters(SandboxExecAuditParams {
            capability_id: None,
            stage: None,
            decision: None,
            limit: 5,
        }))
        .await
        .expect("shell_exec_audit should succeed");
    let shell_audit_json: serde_json::Value =
        serde_json::from_str(&shell_audit).expect("shell_exec_audit response should be JSON");
    assert!(shell_audit_json["items"].is_array());

    let review = server
        .section9_review(Parameters(HubReviewParams {
            id: "mcp:not-exist".to_string(),
            review_status: "approved".to_string(),
            enabled: Some(true),
        }))
        .await
        .expect("section9_review should return JSON");
    let review_json: serde_json::Value =
        serde_json::from_str(&review).expect("section9_review response should be JSON");
    assert_eq!(review_json["updated"], json!(false));

    let section9_log = server
        .section9_audit_log(Parameters(AuditLogParams {
            limit: 5,
            server_filter: None,
        }))
        .await
        .expect("section9_audit_log should succeed");
    let section9_log_json: serde_json::Value =
        serde_json::from_str(&section9_log).expect("section9_audit_log response should be JSON");
    assert!(section9_log_json.is_array());
}

#[tokio::test]
async fn process_runtime_rejects_unenforceable_fs_roots() {
    let server = make_server();
    let cap = make_mcp_capability("mcp:fs-locked", 1);

    server
        .with_global_store(|store| {
            store
                .hub_register(&cap)
                .map_err(|e| format!("register fs-locked failed: {e}"))
        })
        .expect("failed to register fs-locked capability");

    server
        .sandbox_set_policy(Parameters(SandboxSetPolicyParams {
            capability_id: "mcp:fs-locked".to_string(),
            runtime_type: "process".to_string(),
            env_allowlist: vec![],
            fs_read_roots: vec!["/tmp".to_string()],
            fs_write_roots: vec![],
            cwd_roots: vec![],
            max_startup_ms: 1000,
            max_tool_ms: 1000,
            max_concurrency: 1,
            enabled: true,
        }))
        .await
        .expect("sandbox_set_policy should succeed");

    let err = server
        .proxy_call_internal("fs-locked", "echo", None)
        .await
        .expect_err("fs root policy should fail closed for process runtime");

    assert!(
        err.to_string().contains("cannot enforce"),
        "unexpected error: {err}"
    );
}

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

#[tokio::test]
async fn sandbox_policy_prevents_unregistered_capability_startup() {
    let server = make_server();

    // Register a capability without a policy
    let cap = HubCapability {
        id: "mcp:unregistered-policy".to_string(),
        cap_type: "mcp".to_string(),
        name: "unregistered-policy".to_string(),
        version: 1,
        description: "test capability without policy".to_string(),
        definition: r#"{"transport":"stdio","command":"echo","args":["test"]}"#.to_string(),
        enabled: true,
        review_status: "approved".to_string(),
        health_status: "healthy".to_string(),
        last_error: None,
        last_success_at: None,
        last_failure_at: None,
        fail_streak: 0,
        active_version: None,
        exposure_mode: "gateway".to_string(),
        uses: 0,
        successes: 0,
        failures: 0,
        avg_rating: 0.0,
        last_used: None,
        created_at: String::new(),
        updated_at: String::new(),
    };

    server
        .with_global_store(|store| {
            store
                .hub_register(&cap)
                .map_err(|e| format!("register failed: {e}"))
        })
        .expect("failed to register capability");

    // Try to call without setting policy - should fail with policy error
    let result = server
        .proxy_call_internal("unregistered-policy", "test_tool", None)
        .await;

    assert!(
        result.is_err(),
        "proxy_call should fail without sandbox policy"
    );
    let err_msg = result.unwrap_err().to_string();
    assert!(
        err_msg.contains("no sandbox policy") || err_msg.contains("Sandbox"),
        "Error should mention sandbox policy: {}",
        err_msg
    );
}

#[tokio::test]
async fn tachi_shell_rejects_invalid_action() {
    let server = make_server();
    let err = crate::shell_ops::handle_tachi_shell(&server, shell_params("launch"))
        .await
        .expect_err("invalid shell action should fail");
    assert!(err.contains("Invalid action"), "unexpected error: {err}");
}

#[tokio::test]
async fn tachi_shell_status_rejects_invalid_flow_id() {
    let server = make_server();
    let mut params = shell_params("status");
    params.flow_id = Some("../flow_escape".to_string());

    let err = crate::shell_ops::handle_tachi_shell(&server, params)
        .await
        .expect_err("invalid flow id should fail");
    assert!(err.contains("Invalid flow_id"), "unexpected error: {err}");
}
