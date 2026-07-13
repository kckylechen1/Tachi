use super::*;
#[test]
fn parse_session_capture_response_covers_supported_shapes() {
    let cases: [(&str, &str, &[&str]); 4] = [
        (
            "json array",
            r#"[{"text": "hello"}, {"text": "world"}]"#,
            &["hello", "world"],
        ),
        (
            "json code fence",
            "```json\n[{\"text\": \"hello\"}]\n```",
            &["hello"],
        ),
        (
            "reasoning prefix",
            "<think>reasoning that should not be parsed</think>\n[{\"text\": \"hello\"}]",
            &["hello"],
        ),
        (
            "empty drafts filtered",
            r#"[{"text": ""}, {"text": "valid"}]"#,
            &["valid"],
        ),
    ];

    for (name, raw, expected) in cases {
        let drafts = parse_session_capture_response(raw).unwrap();
        let texts: Vec<&str> = drafts.iter().map(|draft| draft.text.as_str()).collect();
        assert_eq!(texts, expected, "case `{name}`");
    }
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
