use super::*;
use crate::db::{
    add_edge, init_schema, register_sqlite_vec, search_fts, try_load_sqlite_vec, upsert,
};
use crate::types::{MemoryEdge, MemoryEntry};
use chrono::Utc;
use rusqlite::Connection;
use serde_json::json;

fn setup() -> Connection {
    libsimple::enable_auto_extension().unwrap();
    register_sqlite_vec();
    let conn = Connection::open_in_memory().unwrap();
    init_schema(&conn).unwrap();
    try_load_sqlite_vec(&conn);
    conn
}

fn insert(conn: &mut Connection, id: &str, text: &str, keywords: &[&str]) {
    let e = memory_entry(id, text, keywords);
    upsert(conn, &e, false).unwrap();
}

fn insert_entry(conn: &mut Connection, entry: MemoryEntry) {
    upsert(conn, &entry, false).unwrap();
}

fn memory_entry(id: &str, text: &str, keywords: &[&str]) -> MemoryEntry {
    MemoryEntry {
        id: id.to_string(),
        path: "/test".into(),
        summary: text.chars().take(30).collect(),
        text: text.into(),
        importance: 0.7,
        timestamp: Utc::now().to_rfc3339(),
        valid_from: String::new(),
        valid_until: None,
        category: "fact".into(),
        topic: "".into(),
        keywords: keywords.iter().map(|s| s.to_string()).collect(),
        persons: vec![],
        entities: vec![],
        location: "".into(),
        source: "".into(),
        scope: "general".into(),
        archived: false,
        access_count: 0,
        last_access: None,
        revision: 1,
        metadata: json!({ "keywords": keywords, "entities": [] }),
        vector: None,
        retention_policy: None,
        domain: None,
        recall_count: 0,
        query_diversity: 0,
        tier: "raw".to_string(),
    }
}

#[test]
fn hybrid_symbolic_candidates_can_seed_path_scoped_short_technical_terms() {
    let mut conn = setup();
    let mut target = memory_entry(
        "clean-cli-memory",
        "The memory-server CLI clean bridge defaults to dry-run and requires --force for deletion.",
        &["clean-cli", "target-clean", "dry-run"],
    );
    target.path = "/scratch/tachi/clean-cli-integration".to_string();
    insert_entry(&mut conn, target);

    let mut other = memory_entry(
        "other-clean-memory",
        "Another cleanup note mentions dry-run but belongs elsewhere.",
        &["cleanup", "dry-run"],
    );
    other.path = "/scratch/other".to_string();
    insert_entry(&mut conn, other);

    let opts = SearchOptions {
        top_k: 3,
        candidates_per_channel: 0,
        path_prefix: Some("/scratch/tachi/clean-cli-integration".to_string()),
        record_access: false,
        ..Default::default()
    };
    let results = hybrid_search(&conn, "dry-run", &opts).unwrap();
    assert_eq!(results[0].entry.id, "clean-cli-memory");
    assert!(results[0].score.symbolic > 0.0);
}

#[test]
fn hybrid_search_propagates_access_history_errors() {
    let mut conn = setup();
    insert(
        &mut conn,
        "access-history-error",
        "AccessHistoryError should not silently degrade search scoring",
        &["accesshistoryerror"],
    );
    conn.execute("DROP TABLE access_history", []).unwrap();

    let opts = SearchOptions {
        top_k: 3,
        record_access: false,
        ..Default::default()
    };
    let err = hybrid_search(&conn, "AccessHistoryError", &opts)
        .expect_err("access history query errors should propagate");
    let msg = err.to_string();
    assert!(
        msg.contains("access_history") || msg.contains("no such table"),
        "expected access_history error, got: {msg}"
    );
}

#[test]
fn hybrid_symbolic_candidates_rank_exact_probe_token_above_siblings() {
    let mut conn = setup();
    insert(
        &mut conn,
        "alpha",
        "RECALL_PROBE_ALPHA_20260607 clean-cli bridge dry-run force-delete subcommands",
        &["recall-probe", "clean-cli", "dry-run"],
    );
    insert(
        &mut conn,
        "beta",
        "RECALL_PROBE_BETA_20260607 cleanup defaults preview before deletion",
        &["recall-probe", "cleanup"],
    );
    insert(
        &mut conn,
        "delta",
        "RECALL_PROBE_DELTA_20260607 profile routing requested_profile tool_profile",
        &["recall-probe", "profile"],
    );

    let opts = SearchOptions {
        top_k: 3,
        candidates_per_channel: 0,
        record_access: false,
        ..Default::default()
    };
    let results = hybrid_search(&conn, "RECALL_PROBE_ALPHA_20260607", &opts).unwrap();
    assert_eq!(results[0].entry.id, "alpha");
    assert!(results[0].score.symbolic > results[1].score.symbolic);
}

