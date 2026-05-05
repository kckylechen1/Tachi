use super::*;

// ─── Handoff Tests ───────────────────────────────────────────────────────────

#[tokio::test]
async fn handoff_leave_and_check_roundtrip() {
    let server = make_server();

    // Register as "agent-a" first
    server
        .agent_register(Parameters(AgentRegisterParams {
            agent_id: "agent-a".to_string(),
            display_name: Some("Agent A".to_string()),
            capabilities: vec![],
            tool_filter: None,
            rate_limit_rpm: None,
            rate_limit_burst: None,
        }))
        .await
        .expect("register agent-a");

    // Leave a handoff memo targeted at agent-b
    let leave = server
        .handoff_leave(Parameters(HandoffLeaveParams {
            summary: "Refactored the auth module, tests still failing on OAuth flow".to_string(),
            next_steps: vec![
                "Fix OAuth callback handler".to_string(),
                "Add integration test for token refresh".to_string(),
            ],
            target_agent: Some("agent-b".to_string()),
            context: Some(json!({"files": ["src/auth.rs", "src/oauth.rs"]})),
        }))
        .await
        .expect("handoff_leave should succeed");
    let leave_json: serde_json::Value = serde_json::from_str(&leave).expect("should be JSON");
    assert_eq!(leave_json["status"], json!("memo_left"));
    assert_eq!(leave_json["from_agent"], json!("agent-a"));
    assert!(leave_json["memo_id"].is_string());

    // Check as agent-b — should see the memo
    let check_b = server
        .handoff_check(Parameters(HandoffCheckParams {
            agent_id: Some("agent-b".to_string()),
            acknowledge: false,
        }))
        .await
        .expect("handoff_check for agent-b");
    let check_b_json: serde_json::Value = serde_json::from_str(&check_b).expect("should be JSON");
    assert_eq!(check_b_json["pending_memos"], json!(1));
    assert_eq!(check_b_json["memos"][0]["from_agent"], json!("agent-a"));

    // Check as agent-c — should NOT see it (targeted at agent-b)
    let check_c = server
        .handoff_check(Parameters(HandoffCheckParams {
            agent_id: Some("agent-c".to_string()),
            acknowledge: false,
        }))
        .await
        .expect("handoff_check for agent-c");
    let check_c_json: serde_json::Value = serde_json::from_str(&check_c).expect("should be JSON");
    assert_eq!(check_c_json["pending_memos"], json!(0));

    // Acknowledge as agent-b
    let ack = server
        .handoff_check(Parameters(HandoffCheckParams {
            agent_id: Some("agent-b".to_string()),
            acknowledge: true,
        }))
        .await
        .expect("handoff_check with acknowledge");
    let ack_json: serde_json::Value = serde_json::from_str(&ack).expect("should be JSON");
    assert_eq!(ack_json["pending_memos"], json!(1)); // Returns memos before acking

    // After acknowledgment, check again — should be empty
    let check_after = server
        .handoff_check(Parameters(HandoffCheckParams {
            agent_id: Some("agent-b".to_string()),
            acknowledge: false,
        }))
        .await
        .expect("handoff_check after ack");
    let check_after_json: serde_json::Value =
        serde_json::from_str(&check_after).expect("should be JSON");
    assert_eq!(check_after_json["pending_memos"], json!(0));
}

#[tokio::test]
async fn handoff_untargeted_memo_visible_to_all() {
    let server = make_server();

    // Leave a memo without a target agent
    server
        .handoff_leave(Parameters(HandoffLeaveParams {
            summary: "Build system needs migration to Bazel".to_string(),
            next_steps: vec!["Read BUILD.bazel files".to_string()],
            target_agent: None,
            context: None,
        }))
        .await
        .expect("handoff_leave should succeed");

    // Any agent should see the untargeted memo
    let check = server
        .handoff_check(Parameters(HandoffCheckParams {
            agent_id: Some("any-agent".to_string()),
            acknowledge: false,
        }))
        .await
        .expect("handoff_check");
    let check_json: serde_json::Value = serde_json::from_str(&check).expect("should be JSON");
    assert_eq!(check_json["pending_memos"], json!(1));
}

#[tokio::test]
async fn handoff_leave_persists_to_memory_store() {
    let server = make_server();

    let leave = server
        .handoff_leave(Parameters(HandoffLeaveParams {
            summary: "Persisted handoff test".to_string(),
            next_steps: vec!["Verify persistence".to_string()],
            target_agent: None,
            context: None,
        }))
        .await
        .expect("handoff_leave should succeed");
    let leave_json: serde_json::Value = serde_json::from_str(&leave).expect("should be JSON");
    let memo_id = leave_json["memo_id"].as_str().unwrap();

    // Verify the memo was persisted to the global memory store
    let memory = server
        .get_memory(Parameters(GetMemoryParams {
            id: format!("handoff:{}", memo_id),
            include_archived: false,
            project: None,
        }))
        .await
        .expect("get_memory for handoff should succeed");
    let mem_json: serde_json::Value = serde_json::from_str(&memory).expect("should be JSON");
    assert_eq!(mem_json["category"], json!("handoff"));
    assert!(
        mem_json["text"]
            .as_str()
            .unwrap_or_default()
            .contains("Persisted handoff test"),
        "persisted memory text should contain the summary"
    );
}
