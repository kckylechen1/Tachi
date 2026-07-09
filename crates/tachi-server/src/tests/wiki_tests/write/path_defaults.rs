use super::*;

#[tokio::test]
async fn tachi_wiki_write_allows_wiki_bucket_without_capture_gate_warning() {
    let server = make_server();

    let response = server
        .tachi_wiki_write(Parameters(WikiWriteParams {
            title: "MCP wiki exposure smoke".to_string(),
            text: "When exposing wiki tools through MCP, keep the lesson under /wiki, attach the correct domain, and write enough concrete detail that the capture gate can stay strict without flagging a valid debugging note. This regression test proves /wiki is a first-class capture bucket now.".to_string(),
            path: None,
            topic: Some("mcp-tool-exposure".to_string()),
            summary: None,
            category: "experience".to_string(),
            keywords: vec!["mcp".to_string(), "wiki".to_string()],
            entities: vec![],
            importance: 0.85,
            scope: "global".to_string(),
            retention_policy: "permanent".to_string(),
            domain: Some("coding".to_string()),
            project: None,
            metadata: None,
            force: false,
            references: vec![],
            include_patterns: false,
            pattern_query: None,
            pattern_top_k: None,
        }))
        .await
        .expect("tachi_wiki_write should succeed");

    let json: Value = serde_json::from_str(&response).expect("wiki write response json");
    assert_eq!(json["wiki_path"], json!("/wiki/general/mcp-tool-exposure"));
    assert!(
        json.get("capture_gate_warnings").is_none(),
        "expected /wiki writes to avoid capture gate warnings, got: {json}"
    );
}

#[tokio::test]
async fn tachi_wiki_write_generates_readable_cjk_path_without_domain_warning() {
    let server = make_server();

    let response = server
        .tachi_wiki_write(Parameters(WikiWriteParams {
            title: "MCP hub_call arguments 丢失：从 schema 层排查".to_string(),
            text: "# MCP hub_call arguments 丢失\n\n## 结论\n\n- 先确认 schema 层是否声明 arguments。\n- 再检查 client serialization 是否丢字段。\n- 最后才看 transport。\n\n这是一条结构化 wiki 经验，正文使用 Markdown 是预期行为，不应该触发 raw markdown dump warning。".to_string(),
            path: None,
            topic: None,
            summary: None,
            category: "experience".to_string(),
            keywords: vec!["mcp".to_string(), "hub_call".to_string()],
            entities: vec![],
            importance: 0.85,
            scope: "global".to_string(),
            retention_policy: "permanent".to_string(),
            domain: None,
            project: None,
            metadata: None,
            force: false,
            references: vec![],
            include_patterns: false,
            pattern_query: None,
            pattern_top_k: None,
        }))
        .await
        .expect("tachi_wiki_write should succeed without explicit domain");

    let json: Value = serde_json::from_str(&response).expect("wiki write response json");
    assert_eq!(
        json["wiki_path"],
        json!("/wiki/general/MCP-hub_call-arguments-丢失-从-schema-层排查")
    );
    assert!(
        json.get("capture_gate_warnings").is_none(),
        "expected wiki write defaults to suppress domain/markdown warnings, got: {json}"
    );
}
