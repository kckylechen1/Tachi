use serde_json::json;

use super::cache::{
    invalidate_recall_cache_after_write, recall_cache_epoch, recall_cache_generation_fingerprint,
    recall_cache_key as build_recall_cache_key, recall_cache_write_through,
    recall_cache_write_through_is_safe, unique_database_targets, SearchDatabaseTarget,
};
use super::filters::project_scope_allows_memory_with_config;
use crate::memory_search_ops::routing_config::RoutingConfig;
use crate::test_support::EnvRestore;
use crate::tests::make_server;
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
        format: None,
    }
}

fn recall_cache_key(params: &SearchMemoryParams, top_k: usize, project_only: bool) -> String {
    build_recall_cache_key(params, top_k, project_only, false).expect("cache request key")
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

    assert_eq!(query, "Tachi tachi-server dispatch smoke failure");
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
            "TACHI-SERVER".to_string(),
            " ".to_string(),
        ],
    );

    assert_eq!(query, "tachi-server Tachi continuity recall");
}

#[test]
fn recall_cache_key_is_stable_and_preserves_exact_request_values() {
    let a = recall_cache_key(&params("Hello   World"), 5, false);
    let b = recall_cache_key(&params("hello world"), 5, false);
    assert_ne!(a, b, "distinct query bytes must not collapse");
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
    let mut variants = Vec::new();

    let mut changed = base.clone();
    changed.query = "other query".into();
    variants.push(("query", changed));
    let mut changed = base.clone();
    changed.query_vec = Some(vec![0.25]);
    variants.push(("query_vec", changed));
    let mut changed = base.clone();
    changed.top_k = 0;
    variants.push(("top_k_requested_zero", changed));
    let mut changed = base.clone();
    changed.path_prefix = Some(String::new());
    variants.push(("path_prefix_absent_vs_empty", changed));
    let mut changed = base.clone();
    changed.include_training = true;
    variants.push(("include_training", changed));
    let mut changed = base.clone();
    changed.include_archived = true;
    variants.push(("include_archived", changed));
    let mut changed = base.clone();
    changed.candidates_per_channel = 0;
    variants.push(("candidates_per_channel_zero", changed));
    let mut changed = base.clone();
    changed.mmr_threshold = Some(0.0);
    variants.push(("mmr_threshold_absent_vs_zero", changed));
    let mut changed = base.clone();
    changed.graph_expand_hops = 1;
    variants.push(("graph_expand_hops", changed));
    let mut changed = base.clone();
    changed.graph_relation_filter = Some(String::new());
    variants.push(("graph_relation_filter_absent_vs_empty", changed));
    let mut changed = base.clone();
    changed.weights = Some(crate::tool_params::HybridWeightsParam {
        semantic: 0.0,
        fts: 0.0,
        symbolic: 0.0,
        decay: 0.0,
        use_rrf: false,
    });
    variants.push(("weights_absent_vs_zero", changed));
    for (field, weights) in [
        (
            "weights_semantic",
            crate::tool_params::HybridWeightsParam {
                semantic: 0.41,
                fts: 0.3,
                symbolic: 0.2,
                decay: 0.1,
                use_rrf: false,
            },
        ),
        (
            "weights_fts",
            crate::tool_params::HybridWeightsParam {
                semantic: 0.4,
                fts: 0.31,
                symbolic: 0.2,
                decay: 0.1,
                use_rrf: false,
            },
        ),
        (
            "weights_symbolic",
            crate::tool_params::HybridWeightsParam {
                semantic: 0.4,
                fts: 0.3,
                symbolic: 0.21,
                decay: 0.1,
                use_rrf: false,
            },
        ),
        (
            "weights_decay",
            crate::tool_params::HybridWeightsParam {
                semantic: 0.4,
                fts: 0.3,
                symbolic: 0.2,
                decay: 0.11,
                use_rrf: false,
            },
        ),
        (
            "weights_use_rrf",
            crate::tool_params::HybridWeightsParam {
                semantic: 0.4,
                fts: 0.3,
                symbolic: 0.2,
                decay: 0.1,
                use_rrf: true,
            },
        ),
    ] {
        let mut changed = base.clone();
        changed.weights = Some(weights);
        variants.push((field, changed));
    }
    let mut changed = base.clone();
    changed.context_symbols = vec!["Symbol".into()];
    variants.push(("context_symbols", changed));
    let mut changed = base.clone();
    changed.agent_role = Some(String::new());
    variants.push(("agent_role_absent_vs_empty", changed));
    let mut changed = base.clone();
    changed.project = None;
    variants.push(("project", changed));
    let mut changed = base.clone();
    changed.domain = Some(String::new());
    variants.push(("domain_absent_vs_empty", changed));
    let mut changed = base.clone();
    changed.file_context = Some(String::new());
    variants.push(("file_context_absent_vs_empty", changed));
    let mut changed = base.clone();
    changed.error_context = Some(String::new());
    variants.push(("error_context_absent_vs_empty", changed));
    let mut changed = base.clone();
    changed.enable_rerank = true;
    variants.push(("enable_rerank", changed));
    let mut changed = base.clone();
    changed.as_of = Some(String::new());
    variants.push(("as_of_absent_vs_empty", changed));
    let mut changed = base.clone();
    changed.include_metadata = true;
    variants.push(("include_metadata", changed));
    let mut changed = base.clone();
    changed.format = Some(String::new());
    variants.push(("format_absent_vs_empty", changed));

    for (field, changed) in variants {
        assert_ne!(base_key, recall_cache_key(&changed, 5, false), "{field}");
    }
    assert_ne!(
        base_key,
        recall_cache_key(&base, 6, false),
        "top_k_effective"
    );
    assert_ne!(base_key, recall_cache_key(&base, 5, true), "project_only");
    assert_ne!(
        base_key,
        build_recall_cache_key(&base, 5, false, true).expect("pipeline key"),
        "pipeline_enabled"
    );
}