#[test]
fn fts_or_fallback_is_config_gated_and_preserves_all_terms_precision() {
    let mut conn = setup();
    insert(
        &mut conn,
        "all-terms",
        "cleanup cli safe deployment note",
        &["cleanup", "cli", "safe"],
    );
    insert(
        &mut conn,
        "partial-term",
        "cleanup preview deletes stale artifacts",
        &["cleanup"],
    );
    insert(
        &mut conn,
        "no-match",
        "router audit status page",
        &["router"],
    );

    let and_only = search_fts(&conn, "cleanup cli safe", 10, false, false, None, None).unwrap();
    assert!(
        and_only.contains_key("all-terms"),
        "simple_query FTS should match the row containing every query term"
    );
    assert!(
        !and_only.contains_key("partial-term"),
        "simple_query FTS is intentionally all-terms AND across query tokens"
    );

    let default_config = RecallConfig::default();
    let default_scores = search_fts_with_expansion_config(
        &conn,
        "cleanup cli safe",
        10,
        false,
        false,
        None,
        None,
        &default_config,
    )
    .unwrap();
    assert!(
        !default_scores.contains_key("partial-term"),
        "OR fallback must stay disabled by default to preserve current behavior"
    );

    let tuned_config = RecallConfig {
        or_fallback_fts_score_factor: 0.3,
        ..RecallConfig::default()
    };
    let tuned_scores = search_fts_with_expansion_config(
        &conn,
        "cleanup cli safe",
        10,
        false,
        false,
        None,
        None,
        &tuned_config,
    )
    .unwrap();
    let all_terms = tuned_scores
        .get("all-terms")
        .copied()
        .expect("all-terms score");
    let partial = tuned_scores
        .get("partial-term")
        .copied()
        .expect("partial term should enter through OR fallback");
    assert_eq!(all_terms, 1.0);
    assert!(
        partial > 0.0 && partial <= tuned_config.or_fallback_fts_score_factor,
        "fallback score should be bounded by the configured factor, got {partial}"
    );
    assert!(
        all_terms > partial,
        "all-term AND precision should outrank partial-term OR fallback"
    );
}

#[test]
fn hybrid_search_can_override_recall_config_per_call() {
    let mut conn = setup();
    insert(
        &mut conn,
        "all-terms",
        "cleanup cli safe deployment note",
        &["cleanup", "cli", "safe"],
    );
    insert(
        &mut conn,
        "partial-term",
        "cleanup preview deletes stale artifacts",
        &["cleanup"],
    );

    let base_opts = SearchOptions {
        top_k: 3,
        candidates_per_channel: 20,
        record_access: false,
        ..Default::default()
    };
    let default_results = hybrid_search(&conn, "cleanup cli safe", &base_opts).unwrap();
    let default_partial = default_results
        .iter()
        .find(|result| result.entry.id == "partial-term")
        .expect("symbolic candidates should keep partial row visible");
    assert_eq!(
        default_partial.score.fts, 0.0,
        "default config keeps all-terms AND FTS precision"
    );

    let tuned_opts = SearchOptions {
        recall_config: Some(RecallConfig {
            or_fallback_fts_score_factor: 0.3,
            ..RecallConfig::default()
        }),
        ..base_opts
    };
    let tuned_results = hybrid_search(&conn, "cleanup cli safe", &tuned_opts).unwrap();
    let tuned_partial = tuned_results
        .iter()
        .find(|result| result.entry.id == "partial-term")
        .expect("partial row should remain visible");
    assert!(
        tuned_partial.score.fts > 0.0,
        "per-call recall_config should enable OR fallback FTS for eval/simulation"
    );
    assert_eq!(tuned_results[0].entry.id, "all-terms");
}

