use super::*;

#[tokio::test]
async fn agent_register_and_whoami_roundtrip() {
    let server = make_server();

    // Before registering, whoami should return unregistered
    let whoami_before = server
        .agent_whoami(Parameters(AgentWhoamiParams { _placeholder: None }))
        .await
        .expect("agent_whoami should succeed");
    let before_json: serde_json::Value =
        serde_json::from_str(&whoami_before).expect("should be JSON");
    assert_eq!(before_json["status"], json!("unregistered"));

    // Register an agent
    let register = server
        .agent_register(Parameters(AgentRegisterParams {
            agent_id: "claude-code".to_string(),
            display_name: Some("Claude Code".to_string()),
            capabilities: vec!["code-gen".to_string(), "file-edit".to_string()],
            tool_filter: Some(vec!["hub_*".to_string(), "save_memory".to_string()]),
            rate_limit_rpm: Some(120),
            rate_limit_burst: Some(5),
        }))
        .await
        .expect("agent_register should succeed");
    let reg_json: serde_json::Value = serde_json::from_str(&register).expect("should be JSON");
    assert_eq!(reg_json["status"], json!("registered"));
    assert_eq!(reg_json["agent_id"], json!("claude-code"));
    assert_eq!(reg_json["display_name"], json!("Claude Code"));
    assert_eq!(reg_json["rate_limit_rpm"], json!(120));
    assert_eq!(reg_json["rate_limit_burst"], json!(5));

    // After registering, whoami should return the profile
    let whoami_after = server
        .agent_whoami(Parameters(AgentWhoamiParams { _placeholder: None }))
        .await
        .expect("agent_whoami should succeed");
    let after_json: serde_json::Value =
        serde_json::from_str(&whoami_after).expect("should be JSON");
    assert_eq!(after_json["agent_id"], json!("claude-code"));
    assert_eq!(after_json["display_name"], json!("Claude Code"));
    assert_eq!(after_json["capabilities"], json!(["code-gen", "file-edit"]));
}
