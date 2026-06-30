use super::*;

#[tokio::test]
async fn standard_profile_direct_add_edge_call_is_rejected() {
    let server = make_server();
    server.set_tool_profile(Some(
        crate::profiles::parse_tool_profile("standard").expect("standard profile should parse"),
    ));

    let result = call_tool_via_server(server, "add_edge", None)
        .await
        .expect("hidden tool should return a tool-level error, not a transport error");

    assert_eq!(result.is_error, Some(true));
    let message = result
        .content
        .first()
        .and_then(|content| content.as_text())
        .map(|text| text.text.as_str())
        .unwrap_or("");
    assert!(message.contains("tool not found"));
    assert!(message.contains("tachi_tools"));
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
async fn standard_profile_exposes_arena_but_hides_heavy_coordination_facades() {
    let server = make_server();
    server.set_tool_profile(Some(
        crate::profiles::parse_tool_profile("standard").expect("standard profile should parse"),
    ));

    let tools = server
        .tachi_tools()
        .await
        .expect("tool discovery should work");

    assert!(tools.contains("`tachi_memory`"));
    assert!(tools.contains("`tachi_task`"));
    assert!(tools.contains("`tachi_arena`"));
    assert!(tools.contains("`tachi_verify`"));
    assert!(tools.contains("`tachi_gh`"));
    assert!(!tools.contains("`tachi_event`"));
    assert!(!tools.contains("`tachi_agent_eval`"));
    assert!(!tools.contains("`tachi_shell`"));
    assert!(!tools.contains("`tachi_orchestrator`"));
}

#[tokio::test]
async fn coordinate_profile_exposes_advanced_coordination_facades() {
    let server = make_server();
    server.set_tool_profile(Some(
        crate::profiles::parse_tool_profile("coordinate").expect("coordinate profile should parse"),
    ));

    let tools = server
        .tachi_tools()
        .await
        .expect("tool discovery should work");

    assert!(tools.contains("`tachi_arena`"));
    assert!(tools.contains("`tachi_shell`"));
    assert!(tools.contains("`tachi_orchestrator`"));
}
