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

/// Cross-vendor review (#1215 BUG 4): "generic search... bypass the new
/// gate" — this Wiki section (`facade_search_ops::collect_tachi_search_sections`)
/// feeds BOTH `tachi_search` and `tachi_memory(action='ask')`'s LLM
/// synthesis. An unreviewed `pending_review` draft must never surface here
/// (it would let `ask` cite unreviewed model output as truth), while an
/// `active` entry with the same needle still must.
#[tokio::test]
async fn tachi_search_wiki_scope_excludes_pending_review_drafts_by_default() {
    let mut active = make_entry("wiki-search-gate-active");
    active.path = "/wiki/engineering/search-gate/active".to_string();
    active.summary = "Search gate active entry".to_string();
    active.text = "SearchGateNeedle documents the reviewed active entry.".to_string();
    active.metadata = json!({"lifecycle": "active"});

    let mut draft = make_entry("wiki-search-gate-draft");
    draft.path = "/wiki/drafts/search-gate-draft".to_string();
    draft.summary = "Search gate pending draft".to_string();
    draft.text = "SearchGateNeedle documents the unreviewed pending draft.".to_string();
    draft.metadata = json!({"review_status": "pending"});

    let (server, _home) = seed_wiki_project_entries(vec![active, draft]);

    let response = server
        .tachi_search(Parameters(TachiSearchParams {
            query: "SearchGateNeedle".to_string(),
            scope: "wiki".to_string(),
            top_k: 10,
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
        !response.contains("/wiki/drafts/search-gate-draft"),
        "RED: an unreviewed draft must not surface through generic tachi_search: {response}"
    );
    assert!(
        response.contains("/wiki/engineering/search-gate/active")
            || response.contains("SearchGateNeedle"),
        "an active entry must still surface: {response}"
    );
}
