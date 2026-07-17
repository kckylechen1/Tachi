use super::*;

// tachi#1201 k3: search_memory/tachi_status default `format` to markdown when
// omitted; explicit format="json" must stay byte-for-byte the same JSON shape
// these raw tools always returned before k3 (proven by reusing the exact
// same assertions the pre-k3 tests used, just with `Some("json")` threaded
// through).

#[tokio::test]
async fn tachi_status_agent_defaults_to_markdown_when_format_omitted() {
    let (server, _temp_home) = make_server_with_temp_home();

    let markdown = crate::status_ops::handle_tachi_status_agent(&server, None)
        .await
        .expect("default status should render");
    assert!(
        markdown.starts_with("## Tachi status"),
        "omitted format should render markdown, got: {markdown}"
    );
    assert!(
        serde_json::from_str::<Value>(&markdown).is_err(),
        "markdown digest must not happen to parse as JSON: {markdown}"
    );
    // Spot-check a couple of known top-level keys surface in the digest.
    assert!(markdown.contains("health_score"), "{markdown}");
    assert!(markdown.contains("daemon"), "{markdown}");
}

#[tokio::test]
async fn tachi_status_agent_explicit_json_keeps_pre_k3_shape() {
    let (server, _temp_home) = make_server_with_temp_home();

    let body = crate::status_ops::handle_tachi_status_agent(&server, Some("json"))
        .await
        .expect("agent status should serialize");
    let parsed: Value = serde_json::from_str(&body).expect("explicit json must parse as JSON");
    assert_eq!(parsed["detail"], json!("agent"));
    assert!(parsed["health_score"].is_number());
    assert!(parsed["warnings"].is_array());
}

#[tokio::test]
async fn tachi_status_full_defaults_to_markdown_when_format_omitted() {
    let (server, _temp_home) = make_server_with_temp_home();

    let markdown = crate::status_ops::handle_tachi_status_full(&server, None)
        .await
        .expect("default full status should render");
    assert!(
        markdown.starts_with("## Tachi status"),
        "omitted format should render markdown, got: {markdown}"
    );
    assert!(
        serde_json::from_str::<Value>(&markdown).is_err(),
        "markdown digest must not happen to parse as JSON: {markdown}"
    );
}

#[tokio::test]
async fn tachi_status_full_explicit_json_keeps_pre_k3_shape() {
    let (server, _temp_home) = make_server_with_temp_home();

    let body = crate::status_ops::handle_tachi_status_full(&server, Some("json"))
        .await
        .expect("full status should serialize");
    let parsed: Value = serde_json::from_str(&body).expect("explicit json must parse as JSON");
    assert!(parsed["health_score"].is_number());
    assert!(parsed["databases"].is_object());
    assert!(parsed["warnings"].is_array());
}

#[tokio::test]
async fn tachi_status_format_is_case_insensitive_and_trims_whitespace() {
    let (server, _temp_home) = make_server_with_temp_home();

    for candidate in ["JSON", " json ", "Json"] {
        let body = crate::status_ops::handle_tachi_status_agent(&server, Some(candidate))
            .await
            .unwrap_or_else(|e| panic!("format={candidate:?} should succeed: {e}"));
        let parsed: Value = serde_json::from_str(&body)
            .unwrap_or_else(|e| panic!("format={candidate:?} should parse as JSON: {e}"));
        assert_eq!(parsed["detail"], json!("agent"));
    }

    // Anything else (including a typo) stays on the markdown default.
    let markdown = crate::status_ops::handle_tachi_status_agent(&server, Some("yaml"))
        .await
        .expect("unrecognized format should still succeed");
    assert!(markdown.starts_with("## Tachi status"));
}