#[test]
fn hybrid_search_promotes_exact_uuid_query() {
    let mut conn = setup();
    let exact_id = "11111111-1111-4111-8111-111111111111";
    insert(
        &mut conn,
        exact_id,
        "This row has unrelated prose and should still win by exact memory id.",
        &["exact-id"],
    );
    insert(
        &mut conn,
        "distractor",
        "11111111-1111-4111-8111-111111111111 appears only in text here.",
        &["distractor"],
    );

    let opts = SearchOptions {
        top_k: 2,
        candidates_per_channel: 0,
        record_access: false,
        ..Default::default()
    };
    let results = hybrid_search(&conn, exact_id, &opts).unwrap();

    assert_eq!(results[0].entry.id, exact_id);
    assert_eq!(results[0].score.symbolic, 1.0);
    assert!(results[0].score.final_score >= 10.0);
}

#[test]
fn hybrid_search_returns_post_record_access_fields() {
    let mut conn = setup();
    let mut entry = memory_entry(
        "access-return",
        "AccessReturnProbe unique searchable memory",
        &["access-return"],
    );
    entry.access_count = 7;
    insert_entry(&mut conn, entry);

    let opts = SearchOptions {
        top_k: 1,
        candidates_per_channel: 0,
        record_access: true,
        ..Default::default()
    };
    let results = hybrid_search(&conn, "AccessReturnProbe", &opts).unwrap();

    assert_eq!(results[0].entry.id, "access-return");
    assert_eq!(results[0].entry.access_count, 8);
    assert!(results[0].entry.last_access.is_some());
}

