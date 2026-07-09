use super::*;

#[tokio::test]
async fn rate_limit_rpm_blocks_when_exceeded() {
    let server = make_server();

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
