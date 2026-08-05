//! tachi#1561 residual (generic surfaces) discriminators for `list_memories`
//! and the mixed-population non-regression case, mirroring the fixture idiom
//! `wiki_tests::search_read_browse::tachi_search::
//! tachi_search_wiki_scope_excludes_pending_review_drafts_by_default` uses:
//! an `active` `/wiki` row plus a `pending_review` `/wiki/drafts/...` row
//! (`review_status: "pending"`, the real marker
//! `foundry_runtime_ops::wiki_evolver::save_wiki_draft` writes).
//!
//! `list_memories` has no `requested_lifecycle` escape hatch of its own (see
//! `memory_ops::is_listable_row`'s doc comment), so unlike the Wiki search
//! leg the exclusion here is unconditional — there is no explicit-scope case
//! to also exercise.

use super::*;

/// tachi#1561 residual: `list_memories(path_prefix="/wiki")` routed straight
/// through `is_listable_row`, which (pre-fix) only knew about
/// internal-bookkeeping noise (`is_namespace_search_noise`), not Wiki
/// lifecycle. RED pre-fix: a `pending_review` draft under `/wiki/drafts/...`
/// listed verbatim alongside reviewed content.
#[tokio::test]
async fn list_memories_wiki_prefix_excludes_pending_review_draft_by_default() {
    let server = make_server();
    server
        .with_global_store(|store| {
            let mut active = make_entry("wiki-listing-lifecycle-active");
            active.path = "/wiki/engineering/listing-lifecycle/active".to_string();
            active.text = "ListingLifecycleNeedle active entry body.".to_string();
            active.summary = "Listing lifecycle active row".to_string();
            active.metadata = json!({"lifecycle": "active"});
            store.upsert(&active).map_err(|e| e.to_string())?;

            let mut draft = make_entry("wiki-listing-lifecycle-draft");
            draft.path = "/wiki/drafts/listing-lifecycle-draft".to_string();
            draft.text = "ListingLifecycleNeedle draft entry body.".to_string();
            draft.summary = "Listing lifecycle pending draft row".to_string();
            draft.metadata = json!({"review_status": "pending"});
            store.upsert(&draft).map_err(|e| e.to_string())
        })
        .expect("seed listing lifecycle fixtures");

    let response = crate::memory_ops::handle_list_memories(
        &server,
        ListMemoriesParams {
            path_prefix: "/wiki".to_string(),
            limit: 50,
            include_archived: false,
            project: None,
        },
    )
    .await
    .expect("list under /wiki");
    let rows: Vec<Value> = serde_json::from_str(&response).expect("list JSON");
    let ids = rows
        .iter()
        .map(|row| row["id"].as_str().unwrap_or_default().to_string())
        .collect::<Vec<_>>();
    assert!(
        ids.iter().any(|id| id == "wiki-listing-lifecycle-active"),
        "active entry must still list: {rows:#?}"
    );
    assert!(
        !ids.iter().any(|id| id == "wiki-listing-lifecycle-draft"),
        "RED: pending_review draft leaked through list_memories: {rows:#?}"
    );
}

/// tachi#1561 residual: mixed-population non-regression. A non-wiki row and
/// an active `/wiki` row must both survive the new lifecycle backstop
/// unaffected, on the SAME population that also carries a draft — pinned by
/// exact id sets (not merely "not empty") on both `search_memory` and
/// `list_memories`, so a filter that also drops unrelated rows (or fails to
/// drop the draft) cannot pass either assertion.
#[tokio::test]
async fn wiki_lifecycle_gate_leaves_non_wiki_and_active_rows_unaffected_across_surfaces() {
    let server = make_server();
    server
        .with_global_store(|store| {
            let mut plain = make_entry("mixed-population-plain-memory");
            plain.path = "/facts/mixed-population".to_string();
            plain.text = "MixedPopulationLifecycleNeedle plain memory row.".to_string();
            plain.summary = "Mixed population plain row".to_string();
            store.upsert(&plain).map_err(|e| e.to_string())?;

            let mut active = make_entry("mixed-population-active-wiki");
            active.path = "/wiki/engineering/mixed-population/active".to_string();
            active.text = "MixedPopulationLifecycleNeedle active wiki row.".to_string();
            active.summary = "Mixed population active wiki row".to_string();
            active.metadata = json!({"lifecycle": "active"});
            store.upsert(&active).map_err(|e| e.to_string())?;

            let mut draft = make_entry("mixed-population-draft-wiki");
            draft.path = "/wiki/drafts/mixed-population-draft".to_string();
            draft.text = "MixedPopulationLifecycleNeedle draft wiki row.".to_string();
            draft.summary = "Mixed population pending draft wiki row".to_string();
            draft.metadata = json!({"review_status": "pending"});
            store.upsert(&draft).map_err(|e| e.to_string())
        })
        .expect("seed mixed population fixtures");

    // search_memory (global, no path_prefix restriction): exactly the plain
    // row and the active wiki row; the draft excluded.
    let search_response = server
        .search_memory(Parameters(SearchMemoryParams {
            query: "MixedPopulationLifecycleNeedle".to_string(),
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
            project: None,
            domain: None,
            file_context: None,
            error_context: None,
            enable_rerank: false,
            as_of: None,
            include_metadata: false,
            format: Some("json".to_string()),
        }))
        .await
        .expect("search_memory over the mixed population");
    let search_rows: Vec<Value> = serde_json::from_str(&search_response).expect("search JSON");
    let search_ids: std::collections::BTreeSet<String> = search_rows
        .iter()
        .map(|row| row["id"].as_str().unwrap_or_default().to_string())
        .collect();
    assert_eq!(
        search_ids,
        std::collections::BTreeSet::from([
            "mixed-population-plain-memory".to_string(),
            "mixed-population-active-wiki".to_string(),
        ]),
        "search_memory must return exactly the plain row and the active wiki row, \
         no more and no less: {search_rows:#?}"
    );

    // list_memories(path_prefix="/"): same exact pair, draft still excluded.
    let list_response = crate::memory_ops::handle_list_memories(
        &server,
        ListMemoriesParams {
            path_prefix: "/".to_string(),
            limit: 50,
            include_archived: false,
            project: None,
        },
    )
    .await
    .expect("list_memories over the mixed population");
    let list_rows: Vec<Value> = serde_json::from_str(&list_response).expect("list JSON");
    let list_ids: std::collections::BTreeSet<String> = list_rows
        .iter()
        .map(|row| row["id"].as_str().unwrap_or_default().to_string())
        .collect();
    assert_eq!(
        list_ids,
        std::collections::BTreeSet::from([
            "mixed-population-plain-memory".to_string(),
            "mixed-population-active-wiki".to_string(),
        ]),
        "list_memories must return exactly the plain row and the active wiki row, \
         no more and no less: {list_rows:#?}"
    );
}
