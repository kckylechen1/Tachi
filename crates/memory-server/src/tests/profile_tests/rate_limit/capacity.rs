use super::*;

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
