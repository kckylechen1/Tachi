use super::*;

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

#[tokio::test]
async fn rate_limit_session_windows_stay_bounded_when_all_sessions_are_active() {
    let server = make_server();
    {
        let mut guard = server.agent_runtime_write();
        guard.agent_profile = Some(AgentProfile {
            agent_id: "rpm-cap-test".to_string(),
            display_name: "RPM Cap Test".to_string(),
            capabilities: vec![],
            tool_filter: None,
            rate_limit_rpm: Some(100),
            rate_limit_burst: Some(0),
            registered_at: Utc::now().to_rfc3339(),
        });
    }

    for i in 0..(RATE_LIMIT_MAX_SESSIONS + 25) {
        server
            .check_rate_limit("save_memory", &format!("hash-{i}"), &format!("session-{i}"))
            .unwrap_or_else(|e| panic!("active session {i} should be accepted: {e:?}"));
    }

    let (windows, bursts) = server.rate_limiter_entry_counts_for_tests();
    assert_eq!(bursts, 0);
    assert!(
        windows <= RATE_LIMIT_MAX_SESSIONS,
        "session window map should stay capped, got {windows}"
    );
}

#[tokio::test]
async fn rate_limit_burst_keys_stay_bounded_when_all_keys_are_active() {
    let server = make_server();

    for i in 0..(RATE_LIMIT_MAX_BURST_KEYS + 25) {
        server
            .check_rate_limit("save_memory", &format!("hash-{i}"), "session-burst-cap")
            .unwrap_or_else(|e| panic!("unique burst key {i} should be accepted: {e:?}"));
    }

    let (windows, bursts) = server.rate_limiter_entry_counts_for_tests();
    assert_eq!(windows, 0);
    assert!(
        bursts <= RATE_LIMIT_MAX_BURST_KEYS,
        "burst key map should stay capped, got {bursts}"
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
