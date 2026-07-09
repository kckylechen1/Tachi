use super::*;

#[tokio::test]
async fn tachi_wiki_write_supports_explicit_markdown_format() {
    let server = make_server();

    let response = server
        .tachi_wiki(Parameters(TachiWikiParams {
            action: "write".to_string(),
            format: Some("markdown".to_string()),
            query: None,
            category: Some("experience".to_string()),
            top_k: None,
            limit: None,
            title: Some("Facade wiki markdown write".to_string()),
            text: Some("Facade wiki write should still support human-readable output.".to_string()),
            path: Some("/wiki/agent/tachi/facade-markdown-write".to_string()),
            topic: Some("facade-markdown-write".to_string()),
            summary: Some("Facade wiki markdown write".to_string()),
            keywords: Vec::new(),
            entities: Vec::new(),
            references: Vec::new(),
            importance: None,
            scope: None,
            project: None,
            domain: Some("engineering".to_string()),
            metadata: None,
            force: true,
            include_patterns: false,
            pattern_query: None,
            pattern_top_k: None,
        }))
        .await
        .expect("tachi_wiki markdown write should succeed");

    assert!(response.starts_with("## Tachi wiki write"), "{response}");
    assert!(response.contains("path:"), "{response}");
    assert!(
        response.contains("/wiki/agent/tachi/facade-markdown-write"),
        "{response}"
    );
}
