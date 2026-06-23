use super::*;
#[test]
fn parse_session_capture_response_accepts_json_array() {
    let raw = r#"[{"text": "hello"}, {"text": "world"}]"#;
    let drafts = parse_session_capture_response(raw).unwrap();
    assert_eq!(drafts.len(), 2);
    assert_eq!(drafts[0].text, "hello");
    assert_eq!(drafts[1].text, "world");
}
#[test]
fn parse_session_capture_response_strips_code_fence() {
    let raw = "```json\n[{\"text\": \"hello\"}]\n```";
    let drafts = parse_session_capture_response(raw).unwrap();
    assert_eq!(drafts.len(), 1);
    assert_eq!(drafts[0].text, "hello");
}
#[test]
fn parse_session_capture_response_ignores_reasoning_prefix() {
    let raw = "<think>reasoning that should not be parsed</think>\n[{\"text\": \"hello\"}]";
    let drafts = parse_session_capture_response(raw).unwrap();
    assert_eq!(drafts.len(), 1);
    assert_eq!(drafts[0].text, "hello");
}
#[test]
fn parse_compact_context_response_ignores_reasoning_prefix() {
    let raw =
        "<think>not json</think>\n{\"compacted_text\":\"summary\",\"salient_topics\":[\"tachi\"]}";
    let draft = parse_compact_context_response(raw).unwrap();
    assert_eq!(draft.compacted_text, "summary");
    assert_eq!(draft.salient_topics, vec!["tachi"]);
}
#[test]
fn capture_session_params_accepts_string_messages() {
    let params: CaptureSessionParams = serde_json::from_value(json!({
        "conversation_id": "c",
        "turn_id": "t",
        "agent_id": "codex-smoke",
        "messages": ["raw user message"]
    }))
    .unwrap();

    assert_eq!(params.messages.len(), 1);
    assert_eq!(params.messages[0].role, "user");
    assert_eq!(params.messages[0].content, "raw user message");
}
#[test]
fn compact_rollup_params_accepts_string_items() {
    let params: CompactRollupParams = serde_json::from_value(json!({
        "agent_id": "codex-smoke",
        "conversation_id": "c",
        "rollup_id": "r",
        "items": ["already compact text"]
    }))
    .unwrap();

    assert_eq!(params.items.len(), 1);
    assert_eq!(params.items[0].compacted_text, "already compact text");
}
#[test]
fn parse_session_capture_response_filters_empty_text() {
    let raw = r#"[{"text": ""}, {"text": "valid"}]"#;
    let drafts = parse_session_capture_response(raw).unwrap();
    assert_eq!(drafts.len(), 1);
    assert_eq!(drafts[0].text, "valid");
}
