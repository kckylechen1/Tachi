use super::*;

#[tokio::test]
async fn f757_direct_add_edge_call_is_not_registered() {
    // #757: add_edge is internalized — missing from the router for every
    // profile (not merely profile-hidden). Discrimination: call still returns
    // tool-not-found rather than executing graph mutation.
    let server = make_server();
    // Admin profile would have exposed admin-only tools if they were registered.
    server.set_tool_profile(Some(
        tachi_hub::parse_tool_profile("admin").expect("admin profile should parse"),
    ));

    let result = call_tool_via_server(server, "add_edge", None)
        .await
        .expect("unregistered tool should return a tool-level error, not a transport error");

    assert_eq!(result.is_error, Some(true));
    let message = result
        .content
        .first()
        .and_then(|content| content.as_text())
        .map(|text| text.text.as_str())
        .unwrap_or("");
    assert!(
        message.contains("tool not found"),
        "expected tool not found for internalized add_edge, got: {message}"
    );
}

#[tokio::test]
async fn unknown_tool_returns_tool_error_without_crashing_service() {
    let server = make_server();

    let result = call_tool_via_server(server, "tachi_tachi_tournament", None)
        .await
        .expect("unknown tool should not become a JSON-RPC transport error");

    assert_eq!(result.is_error, Some(true));
    let message = result
        .content
        .first()
        .and_then(|content| content.as_text())
        .map(|text| text.text.as_str())
        .unwrap_or("");
    assert!(message.contains("tool not found"));
    assert!(message.contains("tachi_tachi_tournament"));
}

#[tokio::test]
async fn standard_profile_exposes_agent_intents_not_execution_internals() {
    let server = make_server();
    server.set_tool_profile(Some(
        tachi_hub::parse_tool_profile("standard").expect("standard profile should parse"),
    ));

    let tools = server
        .tachi_tools()
        .await
        .expect("tool discovery should work");

    assert!(tools.contains("`tachi_memory`"));
    assert!(tools.contains("`tachi_task`"));
    assert!(!tools.contains("`tachi_arena`"));
    assert!(tools.contains("`tachi_verify`"));
    assert!(tools.contains("`tachi_gh`"));
    assert!(!tools.contains("`tachi_event`"));
    // Harnesses report native lifecycle/eval facts through their adapter
    // protocol. Ordinary agents supply semantic completion/adjudication via
    // the work ledger instead of manually relaying system bookkeeping.
    assert!(!tools.contains("`tachi_agent_eval`"));
    assert!(!tools.contains("`tachi_staff`"));
    assert!(!tools.contains("`tachi_orchestrator`"));
}

#[tokio::test]
async fn coordinate_profile_exposes_advanced_coordination_facades() {
    let server = make_server();
    server.set_tool_profile(Some(
        tachi_hub::parse_tool_profile("coordinate").expect("coordinate profile should parse"),
    ));

    let tools = server
        .tachi_tools()
        .await
        .expect("tool discovery should work");

    assert!(tools.contains("`tachi_arena`"));
    assert!(tools.contains("`tachi_staff`"));
    assert!(tools.contains("`tachi_orchestrator`"));
}

/// #1319-C2: `action='dispatch'` was removed from `tachi_task` wholesale
/// (the worker launch lifecycle left Task). It must now be rejected for a
/// delegate worker at the param-parse layer — no profile, not even admin,
/// can dispatch via `tachi_task`. This supersedes the old #919 operator-only
/// gate (which denied dispatch to non-admin profiles); the action is gone
/// entirely rather than gated. End-to-end via `call_tool` so a future
/// regression that re-adds the variant without re-routing fails loudly.
#[tokio::test]
async fn delegate_tachi_task_dispatch_is_rejected_end_to_end() {
    let server = make_server();
    server.set_tool_profile(Some(
        tachi_hub::parse_tool_profile("delegate").expect("delegate profile should parse"),
    ));

    let mut args = serde_json::Map::new();
    args.insert("action".to_string(), serde_json::json!("dispatch"));
    args.insert("agent".to_string(), serde_json::json!("claude"));
    args.insert(
        "task".to_string(),
        serde_json::json!("recursive dispatch attempt"),
    );

    let result = call_tool_via_server(server, "tachi_task", Some(args))
        .await
        .expect("rejected action should return a tool result, not a transport error");

    assert_eq!(
        result.is_error,
        Some(true),
        "delegate must not be able to dispatch via tachi_task"
    );
    let message = result
        .content
        .first()
        .and_then(|content| content.as_text())
        .map(|text| text.text.as_str())
        .unwrap_or("");
    assert!(
        message.contains("dispatch"),
        "rejection message should name the removed action, got: {message}"
    );
    // Not a "tool not found" — the tool IS visible to delegate; only the
    // dispatch *action* is rejected.
    assert!(!message.contains("tool not found"));
}

