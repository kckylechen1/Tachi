use serde_json::json;

use super::cache::recall_cache_key;
use super::filters::project_scope_allows_memory;
use crate::tool_params::SearchMemoryParams;

fn entry(domain: Option<&str>, path: &str) -> memory_core::MemoryEntry {
    memory_core::MemoryEntry {
        id: "id".into(),
        path: path.into(),
        summary: "summary".into(),
        text: "text".into(),
        importance: 0.7,
        timestamp: chrono::Utc::now().to_rfc3339(),
        valid_from: String::new(),
        valid_until: None,
        category: "fact".into(),
        topic: String::new(),
        keywords: vec![],
        persons: vec![],
        entities: vec![],
        location: String::new(),
        source: "test".into(),
        scope: "project".into(),
        archived: false,
        access_count: 0,
        last_access: None,
        revision: 1,
        metadata: json!({}),
        vector: None,
        retention_policy: None,
        domain: domain.map(str::to_string),
        recall_count: 0,
        query_diversity: 0,
        tier: "raw".into(),
    }
}

fn params(query: &str) -> SearchMemoryParams {
    SearchMemoryParams {
        query: query.into(),
        query_vec: None,
        top_k: 5,
        path_prefix: None,
        include_training: false,
        include_archived: false,
        candidates_per_channel: 5,
        mmr_threshold: None,
        graph_expand_hops: 0,
        graph_relation_filter: None,
        weights: None,
        agent_role: None,
        project: Some("sigil".into()),
        domain: None,
        file_context: None,
        error_context: None,
        enable_rerank: false,
        as_of: None,
        include_metadata: false,
    }
}

#[test]
fn recall_cache_key_is_stable_and_normalizes_query() {
    let a = recall_cache_key(&params("Hello   World"), 5, false);
    let b = recall_cache_key(&params("hello world"), 5, false);
    assert_eq!(a, b, "case + collapsed whitespace map to the same key");
    assert_eq!(
        a,
        recall_cache_key(&params("Hello   World"), 5, false),
        "key is deterministic"
    );
}

#[test]
fn recall_cache_key_separates_result_affecting_fields() {
    let base = params("same query");
    let base_key = recall_cache_key(&base, 5, false);

    assert_ne!(
        base_key,
        recall_cache_key(&params("other query"), 5, false),
        "query"
    );
    assert_ne!(base_key, recall_cache_key(&base, 6, false), "top_k");
    assert_ne!(base_key, recall_cache_key(&base, 5, true), "project_only");

    let mut p = base.clone();
    p.path_prefix = Some("/wiki".into());
    assert_ne!(base_key, recall_cache_key(&p, 5, false), "path_prefix");

    let mut p = base.clone();
    p.project = Some("other".into());
    assert_ne!(base_key, recall_cache_key(&p, 5, false), "project");

    let mut p = base.clone();
    p.domain = Some("finance".into());
    assert_ne!(base_key, recall_cache_key(&p, 5, false), "domain");

    let mut p = base.clone();
    p.agent_role = Some("reader".into());
    assert_ne!(base_key, recall_cache_key(&p, 5, false), "agent_role");

    let mut p = base.clone();
    p.include_metadata = true;
    assert_ne!(base_key, recall_cache_key(&p, 5, false), "include_metadata");

    let mut p = base.clone();
    p.include_training = true;
    assert_ne!(base_key, recall_cache_key(&p, 5, false), "include_training");

    let mut p = base.clone();
    p.include_archived = true;
    assert_ne!(base_key, recall_cache_key(&p, 5, false), "include_archived");
}

#[test]
fn recall_cache_key_ignores_rerank_intent() {
    // enable_rerank is intentionally NOT part of the key — the background
    // rerank job upgrades the same entry, and rerank intent is reconciled
    // against the stored `reranked` flag at read time.
    let mut a = params("q");
    a.enable_rerank = false;
    let mut b = params("q");
    b.enable_rerank = true;
    assert_eq!(
        recall_cache_key(&a, 5, false),
        recall_cache_key(&b, 5, false)
    );
}

#[test]
fn sigil_project_scope_filters_foreign_domains_for_default_recall() {
    let params = params("Tachi 召回 向量 有没有问题");

    assert!(!project_scope_allows_memory(
        "sigil",
        &params,
        &entry(Some("equity_trading"), "/")
    ));
    assert!(!project_scope_allows_memory(
        "sigil",
        &params,
        &entry(Some("hyperion"), "/scratch/hyperion/v4")
    ));
    assert!(project_scope_allows_memory(
        "sigil",
        &params,
        &entry(Some("scratch"), "/scratch/sigil/recall")
    ));
}

#[test]
fn sigil_project_scope_allows_foreign_domains_when_query_requests_them() {
    let params = params("Hyperion V8 股票召回");

    assert!(project_scope_allows_memory(
        "sigil",
        &params,
        &entry(Some("equity_trading"), "/")
    ));
}

#[test]
fn sigil_project_scope_keeps_filtering_when_query_only_substring_matches_terms() {
    // "change"/"channel"/"quantity" must NOT trip "chan"/"quant"; the
    // foreign-domain filter has to stay active for ordinary coding queries.
    for query in [
        "refactor the channel change handler",
        "compute the quantity of pending jobs",
        "address inequity in scheduling",
    ] {
        let params = params(query);
        assert!(
            !project_scope_allows_memory("sigil", &params, &entry(Some("equity_trading"), "/")),
            "query {query:?} should not unlock foreign trading memories"
        );
    }
}

#[test]
fn sigil_project_scope_allows_foreign_domains_on_whole_word_match() {
    // Whole-word foreign terms (even without CJK) must still open the gate.
    for query in ["quant trading recall", "v8 engine notes", "chan pump-fake"] {
        let params = params(query);
        assert!(
            project_scope_allows_memory("sigil", &params, &entry(Some("equity_trading"), "/")),
            "query {query:?} explicitly names a foreign domain term"
        );
    }
}
