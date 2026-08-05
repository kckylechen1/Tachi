use super::*;
use crate::tests::seed_wiki_project_entries;

/// tachi#1561 residual (generic surfaces): `search_memory` has no
/// `is_wiki_row` post-filter — unlike `tachi_search`'s Memory-leg section
/// builder (`facade_search_ops::parse_memory_rows`), it is the raw
/// `handle_search_memory_with_access` surface, so a `/wiki` row's derived
/// lifecycle (`memcore::is_non_default_retrievable_wiki_row`, wired into
/// `search::filtering::is_search_noise_entry` with
/// `bypass_wiki_lifecycle_gate=false` for this call site) is the ONLY thing
/// standing between a `pending_review` draft and this response.
///
/// RED pre-fix: the shared namespace-noise backstop had no lifecycle
/// awareness, so `search_memory(project="wiki", ...)` returned
/// `/wiki/drafts/...` rows verbatim.
#[tokio::test]
async fn search_memory_project_wiki_excludes_pending_review_draft_by_default() {
    let mut active = make_entry("wiki-lifecycle-generic-active");
    active.path = "/wiki/engineering/lifecycle-generic/active".to_string();
    active.summary = "Generic-surface lifecycle gate active entry".to_string();
    active.text = "GenericLifecycleNeedle documents the reviewed active entry.".to_string();
    active.metadata = json!({"lifecycle": "active"});

    let mut draft = make_entry("wiki-lifecycle-generic-draft");
    draft.path = "/wiki/drafts/lifecycle-generic-draft".to_string();
    draft.summary = "Generic-surface lifecycle gate pending draft".to_string();
    draft.text = "GenericLifecycleNeedle documents the unreviewed pending draft.".to_string();
    draft.metadata = json!({"review_status": "pending"});

    let (server, _home) = seed_wiki_project_entries(vec![active, draft]);

    let response = server
        .search_memory(Parameters(SearchMemoryParams {
            query: "GenericLifecycleNeedle".to_string(),
            query_vec: None,
            top_k: 10,
            path_prefix: None,
            include_training: false,
            include_archived: false,
            candidates_per_channel: 20,
            mmr_threshold: None,
            graph_expand_hops: 0,
            graph_relation_filter: None,
            weights: None,
            context_symbols: Vec::new(),
            agent_role: None,
            project: Some("wiki".to_string()),
            domain: None,
            file_context: None,
            error_context: None,
            enable_rerank: false,
            as_of: None,
            include_metadata: false,
            format: None,
        }))
        .await
        .expect("search_memory(project=\"wiki\") should succeed");

    assert!(
        !response.contains("/wiki/drafts/lifecycle-generic-draft"),
        "RED: search_memory(project=\"wiki\") must not surface an unreviewed \
         pending_review draft: {response}"
    );
    assert!(
        response.contains("/wiki/engineering/lifecycle-generic/active"),
        "an active wiki entry must still surface: {response}"
    );
}
