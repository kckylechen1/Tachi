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
    assert!(response.contains("[store: named wiki]"), "{response}");
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

/// tachi#1561 residual (generic surfaces): `tachi_search(scope="all")`
/// combines both legs `collect_tachi_search_sections` builds. The Memory-leg
/// section builder (`facade_search_ops::parse_memory_rows`) already strips
/// every `/wiki`-path row from its output via a pre-existing, path-based
/// `is_wiki_row` filter unrelated to this leaf — so a `/wiki` draft was never
/// independently a "RED" case in the Memory section the way it is for bare
/// `search_memory` (see
/// `memory_tests::search_facade::scope_boundaries::wiki_lifecycle_gate::
/// search_memory_project_wiki_excludes_pending_review_draft_by_default`,
/// which has no such pre-filter). This test still pins the non-leak in both
/// sections of a combined `scope="all"` call, and separately pins that the
/// Wiki leg's own `requested_lifecycle`-scoped gate
/// (`bypass_wiki_lifecycle_gate=true` for `search_wiki_store_candidates`) is
/// unchanged under `scope="all"`, matching the sibling
/// `tachi_search_wiki_scope_excludes_pending_review_drafts_by_default` above.
#[tokio::test]
async fn tachi_search_scope_all_memory_leg_and_wiki_leg_both_exclude_pending_review_draft() {
    let mut active = make_entry("scope-all-active-wiki");
    active.path = "/wiki/engineering/scope-all-lifecycle/active".to_string();
    active.summary = "Scope-all active wiki row".to_string();
    active.text = "ScopeAllLifecycleNeedle documents the reviewed active entry.".to_string();
    active.metadata = json!({"lifecycle": "active"});

    let mut draft = make_entry("scope-all-draft-wiki");
    draft.path = "/wiki/drafts/scope-all-lifecycle-draft".to_string();
    draft.summary = "Scope-all pending draft wiki row".to_string();
    draft.text = "ScopeAllLifecycleNeedle documents the unreviewed pending draft.".to_string();
    draft.metadata = json!({"review_status": "pending"});

    let (server, _home) = seed_wiki_project_entries(vec![active, draft]);

    let (sections, _remapped, _scope) = crate::facade_search_ops::collect_tachi_search_sections(
        &server,
        &TachiSearchParams {
            query: "ScopeAllLifecycleNeedle".to_string(),
            scope: "all".to_string(),
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
        },
    )
    .await;

    let memory_paths: Vec<&str> = sections
        .iter()
        .find(|(name, _)| name == "Memory")
        .and_then(|(_, rows)| rows.as_array())
        .expect("scope=\"all\" must include a Memory section")
        .iter()
        .filter_map(|row| row["path"].as_str())
        .collect();
    assert!(
        !memory_paths.contains(&"/wiki/drafts/scope-all-lifecycle-draft"),
        "the Memory section must not surface the draft: {memory_paths:?}"
    );

    let wiki_paths: Vec<&str> = sections
        .iter()
        .find(|(name, _)| name == "Wiki")
        .and_then(|(_, rows)| rows.as_array())
        .expect("scope=\"all\" must include a Wiki section")
        .iter()
        .filter_map(|row| row["path"].as_str())
        .collect();
    assert!(
        !wiki_paths.contains(&"/wiki/drafts/scope-all-lifecycle-draft"),
        "RED: the Wiki leg's own default-scope draft exclusion must be unchanged \
         under scope=\"all\": {wiki_paths:?}"
    );
    assert!(
        wiki_paths.contains(&"/wiki/engineering/scope-all-lifecycle/active"),
        "the Wiki leg must still surface the active entry under scope=\"all\": {wiki_paths:?}"
    );
}

#[tokio::test]
async fn tachi_search_explicit_project_cannot_relabel_same_id_global_row() {
    let mut named = make_entry("wiki-same-id-boundary");
    named.path = "/wiki/strict/named".to_string();
    named.summary = "Named authority row".to_string();
    named.text = "NamedAuthorityOnlyToken".to_string();
    named.metadata = json!({"lifecycle": "active"});
    let (server, _home) = seed_wiki_project_entries(vec![named]);

    let mut global = make_entry("wiki-same-id-boundary");
    global.path = "/wiki/strict/global-decoy".to_string();
    global.summary = "Global authority decoy".to_string();
    global.text = "WrongStoreSameIdNeedle".to_string();
    global.metadata = json!({"lifecycle": "active"});
    server
        .with_global_store(|store| store.upsert(&global).map_err(|error| error.to_string()))
        .expect("seed global same-id decoy");

    let response = server
        .tachi_search(Parameters(TachiSearchParams {
            query: "WrongStoreSameIdNeedle".to_string(),
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
        .expect("strict tachi_search");

    assert!(
        !response.contains("/wiki/strict/global-decoy")
            && !response.contains("Global authority decoy"),
        "RED: explicit named Wiki search relabeled a same-id global row: {response}"
    );
}
