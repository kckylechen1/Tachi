use super::*;

#[tokio::test]
async fn standard_profile_direct_add_edge_call_is_rejected() {
    let server = make_server();
    server.set_tool_profile(Some(
        crate::profiles::parse_tool_profile("standard").expect("standard profile should parse"),
    ));

    let err = call_tool_via_server(server, "add_edge", None)
        .await
        .expect_err("standard profile should not be able to call add_edge directly");

    assert!(
        err.to_string()
            .to_ascii_lowercase()
            .contains("tool not found"),
        "hidden tool calls should fail like missing tools, got: {err}"
    );
}

#[tokio::test]
async fn runtime_info_reports_identity_and_db_routing() {
    let server = make_server();
    server.set_tool_profile(Some(
        crate::profiles::parse_tool_profile("openclaw").expect("openclaw profile should parse"),
    ));

    let info = server
        .runtime_info()
        .await
        .expect("runtime_info should serialize");
    let value: serde_json::Value = serde_json::from_str(&info).expect("runtime_info JSON");
    assert_eq!(value["runtime"]["name"], json!("tachi"));
    assert_eq!(
        value["runtime"]["version"],
        json!(env!("CARGO_PKG_VERSION"))
    );
    assert_eq!(
        value["runtime"]["tool_profile"],
        json!("observe,remember,operate")
    );
    assert!(value["databases"]["global"]["path"].as_str().is_some());
    assert_eq!(value["databases"]["project"], serde_json::Value::Null);
    assert_eq!(value["databases"]["single_db_mode"], json!(true));
    assert!(value["process"]["pid"].as_u64().is_some());
    assert!(value["process"]["provider_secret_count"].as_u64().is_some());
    assert_eq!(value["process"]["vault"]["unlocked"], json!(false));
}

// ─── Rate Limiter Tests ──────────────────────────────────────────────────────

#[tokio::test]
async fn rate_limit_burst_detection_blocks_identical_calls() {
    let server = make_server();

    // The default burst limit is 8 (DEFAULT_RATE_LIMIT_BURST).
    // Calling check_rate_limit with the same tool+args should succeed 8 times
    // and fail on the 9th.
    for i in 0..8 {
        server
            .check_rate_limit("save_memory", "hash-abc", "session-1")
            .unwrap_or_else(|e| panic!("call {} should succeed: {:?}", i + 1, e));
    }

    let err = server
        .check_rate_limit("save_memory", "hash-abc", "session-1")
        .expect_err("9th identical call should be rate limited");

    assert!(
        err.message.contains("Loop detected"),
        "expected loop detection error, got: {}",
        err.message
    );
    assert!(
        err.message.contains("save_memory"),
        "error should mention the tool name"
    );
}

#[tokio::test]
async fn rate_limit_burst_allows_different_args() {
    let server = make_server();

    // Each unique (tool+args_hash) gets its own burst window
    for i in 0..10 {
        server
            .check_rate_limit("save_memory", &format!("hash-{i}"), "session-1")
            .unwrap_or_else(|e| panic!("call with unique args should succeed: {:?}", e));
    }
}

#[tokio::test]
async fn rate_limit_rpm_blocks_when_exceeded() {
    ensure_test_env();
    let db_path = std::env::temp_dir().join(format!(
        "memory-server-test-rpm-{}.sqlite",
        uuid::Uuid::new_v4()
    ));
    let server = MemoryServer::new(db_path, None).expect("failed to create test server");

    // Override RPM to a very low value for testing.
    // Since rate_limit_rpm is not pub, we use agent profile override instead.
    {
        let mut guard = server.agent_runtime_write();
        guard.agent_profile = Some(AgentProfile {
            agent_id: "rpm-test".to_string(),
            display_name: "RPM Test".to_string(),
            capabilities: vec![],
            tool_filter: None,
            rate_limit_rpm: Some(5),   // Override: only 5 calls/min
            rate_limit_burst: Some(0), // Disable burst detection for this test
            registered_at: Utc::now().to_rfc3339(),
        });
    }

    for i in 0..5 {
        server
            .check_rate_limit(&format!("tool-{i}"), &format!("args-{i}"), "sess-rpm")
            .unwrap_or_else(|e| panic!("call {} should succeed: {:?}", i + 1, e));
    }

    let err = server
        .check_rate_limit("tool-6", "args-6", "sess-rpm")
        .expect_err("6th call should be RPM limited");
    assert!(
        err.message.contains("Rate limited"),
        "expected RPM error, got: {}",
        err.message
    );
}

#[tokio::test]
async fn rate_limit_agent_profile_overrides_server_defaults() {
    let server = make_server();

    // Register an agent with a tight burst limit
    {
        let mut guard = server.agent_runtime_write();
        guard.agent_profile = Some(AgentProfile {
            agent_id: "tight-agent".to_string(),
            display_name: "Tight Agent".to_string(),
            capabilities: vec![],
            tool_filter: None,
            rate_limit_rpm: None,
            rate_limit_burst: Some(3), // Override: only 3 identical calls
            registered_at: Utc::now().to_rfc3339(),
        });
    }

    for i in 0..3 {
        server
            .check_rate_limit("save_memory", "hash-same", "session-prof")
            .unwrap_or_else(|e| panic!("call {} should succeed: {:?}", i + 1, e));
    }

    let err = server
        .check_rate_limit("save_memory", "hash-same", "session-prof")
        .expect_err("4th call should be blocked by agent profile burst limit");
    assert!(err.message.contains("Loop detected"));
}

// ─── Agent Profile Tests ─────────────────────────────────────────────────────

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