#[test]
fn recall_cache_key_rejects_non_finite_floats() {
    let mut invalid = params("q");
    invalid.mmr_threshold = Some(f64::NAN);
    assert!(build_recall_cache_key(&invalid, 5, false, false).is_err());

    let mut invalid = params("q");
    invalid.query_vec = Some(vec![f32::INFINITY]);
    assert!(build_recall_cache_key(&invalid, 5, false, false).is_err());

    let mut invalid = params("q");
    invalid.weights = Some(crate::tool_params::HybridWeightsParam {
        semantic: 0.4,
        fts: 0.3,
        symbolic: f64::NEG_INFINITY,
        decay: 0.1,
        use_rrf: false,
    });
    assert!(build_recall_cache_key(&invalid, 5, false, false).is_err());
}

#[test]
fn initialized_store_exposes_a_cache_safe_authoritative_generation() {
    let server = make_server();
    let mut search = params("cache generation probe");
    search.project = None;
    let generation = recall_cache_generation_fingerprint(&server, &search, false)
        .expect("freshly initialized store must be cache-safe");
    let generation: serde_json::Value =
        serde_json::from_str(&generation).expect("fingerprint json");
    assert_eq!(generation["databases"].as_array().map(Vec::len), Some(1));
    assert!(generation["databases"][0]["generation"]
        .as_i64()
        .is_some_and(|value| value >= 0));
}

#[test]
fn cache_generation_fingerprint_includes_global_and_bound_project_stores() {
    let temp = tempfile::tempdir().expect("temporary database directory");
    let server = crate::MemoryServer::new(
        temp.path().join("global.sqlite"),
        Some(temp.path().join("project.sqlite")),
    )
    .expect("server with global and bound project stores");
    let mut search = params("cache generation probe");
    search.project = None;

    let generation = recall_cache_generation_fingerprint(&server, &search, false)
        .expect("both selected stores must expose a cache-safe generation");

    let generation: serde_json::Value =
        serde_json::from_str(&generation).expect("fingerprint json");
    assert_eq!(generation["databases"].as_array().map(Vec::len), Some(2));
    assert!(generation["databases"]
        .as_array()
        .expect("databases")
        .iter()
        .all(|db| db["generation"].as_i64().is_some_and(|value| value >= 0)));
}

#[cfg(unix)]
#[test]
fn cache_generation_through_plan_c_alias_tracks_canonical_bound_project_writes() {
    let _guard = crate::utils::global_test_lock()
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    let temp = tempfile::tempdir().expect("temporary database directory");
    let _tachi_home = EnvRestore::set_path("TACHI_HOME", &temp.path().join("home"));
    let _sigil_home = EnvRestore::remove("SIGIL_HOME");
    let _app_home = EnvRestore::remove("TACHI_APP_HOME");

    let global = temp.path().join("global.sqlite");
    let repo = temp.path().join("project");
    let project_db = repo.join(".tachi").join(memcore::MEMORY_DB_FILENAME);
    std::fs::create_dir_all(repo.join(".git")).expect("repository marker");
    let server = crate::MemoryServer::new(global, Some(project_db.clone()))
        .expect("server with canonical bound project database");
    let project_name =
        crate::path_utils::plan_c_dir_name_from_root(&repo).expect("Plan C project identity");
    let alias = crate::path_utils::plan_c_global_db_path(&project_name);
    std::fs::create_dir_all(alias.parent().expect("Plan C alias parent"))
        .expect("create Plan C alias parent");
    std::os::unix::fs::symlink(&project_db, &alias).expect("managed Plan C alias");

    let mut search = params("cache generation probe");
    search.project = Some(project_name);

    let generation_before = recall_cache_generation_fingerprint(&server, &search, false)
        .expect("managed Plan C alias must be cache-safe");
    let generation_before: serde_json::Value =
        serde_json::from_str(&generation_before).expect("fingerprint json");
    assert_eq!(
        generation_before["databases"].as_array().map(Vec::len),
        Some(2),
        "the named Plan C alias and unrelated global store are both selected"
    );
    assert_eq!(
        std::fs::canonicalize(&alias).expect("resolve Plan C alias"),
        std::fs::canonicalize(&project_db).expect("resolve canonical project DB"),
        "only the managed alias may resolve to the canonical project DB"
    );

    server
        .with_project_store(|store| {
            store
                .upsert(&entry(None, "/scratch/plan-c-alias"))
                .map_err(|error| error.to_string())
        })
        .expect("canonical project write");

    let generation_after = recall_cache_generation_fingerprint(&server, &search, false)
        .expect("Plan C alias must observe canonical project generation changes");

    assert_ne!(
        serde_json::to_string(&generation_before).expect("serialize before fingerprint"),
        generation_after,
        "a write through the canonical project DB must invalidate the Plan C alias generation"
    );
}

