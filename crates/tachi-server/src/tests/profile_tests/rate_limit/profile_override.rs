use super::*;

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
