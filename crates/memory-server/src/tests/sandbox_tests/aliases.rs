use super::*;

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