#[cfg(unix)]
#[test]
fn cache_generation_refuses_direct_project_leaf_symlink_loudly() {
    let temp = tempfile::tempdir().expect("temporary database directory");
    let global = temp.path().join("global.sqlite");
    let _seed = crate::MemoryServer::new(global.clone(), None).expect("seed global database");
    let project_leaf = temp
        .path()
        .join("project")
        .join(".tachi")
        .join(memcore::MEMORY_DB_FILENAME);
    std::fs::create_dir_all(project_leaf.parent().expect("project DB parent"))
        .expect("create project DB parent");
    std::os::unix::fs::symlink(&global, &project_leaf).expect("direct project leaf symlink");

    let error = match crate::MemoryServer::new(global.clone(), Some(project_leaf.clone())) {
        Err(error) => error.to_string(),
        Ok(_) => panic!("direct project DB leaf symlinks must refuse before cache generation"),
    };

    assert!(
        error.contains("project DB path") && error.contains("must not be a symlink"),
        "expected loud direct-project refusal, got: {error}"
    );
    assert!(
        std::fs::symlink_metadata(&project_leaf)
            .expect("direct project leaf preserved")
            .file_type()
            .is_symlink(),
        "refusal must not replace the direct project leaf"
    );
    assert_eq!(
        std::fs::read_link(&project_leaf).expect("direct project leaf target"),
        global,
        "refusal must not follow or retarget the direct project leaf"
    );
}

#[cfg(unix)]
#[test]
fn cache_generation_fingerprint_refuses_hard_link_aliases() {
    let temp = tempfile::tempdir().expect("temporary database directory");
    let global = temp.path().join("global.sqlite");
    let _seed = crate::MemoryServer::new(global.clone(), None).expect("seed database");
    let alias = temp.path().join("project-hard-link.sqlite");
    std::fs::hard_link(&global, &alias).expect("database hard link");

    let error = unique_database_targets(vec![
        SearchDatabaseTarget::Global(global),
        SearchDatabaseTarget::BoundProject(alias),
    ])
    .expect_err("hard-link aliases must bypass recall cache validation");

    assert!(
        error.contains("hard-link alias") && error.contains("recall cache is bypassed"),
        "unexpected hard-link safety error: {error}"
    );
}

