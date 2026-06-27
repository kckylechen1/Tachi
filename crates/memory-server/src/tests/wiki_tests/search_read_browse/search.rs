use super::*;

#[tokio::test]
async fn wiki_search_returns_compact_hits_without_related_entries() {
    let mut alpha = make_entry("wiki-search-alpha");
    alpha.path = "/wiki/engineering/debugging/search-alpha".to_string();
    alpha.summary = "MCP schema debugging".to_string();
    alpha.text = "MCP schema debugging requires checking serialization.".to_string();
    alpha.entities = vec!["MCP".to_string()];

    let mut beta = make_entry("wiki-search-beta");
    beta.path = "/wiki/engineering/debugging/search-beta".to_string();
    beta.summary = "MCP transport debugging".to_string();
    beta.text = "MCP transport debugging should come after schema checks.".to_string();
    beta.entities = vec!["MCP".to_string()];

    let (server, _home) = seed_wiki_project_entries(vec![alpha, beta]);

    let response = server
        .wiki_search(Parameters(WikiSearchParams {
            query: "MCP debugging".to_string(),
            path_prefix: Some("/wiki".to_string()),
            category: None,
            top_k: 5,
            include_archived: false,
            agent_role: None,
            project: Some("wiki".to_string()),
            domain: None,
            file_context: None,
            error_context: None,
            weights: None,
        }))
        .await
        .expect("wiki search should succeed");
    assert!(response.starts_with("## Wiki search:"));
    assert!(
        response.contains("MCP schema debugging") || response.contains("MCP transport debugging")
    );
    assert!(!response.contains("merge_hints"));
}

#[tokio::test]
async fn tachi_wiki_search_defaults_to_named_wiki_project() {
    let mut entry = make_entry("wiki-facade-default-project-search");
    entry.path = "/wiki/agent/tachi/default-search".to_string();
    entry.summary = "Default facade wiki search".to_string();
    entry.text =
        "DefaultFacadeWikiNeedle should be found without passing project=wiki.".to_string();
    entry.entities = vec!["DefaultFacadeWikiNeedle".to_string()];

    let (server, _home) = seed_wiki_project_entries(vec![entry]);

    let response = server
        .tachi_wiki(Parameters(TachiWikiParams {
            action: "search".to_string(),
            format: None,
            query: Some("DefaultFacadeWikiNeedle".to_string()),
            category: None,
            top_k: Some(5),
            limit: None,
            title: None,
            text: None,
            path: None,
            topic: None,
            summary: None,
            keywords: Vec::new(),
            entities: Vec::new(),
            references: Vec::new(),
            importance: None,
            scope: None,
            project: None,
            domain: None,
            metadata: None,
            force: false,
            include_patterns: false,
            pattern_query: None,
            pattern_top_k: None,
        }))
        .await
        .expect("tachi_wiki search should succeed");

    let parsed: Value = serde_json::from_str(&response).expect("default tachi_wiki search JSON");
    assert_eq!(parsed["status"], json!("completed"));
    assert!(
        parsed.to_string().contains("DefaultFacadeWikiNeedle")
            || parsed.to_string().contains("default-search"),
        "expected default tachi_wiki search to include project:wiki hits, got: {response}"
    );
}

#[tokio::test]
async fn tachi_wiki_search_supports_explicit_markdown_format() {
    let mut entry = make_entry("wiki-facade-markdown-search");
    entry.path = "/wiki/agent/tachi/markdown-search".to_string();
    entry.summary = "Markdown facade wiki search".to_string();
    entry.text = "MarkdownFacadeWikiNeedle should render as markdown.".to_string();
    entry.entities = vec!["MarkdownFacadeWikiNeedle".to_string()];

    let (server, _home) = seed_wiki_project_entries(vec![entry]);

    let response = server
        .tachi_wiki(Parameters(TachiWikiParams {
            action: "search".to_string(),
            format: Some("markdown".to_string()),
            query: Some("MarkdownFacadeWikiNeedle".to_string()),
            category: None,
            top_k: Some(5),
            limit: None,
            title: None,
            text: None,
            path: None,
            topic: None,
            summary: None,
            keywords: Vec::new(),
            entities: Vec::new(),
            references: Vec::new(),
            importance: None,
            scope: None,
            project: None,
            domain: None,
            metadata: None,
            force: false,
            include_patterns: false,
            pattern_query: None,
            pattern_top_k: None,
        }))
        .await
        .expect("tachi_wiki markdown search should succeed");

    assert!(response.starts_with("## Wiki search:"), "{response}");
    assert!(
        response.contains("MarkdownFacadeWikiNeedle") || response.contains("markdown-search"),
        "{response}"
    );
}
