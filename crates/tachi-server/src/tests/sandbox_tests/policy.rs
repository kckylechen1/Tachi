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