#[test]
fn sigil_project_scope_defaults_do_not_special_case_domain_pack_rows() {
    let params = params("Tachi 召回 向量 有没有问题");

    assert!(project_scope_allows_memory_with_config(
        "sigil",
        &params,
        &entry(Some("domain_pack"), "/domain-pack/v4"),
        &RoutingConfig::default(),
    ));
    assert!(project_scope_allows_memory_with_config(
        "sigil",
        &params,
        &entry(Some("scratch"), "/scratch/sigil/recall"),
        &RoutingConfig::default(),
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

// ── T8: epoch guard against the miss-compute/save race (tachi#1435 slice 4
// / #2059 codex round 2, "tooth 2" — BUG, race) ─────────────────────────────
//
// Full end-to-end interleaving (miss-search snapshots epoch → concurrent
// save commits + invalidates → miss-search's write-through lands) is not
// unit-testable without refactoring `handle_search_memory_with_access` to
// take an injectable pause point mid-function — that function has no such
// seam today and adding one purely for a test would be its own scope
// creep. Per the frozen spec's fallback, this downgrades to a direct test
// of the extracted epoch-guard primitive `handlers.rs` actually calls
// (`recall_cache_write_through_is_safe`), driven through the real
// `invalidate_recall_cache_after_write` choke point (not a hand-rolled
// epoch bump) so it exercises the identical bump-on-success path a real
// save/enrichment/contradiction takes.
//
// Pre-fix RED: comment out (or `if false &&`-gate) the
// `RECALL_CACHE_EPOCH.fetch_add` line inside `invalidate_recall_cache_after_write`
// and rerun `write_through_is_rejected_after_a_concurrent_invalidation` alone
// — it goes red because a snapshot taken before the invalidation would still
// read as "safe" after it.

#[test]
fn write_through_is_safe_when_no_invalidation_landed_since_snapshot() {
    let _lock = crate::utils::global_test_lock()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let epoch_at_read = recall_cache_epoch();
    assert!(
        recall_cache_write_through_is_safe(epoch_at_read),
        "no concurrent invalidation happened since the snapshot; write-through must proceed"
    );
}

#[test]
fn write_through_is_rejected_after_a_concurrent_invalidation_bumped_the_epoch() {
    let _lock = crate::utils::global_test_lock()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let _flag = EnvRestore::set("TACHI_ENABLE_RECALL_CACHE", "true");
    let server = make_server();

    // Snapshot BEFORE any store work — exactly what
    // `handle_search_memory_with_access` does before its miss-path compute.
    let epoch_at_read = recall_cache_epoch();

    // Simulate a concurrent save/enrichment/contradiction committing and
    // invalidating mid-flight, through the real choke point (not a hand-rolled
    // counter bump).
    let fence = invalidate_recall_cache_after_write(&server, "t8-simulated-concurrent-writer");
    assert_eq!(
        fence, "cleared",
        "invalidation must have actually run and bumped the epoch"
    );

    assert!(
        !recall_cache_write_through_is_safe(epoch_at_read),
        "a write-through snapshotted BEFORE a concurrent invalidation must be rejected — \
         committing it now would resurrect exactly the stale content the invalidation cleared"
    );

    // Sanity: a snapshot taken AFTER the invalidation is still safe (the
    // guard isn't just permanently false).
    let epoch_after_invalidation = recall_cache_epoch();
    assert!(
        recall_cache_write_through_is_safe(epoch_after_invalidation),
        "a fresh snapshot taken after the invalidation must still be able to write through"
    );
}

/// tachi#1435 slice 6 / #2059 codex round 3, "tooth B" (TOCTOU fix,
/// deterministic unit test): drives the ACTUAL production write-through
/// function (`recall_cache_write_through` — the same one
/// `handle_search_memory_with_access` calls) instead of the raw
/// check-then-write split `write_through_is_rejected_after_a_concurrent_invalidation_bumped_the_epoch`
/// above exercises. Proves the in-lock recheck rejects a write snapshotted
/// before an invalidation that has ALREADY completed by the time the write
/// attempt runs — no row lands, and the recall_cache table stays empty.
///
/// The TOCTOU itself (a window between an out-of-lock check and an in-lock
/// write) is closed structurally by folding both operations into the same
/// `with_global_store` critical section (see `search_memory::cache`'s
/// module doc for the two-ordering proof) — that structural guarantee isn't
/// something a single-threaded test can exercise by racing two real
/// threads without reintroducing timing flakiness; this test instead pins
/// down the deterministic behavior of the in-lock recheck function itself,
/// which is the piece the structural fix depends on behaving correctly.
#[test]
fn locked_write_through_rejects_a_write_snapshotted_before_invalidation_completed() {
    let _lock = crate::utils::global_test_lock()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let _flag = EnvRestore::set("TACHI_ENABLE_RECALL_CACHE", "true");
    let server = make_server();

    // Snapshot BEFORE any store work, exactly as
    // `handle_search_memory_with_access` does before its miss-path compute.
    let epoch_at_read = recall_cache_epoch();

    // Invalidation completes in full (DELETE + bump, one atomic unit) before
    // the write-through attempt below ever runs — the straightforward,
    // already-completed-by-the-time-we-write case the mutual exclusion must
    // get right.
    let fence = invalidate_recall_cache_after_write(&server, "t8-locked-write-through-red-proof");
    assert_eq!(
        fence, "cleared",
        "invalidation must have actually run and bumped the epoch"
    );

    let wrote = recall_cache_write_through(
        &server,
        epoch_at_read,
        "rc:t8-locked-probe",
        "test-generation",
        "t8 locked probe query",
        "[{\"id\":\"stale-should-not-land\"}]",
        1,
        false,
    )
    .expect("write_through call must not itself error");
    assert!(
        !wrote,
        "a write-through snapshotted before an already-completed invalidation \
         must be rejected by the in-lock recheck, not silently write stale rows"
    );

    let entries = server
        .with_global_store_read(|store| store.recall_cache_stats().map_err(|e| e.to_string()))
        .expect("stats after rejected write-through")
        .entries;
    assert_eq!(
        entries, 0,
        "the rejected write-through must not have landed any row in recall_cache"
    );
}
