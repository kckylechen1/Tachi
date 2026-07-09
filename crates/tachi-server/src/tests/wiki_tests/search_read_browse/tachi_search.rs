use super::*;

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
            context_symbols: Vec::new(),
            agent_role: None,
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
