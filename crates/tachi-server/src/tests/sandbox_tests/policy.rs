use super::*;

#[tokio::test]
async fn sandbox_policy_tool_roundtrip() {
    let server = make_server();

    let set_resp = server
        .tachi_sandbox(Parameters(sandbox_params(
            "set_policy",
            json!({
                "capability_id": "mcp:exa",
                "runtime_type": "process",
                "env_allowlist": ["EXA_API_KEY"],
                "fs_read_roots": ["/tmp"],
                "fs_write_roots": ["/tmp"],
                "cwd_roots": ["/tmp"],
                "max_startup_ms": 5000,
                "max_tool_ms": 7000,
                "max_concurrency": 2,
                "enabled": true,
            }),
        )))
        .await
        .expect("tachi_sandbox(action='set_policy') should succeed");
    let set_json: serde_json::Value =
        serde_json::from_str(&set_resp).expect("set_policy response should be JSON");
    assert_eq!(set_json["status"], json!("ok"));

    let get_resp = server
        .tachi_sandbox(Parameters(sandbox_params(
            "get_policy",
            json!({
                "capability_id": "mcp:exa",
            }),
        )))
        .await
        .expect("tachi_sandbox(action='get_policy') should succeed");
    let get_json: serde_json::Value =
        serde_json::from_str(&get_resp).expect("get_policy response should be JSON");
    assert_eq!(get_json["capability_id"], json!("mcp:exa"));
    assert_eq!(get_json["max_tool_ms"], json!(7000));
    assert_eq!(get_json["max_concurrency"], json!(2));

    let list_resp = server
        .tachi_sandbox(Parameters(sandbox_params(
            "list_policies",
            json!({
                "enabled_only": true,
                "limit": 10,
            }),
        )))
        .await
        .expect("tachi_sandbox(action='list_policies') should succeed");
    let list_json: serde_json::Value =
        serde_json::from_str(&list_resp).expect("list_policies response should be JSON");
    assert!(
        list_json["count"].as_u64().unwrap_or(0) >= 1,
        "expected at least one policy"
    );
}
