use super::*;

#[tokio::test]
async fn wiki_browse_includes_related_entries_and_logs_operation() {
    let mut alpha = make_entry("wiki-related-alpha");
    alpha.path = "/wiki/engineering/debugging/alpha".to_string();
    alpha.summary = "Alpha debugging".to_string();
    alpha.text = "Alpha debugging lesson for MCP".to_string();
    alpha.entities = vec!["MCP".to_string()];
    alpha.importance = 0.7;

    let mut beta = make_entry("wiki-related-beta");
    beta.path = "/wiki/engineering/debugging/beta".to_string();
    beta.summary = "Beta debugging".to_string();
    beta.text = "Beta debugging lesson for MCP".to_string();
    beta.entities = vec!["MCP".to_string()];
    beta.importance = 0.9;

    let (server, _home) = seed_wiki_project_entries(vec![alpha, beta]);

    let response = server
        .wiki_browse(Parameters(WikiBrowseParams {
            category: Some("engineering/debugging".to_string()),
            limit: 10,
            project: "wiki".to_string(),
        }))
        .await
        .expect("wiki browse should succeed");
    assert!(
        response.contains("/wiki/engineering/debugging/alpha"),
        "browse markdown should contain alpha path"
    );
    assert!(
        response.contains("/wiki/engineering/debugging/beta"),
        "browse markdown should contain beta path"
    );

    let log = server
        .with_named_project_store_read("wiki", |store| {
            store.get("wiki-operation-log").map_err(|e| e.to_string())
        })
        .expect("read wiki log")
        .expect("wiki log should exist");
    assert!(log.text.contains("browse | /wiki/engineering/debugging"));
}

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
        }))
        .await
        .expect("tachi_wiki markdown search should succeed");

    assert!(response.starts_with("## Wiki search:"), "{response}");
    assert!(
        response.contains("MarkdownFacadeWikiNeedle") || response.contains("markdown-search"),
        "{response}"
    );
}

#[tokio::test]
async fn tachi_wiki_read_and_browse_default_to_json() {
    let mut entry = make_entry("wiki-facade-json-read");
    entry.path = "/wiki/engineering/json-read".to_string();
    entry.summary = "JSON wiki read summary".to_string();
    entry.text = "JsonReadFacadeNeedle should be structured.".to_string();
    entry.keywords = vec!["json-read".to_string()];

    let (server, _home) = seed_wiki_project_entries(vec![entry]);

    let read = server
        .tachi_wiki(Parameters(TachiWikiParams {
            action: "read".to_string(),
            format: None,
            query: None,
            category: None,
            top_k: None,
            limit: None,
            title: None,
            text: None,
            path: Some("/wiki/engineering/json-read".to_string()),
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
        }))
        .await
        .expect("tachi_wiki read should succeed");
    let read_json: Value = serde_json::from_str(&read).expect("read JSON");
    assert_eq!(read_json["status"], json!("found"));
    assert_eq!(
        read_json["entry"]["text"],
        json!("JsonReadFacadeNeedle should be structured.")
    );

    let browse = server
        .tachi_wiki(Parameters(TachiWikiParams {
            action: "browse".to_string(),
            format: None,
            query: None,
            category: Some("engineering".to_string()),
            top_k: None,
            limit: Some(10),
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
        }))
        .await
        .expect("tachi_wiki browse should succeed");
    let browse_json: Value = serde_json::from_str(&browse).expect("browse JSON");
    assert_eq!(browse_json["status"], json!("completed"));
    assert_eq!(browse_json["kind"], json!("category"));
    assert!(browse_json["entries"].as_array().is_some_and(|entries| {
        entries
            .iter()
            .any(|entry| entry["path"] == json!("/wiki/engineering/json-read"))
    }));
}

#[tokio::test]
async fn wiki_browse_hides_recall_cache_entries() {
    let mut visible = make_entry("wiki-visible-debugging");
    visible.path = "/wiki/engineering/debugging/visible".to_string();
    visible.summary = "Visible debugging lesson".to_string();
    visible.text = "Visible debugging lesson for wiki browse.".to_string();

    let mut cache = make_entry("wiki-recall-cache-pollution");
    cache.path = "/wiki/engineering/debugging/recall-cache/polluted".to_string();
    cache.summary = "RecallCacheNeedle should stay hidden".to_string();
    cache.text = "RecallCacheNeedle is an ephemeral recall projection.".to_string();
    cache.source = "foundry_recall_rerank_cache".to_string();

    let (server, _home) = seed_wiki_project_entries(vec![visible, cache]);

    let response = server
        .wiki_browse(Parameters(WikiBrowseParams {
            category: Some("engineering/debugging".to_string()),
            limit: 10,
            project: "wiki".to_string(),
        }))
        .await
        .expect("wiki browse should succeed");

    assert!(response.contains("Visible debugging lesson"));
    assert!(!response.contains("RecallCacheNeedle"));
    assert!(!response.contains("recall-cache"));
}

#[tokio::test]
async fn tachi_search_wiki_scope_honors_explicit_project() {
    let mut entry = make_entry("wiki-default-project-search");
    entry.path = "/wiki/engineering/search-default".to_string();
    entry.summary = "Default wiki project search".to_string();
    entry.text = "UniqueDefaultWikiNeedle should be found in the named wiki project.".to_string();
    entry.entities = vec!["UniqueDefaultWikiNeedle".to_string()];

    let (server, _home) = seed_wiki_project_entries(vec![entry]);

    let response = server
        .tachi_search(Parameters(TachiSearchParams {
            query: "UniqueDefaultWikiNeedle".to_string(),
            scope: "wiki".to_string(),
            top_k: 5,
            path_prefix: None,
            project: Some("wiki".to_string()),
            domain: None,
            file_context: None,
            error_context: None,
            category: None,
            include_archived: false,
            include_training: false,
            enable_rerank: false,
            as_of: None,
        }))
        .await
        .expect("tachi_search wiki scope should succeed");

    assert!(
        response.contains("UniqueDefaultWikiNeedle") || response.contains("search-default"),
        "expected explicit project=wiki to search the named wiki DB, got: {response}"
    );
}

#[tokio::test]
async fn wiki_browse_large_limit_keeps_related_entries_empty() {
    let mut alpha = make_entry("wiki-large-limit-alpha");
    alpha.path = "/wiki/engineering/scale/alpha".to_string();
    alpha.summary = "Alpha scale".to_string();
    alpha.text = "Alpha scale lesson for MCP".to_string();
    alpha.entities = vec!["MCP".to_string()];

    let mut beta = make_entry("wiki-large-limit-beta");
    beta.path = "/wiki/engineering/scale/beta".to_string();
    beta.summary = "Beta scale".to_string();
    beta.text = "Beta scale lesson for MCP".to_string();
    beta.entities = vec!["MCP".to_string()];

    let (server, _home) = seed_wiki_project_entries(vec![alpha, beta]);

    let response = server
        .wiki_browse(Parameters(WikiBrowseParams {
            category: Some("engineering/scale".to_string()),
            limit: 21,
            project: "wiki".to_string(),
        }))
        .await
        .expect("wiki browse should succeed");
    assert!(
        response.contains("/wiki/engineering/scale/alpha"),
        "browse markdown should contain alpha path"
    );
    assert!(
        response.contains("/wiki/engineering/scale/beta"),
        "browse markdown should contain beta path"
    );
}