/// #1319-C2: dispatch is gone for every profile, including admin. The old
/// operator-only distinction no longer exists for dispatch — it is rejected
/// at the param-parse layer regardless of profile.
#[tokio::test]
async fn standard_tachi_task_dispatch_is_rejected_end_to_end() {
    let server = make_server();
    server.set_tool_profile(Some(
        tachi_hub::parse_tool_profile("standard").expect("standard profile should parse"),
    ));

    let mut args = serde_json::Map::new();
    args.insert("action".to_string(), serde_json::json!("dispatch"));
    args.insert("agent".to_string(), serde_json::json!("opencode"));
    args.insert(
        "dispatch_reason".to_string(),
        serde_json::json!("explicit_user_request"),
    );
    args.insert(
        "task".to_string(),
        serde_json::json!("attempt Tachi-owned launch from a daily agent profile"),
    );

    let result = call_tool_via_server(server, "tachi_task", Some(args))
        .await
        .expect("rejected action should return a tool result");

    assert_eq!(result.is_error, Some(true));
    let message = result
        .content
        .first()
        .and_then(|content| content.as_text())
        .map(|text| text.text.as_str())
        .unwrap_or("");
    assert!(message.contains("dispatch"), "{message}");
}

/// Positive control for the RED test above: the same delegate profile CAN
/// call a permitted `tachi_task` action end-to-end (the gate isn't just
/// denying everything).
#[tokio::test]
async fn delegate_tachi_task_board_is_allowed_end_to_end() {
    let server = make_server();
    server.set_tool_profile(Some(
        tachi_hub::parse_tool_profile("delegate").expect("delegate profile should parse"),
    ));

    let mut args = serde_json::Map::new();
    args.insert("action".to_string(), serde_json::json!("board"));

    let result = call_tool_via_server(server, "tachi_task", Some(args))
        .await
        .expect("permitted action should return a tool result");

    assert_ne!(
        result.is_error,
        Some(true),
        "delegate should be able to call tachi_task(action='board')"
    );
}

#[tokio::test]
async fn delegate_tachi_task_handoff_is_denied_end_to_end() {
    let server = make_server();
    server.set_tool_profile(Some(
        tachi_hub::parse_tool_profile("delegate").expect("delegate profile should parse"),
    ));

    let mut args = serde_json::Map::new();
    args.insert("action".to_string(), serde_json::json!("handoff"));
    args.insert("claim_id".to_string(), serde_json::json!("claim-1"));
    args.insert("transition_version".to_string(), serde_json::json!(0));

    let result = call_tool_via_server(server, "tachi_task", Some(args))
        .await
        .expect("denied action should return a tool result, not a transport error");

    assert_eq!(
        result.is_error,
        Some(true),
        "delegate must not be able to hand off via tachi_task"
    );
    let message = result
        .content
        .first()
        .and_then(|content| content.as_text())
        .map(|text| text.text.as_str())
        .unwrap_or("");
    assert!(
        message.contains("not allowed") || message.contains("not available"),
        "expected a permission-denied message, got: {message}"
    );
    assert!(
        message.contains("handoff"),
        "denial message should name the denied action, got: {message}"
    );
    assert!(!message.contains("tool not found"));
}
