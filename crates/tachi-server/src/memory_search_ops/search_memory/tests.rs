use serde_json::json;

use super::cache::recall_cache_key;
use super::filters::{project_scope_allows_memory, project_scope_allows_memory_with_config};
use crate::memory_search_ops::routing_config::RoutingConfig;
use crate::tool_params::SearchMemoryParams;

fn entry(domain: Option<&str>, path: &str) -> memcore::MemoryEntry {
    memcore::MemoryEntry {
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
        context_symbols: Vec::new(),
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

fn domain_pack_routing_config() -> RoutingConfig {
    RoutingConfig {
        domain_routes: Vec::new(),
        ticker_route_project: None,
        project_routes: Vec::new(),
        foreign_domain_word_terms: vec!["domainpack".into(), "v8".into()],
        foreign_domain_substring_terms: vec!["领域包".into(), "123456".into()],
        foreign_domains: vec!["domain_pack".into()],
        foreign_path_prefixes: vec!["/domain-pack/".into()],
    }
}

#[test]
fn context_symbols_prefix_plain_queries_for_recall_bias() {
    let query = super::rows::query_with_context_symbols(
        "dispatch smoke failure",
        &["Tachi".to_string(), "tachi-server".to_string()],
    );

    assert_eq!(query, "Tachi memory-server dispatch smoke failure");
}

#[test]
fn context_symbols_do_not_pollute_id_like_exact_queries() {
    let query = super::rows::query_with_context_symbols(
        "RECALL_PROBE_ALPHA_20260607",
        &["Tachi".to_string(), "tachi-server".to_string()],
    );

    assert_eq!(query, "RECALL_PROBE_ALPHA_20260607");
}

#[test]
fn context_symbols_are_deduped_and_not_repeated_when_query_already_mentions_them() {
    let query = super::rows::query_with_context_symbols(
        "Tachi continuity recall",
        &[
            "tachi".to_string(),
            "tachi-server".to_string(),
            "MEMORY-SERVER".to_string(),
            " ".to_string(),
        ],
    );

    assert_eq!(query, "memory-server Tachi continuity recall");
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
    p.domain = Some("domain_pack".into());
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
fn sigil_project_scope_defaults_do_not_special_case_domain_pack_rows() {
    let params = params("Tachi 召回 向量 有没有问题");

    assert!(project_scope_allows_memory(
        "sigil",
        &params,
        &entry(Some("domain_pack"), "/domain-pack/v4")
    ));
    assert!(project_scope_allows_memory(
        "sigil",
        &params,
        &entry(Some("scratch"), "/scratch/sigil/recall")
    ));
}

#[test]
fn sigil_project_scope_filters_configured_foreign_domains_for_default_recall() {
    let params = params("Tachi recall vector issue");
    let config = domain_pack_routing_config();

    assert!(!project_scope_allows_memory_with_config(
        "sigil",
        &params,
        &entry(Some("domain_pack"), "/"),
        &config
    ));
    assert!(!project_scope_allows_memory_with_config(
        "sigil",
        &params,
        &entry(Some("scratch"), "/domain-pack/v4"),
        &config
    ));
    assert!(project_scope_allows_memory_with_config(
        "sigil",
        &params,
        &entry(Some("scratch"), "/scratch/sigil/recall"),
        &config
    ));
}

#[test]
fn sigil_project_scope_allows_configured_foreign_domains_when_query_requests_them() {
    let params = params("DomainPack V8 领域包 recall");
    let config = domain_pack_routing_config();

    assert!(project_scope_allows_memory_with_config(
        "sigil",
        &params,
        &entry(Some("domain_pack"), "/"),
        &config
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
            !project_scope_allows_memory_with_config(
                "sigil",
                &params,
                &entry(Some("domain_pack"), "/"),
                &domain_pack_routing_config()
            ),
            "query {query:?} should not unlock configured foreign-domain memories"
        );
    }
}

#[test]
fn sigil_project_scope_allows_configured_foreign_domains_on_whole_word_match() {
    // Whole-word foreign terms (even without CJK) must still open the gate.
    for query in ["domainpack recall", "v8 engine notes"] {
        let params = params(query);
        assert!(
            project_scope_allows_memory_with_config(
                "sigil",
                &params,
                &entry(Some("domain_pack"), "/"),
                &domain_pack_routing_config()
            ),
            "query {query:?} explicitly names a foreign domain term"
        );
    }
}