#[test]
fn record_access_deduplicates_repeated_ids_before_incrementing() {
    let mut conn = setup();
    insert(
        &mut conn,
        "duplicate-access",
        "DuplicateAccessProbe unique searchable memory",
        &["duplicate-access"],
    );

    let ids = vec![
        "duplicate-access".to_string(),
        "duplicate-access".to_string(),
    ];
    let updates =
        record_access_with_updates(&conn, &ids, &ids, Some("DuplicateAccessProbe")).unwrap();

    assert_eq!(updates["duplicate-access"].access_count, 1);
    let (access_count, recall_count): (i64, i64) = conn
        .query_row(
            "SELECT access_count, recall_count FROM memories WHERE id = ?1",
            rusqlite::params!["duplicate-access"],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!(access_count, 1);
    assert_eq!(recall_count, 1);

    let history_count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM access_history WHERE memory_id = ?1",
            rusqlite::params!["duplicate-access"],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(history_count, 1);
}

#[test]
fn superseded_path_gate_matches_reserved_prefixes_exactly() {
    assert!(!scoped_path_can_surface_superseded(Some("/wiki")));
    assert!(!scoped_path_can_surface_superseded(Some(
        "/wiki/agent/tachi"
    )));
    assert!(!scoped_path_can_surface_superseded(Some("/kanban")));
    assert!(!scoped_path_can_surface_superseded(Some(
        "/kanban/active/task"
    )));

    assert!(scoped_path_can_surface_superseded(Some(
        "/wiki_rules/agent/tachi"
    )));
    assert!(scoped_path_can_surface_superseded(Some(
        "/kanbanboard/active/task"
    )));
}

#[test]
fn hybrid_returns_relevant() {
    let mut conn = setup();
    insert(
        &mut conn,
        "a",
        "Rust is fast and memory safe",
        &["rust", "performance"],
    );
    insert(
        &mut conn,
        "b",
        "Python is great for scripting",
        &["python", "scripting"],
    );

    let opts = SearchOptions {
        top_k: 3,
        record_access: false,
        ..Default::default()
    };
    let results = hybrid_search(&conn, "rust performance", &opts).unwrap();
    assert!(!results.is_empty());
    // "a" should score higher for "rust performance" query
    assert_eq!(results[0].entry.id, "a");
}

#[test]
fn hybrid_uses_fts_when_vectors_are_available_but_query_vec_missing() {
    let mut conn = setup();
    insert(
        &mut conn,
        "a",
        "Voyage outage should still allow lexical fallback search",
        &["voyage", "fallback"],
    );
    insert(&mut conn, "b", "Unrelated operational note", &["ops"]);

    let opts = SearchOptions {
        top_k: 3,
        record_access: false,
        vec_available: true,
        query_vec: None,
        ..Default::default()
    };
    let results = hybrid_search(&conn, "voyage fallback", &opts).unwrap();
    assert!(!results.is_empty());
    assert_eq!(results[0].entry.id, "a");
    assert!(results[0].score.fts > 0.0);
}

#[test]
fn hybrid_expands_acronym_queries_for_fts() {
    let mut conn = setup();
    insert(
        &mut conn,
        "expanded",
        "Model Context Protocol handshake serialization checklist",
        &["protocol"],
    );

    let opts = SearchOptions {
        top_k: 3,
        record_access: false,
        ..Default::default()
    };
    let results = hybrid_search(&conn, "mcp handshake", &opts).unwrap();
    assert!(results.iter().any(|result| result.entry.id == "expanded"));
}

#[test]
fn hybrid_expands_phrase_queries_for_fts() {
    let mut conn = setup();
    insert(
        &mut conn,
        "acronym",
        "MCP handshake serialization checklist",
        &["mcp"],
    );

    let opts = SearchOptions {
        top_k: 3,
        record_access: false,
        ..Default::default()
    };
    let results = hybrid_search(&conn, "model context protocol handshake", &opts).unwrap();
    assert!(results.iter().any(|result| result.entry.id == "acronym"));
}

#[test]
fn hybrid_keeps_exact_fts_match_above_expanded_match() {
    let mut conn = setup();
    insert(
        &mut conn,
        "exact",
        "MCP handshake serialization checklist",
        &["mcp"],
    );
    insert(
        &mut conn,
        "expanded",
        "Model Context Protocol handshake serialization checklist",
        &["protocol"],
    );

    let opts = SearchOptions {
        top_k: 3,
        record_access: false,
        ..Default::default()
    };
    let results = hybrid_search(&conn, "mcp handshake", &opts).unwrap();
    assert_eq!(results[0].entry.id, "exact");
}

#[test]
fn empty_query_returns_empty() {
    let mut conn = setup();
    insert(&mut conn, "x", "some text", &[]);
    let opts = SearchOptions {
        record_access: false,
        ..Default::default()
    };
    let results = hybrid_search(&conn, "", &opts).unwrap();
    // FTS5 with empty query should produce no FTS results; vec channel also empty
    assert!(results.is_empty());
}

#[test]
fn guide_path_uses_operational_weights() {
    let opts = SearchOptions {
        path_prefix: Some("/guide/fix_pattern".to_string()),
        ..Default::default()
    };
    let weights = resolve_weights(&opts);
    assert_eq!(weights.fts, 0.45);
    assert_eq!(weights.symbolic, 0.28);
    assert_eq!(weights.decay, 0.02);
    assert!(weights.use_rrf);
}

#[test]
fn valid_at_compares_offset_timestamps_by_instant() {
    let mut entry = memory_entry("offset", "Offset timestamp memory", &[]);
    entry.valid_from = "2026-01-01T08:00:00+08:00".to_string();
    entry.valid_until = Some("2026-01-02T08:00:00+08:00".to_string());

    assert!(valid_at(&entry, Some("2026-01-01T00:00:00Z")));
    assert!(!valid_at(&entry, Some("2026-01-02T00:00:00Z")));
}

#[test]
fn hybrid_hides_superseded_by_default() {
    let mut conn = setup();
    insert(
        &mut conn,
        "old",
        "TrendLock protects trends using the stale rule",
        &["trendlock"],
    );
    insert(
        &mut conn,
        "new",
        "TrendLock protects trends using the canonical rule",
        &["trendlock"],
    );
    crate::db::supersede_memory(&conn, "old", "new").unwrap();

    let opts = SearchOptions {
        top_k: 5,
        record_access: false,
        ..Default::default()
    };
    let results = hybrid_search(&conn, "TrendLock", &opts).unwrap();
    let ids = results
        .into_iter()
        .map(|result| result.entry.id)
        .collect::<Vec<_>>();
    assert!(ids.contains(&"new".to_string()));
    assert!(!ids.contains(&"old".to_string()));
}

#[test]
fn hybrid_surfaces_superseded_when_explicitly_scoped_to_deep_path() {
    let mut conn = setup();
    let mut old = memory_entry(
        "old-path-memory",
        "clean-cli integration defaults to dry-run and requires --force",
        &["clean-cli", "dry-run"],
    );
    old.path = "/scratch/tachi/clean-cli-integration".to_string();
    insert_entry(&mut conn, old);
    let mut new = memory_entry(
        "new-release-memory",
        "release prep summary for tachi version bump",
        &["release-prep"],
    );
    new.path = "/scratch/tachi/v1.5-release-prep".to_string();
    insert_entry(&mut conn, new);
    crate::db::supersede_memory(&conn, "old-path-memory", "new-release-memory").unwrap();

    let opts = SearchOptions {
        top_k: 5,
        path_prefix: Some("/scratch/tachi/clean-cli-integration".to_string()),
        record_access: false,
        ..Default::default()
    };
    let results = hybrid_search(&conn, "dry-run", &opts).unwrap();
    assert_eq!(results[0].entry.id, "old-path-memory");
    assert!(results[0].score.final_score < 1.0);
}

#[test]
fn hybrid_search_respects_as_of_validity_window() {
    let mut conn = setup();
    let mut old = memory_entry("temporal-old", "TemporalHybridNeedle old memory", &[]);
    old.valid_from = "2026-01-01T00:00:00Z".to_string();
    old.valid_until = Some("2026-02-01T00:00:00Z".to_string());
    upsert(&mut conn, &old, false).unwrap();

    let mut new = memory_entry("temporal-new", "TemporalHybridNeedle new memory", &[]);
    new.valid_from = "2026-02-01T00:00:00Z".to_string();
    upsert(&mut conn, &new, false).unwrap();

    let january = hybrid_search(
        &conn,
        "TemporalHybridNeedle",
        &SearchOptions {
            top_k: 5,
            record_access: false,
            as_of: Some("2026-01-15T00:00:00.000Z".to_string()),
            ..Default::default()
        },
    )
    .unwrap()
    .into_iter()
    .map(|result| result.entry.id)
    .collect::<Vec<_>>();
    assert!(january.contains(&"temporal-old".to_string()));
    assert!(!january.contains(&"temporal-new".to_string()));

    let march = hybrid_search(
        &conn,
        "TemporalHybridNeedle",
        &SearchOptions {
            top_k: 5,
            record_access: false,
            as_of: Some("2026-03-01T00:00:00.000Z".to_string()),
            ..Default::default()
        },
    )
    .unwrap()
    .into_iter()
    .map(|result| result.entry.id)
    .collect::<Vec<_>>();
    assert!(!march.contains(&"temporal-old".to_string()));
    assert!(march.contains(&"temporal-new".to_string()));
}

#[test]
fn supersede_closes_open_valid_until_for_point_in_time_recall() {
    let mut conn = setup();
    let mut old = memory_entry("sup-old", "SupersedeNeedle old memory", &[]);
    old.valid_from = "2020-01-01T00:00:00Z".to_string();
    upsert(&mut conn, &old, false).unwrap();
    let new = memory_entry("sup-new", "SupersedeNeedle new memory", &[]);
    upsert(&mut conn, &new, false).unwrap();

    // Before supersession the row's validity window is open.
    let before: Option<String> = conn
        .query_row(
            "SELECT valid_until FROM memories WHERE id = 'sup-old'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert!(before.is_none(), "valid_until should start open");

    crate::db::supersede_memory(&conn, "sup-old", "sup-new").unwrap();

    // After supersession valid_until is closed, so `as_of` after that time
    // prefers the superseding row instead of returning the stale fact.
    let after: Option<String> = conn
        .query_row(
            "SELECT valid_until FROM memories WHERE id = 'sup-old'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert!(
        after.is_some(),
        "supersede_memory must close valid_until at supersession time"
    );
}

#[test]
fn supersede_preserves_explicit_valid_until() {
    let mut conn = setup();
    let mut old = memory_entry("sup-old2", "SupersedeNeedle2 old memory", &[]);
    old.valid_from = "2020-01-01T00:00:00Z".to_string();
    old.valid_until = Some("2021-06-01T00:00:00Z".to_string());
    upsert(&mut conn, &old, false).unwrap();
    let new = memory_entry("sup-new2", "SupersedeNeedle2 new memory", &[]);
    upsert(&mut conn, &new, false).unwrap();

    crate::db::supersede_memory(&conn, "sup-old2", "sup-new2").unwrap();

    // COALESCE keeps an explicitly-set window; supersession does not clobber it.
    let after: Option<String> = conn
        .query_row(
            "SELECT valid_until FROM memories WHERE id = 'sup-old2'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert!(
        after
            .as_deref()
            .unwrap_or_default()
            .starts_with("2021-06-01"),
        "explicit valid_until must be preserved, got {after:?}"
    );
}

#[test]
fn hybrid_hides_operation_logs() {
    let mut conn = setup();
    insert(
        &mut conn,
        "knowledge",
        "TrendLock durable decision rule for agents",
        &["trendlock"],
    );
    let mut log = memory_entry(
        "wiki-operation-log",
        "TrendLock write operation log should not be recalled",
        &["trendlock", "log"],
    );
    log.path = "/wiki/_log".to_string();
    log.topic = "wiki_log".to_string();
    log.metadata = json!({"wiki_log": true});
    upsert(&mut conn, &log, false).unwrap();

    let opts = SearchOptions {
        top_k: 5,
        record_access: false,
        ..Default::default()
    };
    let results = hybrid_search(&conn, "TrendLock", &opts).unwrap();
    assert!(results.iter().any(|result| result.entry.id == "knowledge"));
    assert!(!results
        .iter()
        .any(|result| result.entry.id == "wiki-operation-log"));
}

#[test]
fn quality_multiplier_demotes_sft_training_samples() {
    let mut sample = memory_entry(
        "sft-sample",
        "DaemonAdapterTimeoutFix root cause and verified production fix",
        &["daemon", "timeout", "fix"],
    );
    sample.importance = 0.95;
    sample.path = "/sft/v4/strict/engineering/123".to_string();
    sample.topic = "sft-memory".to_string();
    sample.metadata = json!({"training_sample": true});
    assert_eq!(quality_multiplier(&sample), 0.45);

    let mut handoff = memory_entry(
        "handoff",
        "DaemonAdapterTimeoutFix operational handoff",
        &["daemon", "timeout", "fix"],
    );
    handoff.category = "handoff".to_string();
    handoff.importance = 0.95;
    assert_eq!(quality_multiplier(&handoff), 1.0);
}

#[test]
fn quality_multiplier_demotes_openclaw_low_signal_entries() {
    let mut legacy = memory_entry(
        "openclaw-legacy",
        "Legacy migrated raw session note",
        &["openclaw", "legacy"],
    );
    legacy.importance = 0.95;
    legacy.path = "/openclaw/legacy".to_string();
    assert_eq!(quality_multiplier(&legacy), 0.55);

    let mut unnamed = memory_entry(
        "openclaw-unnamed",
        "Unnamed migrated memory should not dominate recall",
        &["openclaw", "unnamed"],
    );
    unnamed.importance = 0.95;
    unnamed.path = "/openclaw/agent-main/unnamed".to_string();
    assert_eq!(quality_multiplier(&unnamed), 0.55);
}

#[test]
fn recall_cache_variants_are_search_noise_by_default() {
    let mut cache = memory_entry(
        "openclaw-recall-cache",
        "Recall rerank cache for query: Scout pipeline fixes",
        &["recall", "cache"],
    );
    cache.path = "/openclaw/agent-main/recall-cache/Scout_pipeline".to_string();
    cache.topic = "recall_rerank_cache".to_string();
    assert!(is_search_noise_entry(&cache, None));
    assert!(is_search_noise_entry(&cache, Some("/openclaw/agent-main")));
    assert!(!is_search_noise_entry(
        &cache,
        Some("/openclaw/agent-main/recall-cache")
    ));
}

#[test]
fn graph_expansion_orders_neighbors_by_spreading_activation() {
    let mut conn = setup();
    insert(
        &mut conn,
        "seed",
        "TrendLock durable decision rule",
        &["trendlock"],
    );
    insert(
        &mut conn,
        "support",
        "Support note only reachable by graph",
        &["support"],
    );
    insert(
        &mut conn,
        "related",
        "Related note only reachable by graph",
        &["related"],
    );
    add_edge(
        &conn,
        &MemoryEdge {
            source_id: "seed".to_string(),
            target_id: "related".to_string(),
            relation: "related_to".to_string(),
            weight: 1.0,
            metadata: json!({}),
            created_at: String::new(),
            valid_from: String::new(),
            valid_to: None,
        },
    )
    .unwrap();
    add_edge(
        &conn,
        &MemoryEdge {
            source_id: "seed".to_string(),
            target_id: "support".to_string(),
            relation: "supports".to_string(),
            weight: 1.0,
            metadata: json!({}),
            created_at: String::new(),
            valid_from: String::new(),
            valid_to: None,
        },
    )
    .unwrap();

    let opts = SearchOptions {
        top_k: 1,
        record_access: false,
        graph_expand_hops: 1,
        ..Default::default()
    };
    let results = hybrid_search(&conn, "TrendLock", &opts).unwrap();
    let ids = results
        .iter()
        .map(|result| result.entry.id.as_str())
        .collect::<Vec<_>>();
    assert_eq!(ids, vec!["seed", "support", "related"]);
    assert!(results[1].score.final_score > results[2].score.final_score);
}
