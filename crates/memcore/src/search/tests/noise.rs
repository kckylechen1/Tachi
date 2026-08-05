use super::*;

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
        "wiki-hidden-log",
        "TrendLock write operation log should not be recalled",
        &["trendlock", "log"],
    );
    log.path = "/wiki/general/internal-log".to_string();
    log.metadata = json!({"wiki_log": true});
    upsert(&mut conn, &log, false).unwrap();
    conn.execute(
        "UPDATE memories SET metadata = json_set(metadata, '$.wiki_log', 1) WHERE id = 'wiki-hidden-log'",
        [],
    )
    .unwrap();

    let opts = SearchOptions {
        top_k: 5,
        record_access: false,
        ..Default::default()
    };
    let results = hybrid_search(&conn, "TrendLock", &opts).unwrap();
    assert!(results.iter().any(|result| result.entry.id == "knowledge"));
    assert!(!results
        .iter()
        .any(|result| result.entry.id == "wiki-hidden-log"));
}

#[test]
fn wiki_scoped_search_filters_logs_before_channel_limit() {
    let mut conn = setup();
    let mut real = memory_entry(
        "wiki-real",
        "WikiBudgetNeedle durable user-facing lesson",
        &["wikibudgetneedle"],
    );
    real.path = "/wiki/z-agent/real".to_string();
    upsert(&mut conn, &real, false).unwrap();
    for index in 0..8 {
        let mut log = memory_entry(
            &format!("wiki-log-{index}"),
            "WikiBudgetNeedle WikiBudgetNeedle internal operation log",
            &["wikibudgetneedle", "log"],
        );
        log.path = format!("/wiki/a-log/{index}");
        log.importance = 1.0;
        upsert(&mut conn, &log, false).unwrap();
        conn.execute(
            "UPDATE memories SET metadata = json_set(metadata, '$.wiki_log', 1) WHERE id = ?1",
            [&log.id],
        )
        .unwrap();
    }
    let results = hybrid_search(
        &conn,
        "WikiBudgetNeedle",
        &SearchOptions {
            candidates_per_channel: 5,
            top_k: 1,
            path_prefix: Some("/wiki".to_string()),
            record_access: false,
            ..Default::default()
        },
    )
    .unwrap();
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].entry.id, "wiki-real");
}

#[test]
fn quality_multiplier_scopes_wiki_boost_to_wiki_path_prefix() {
    use super::super::filtering::quality_multiplier;

    let mut wiki = memory_entry("wiki-row", "A reusable wiki lesson", &["wiki"]);
    wiki.path = "/wiki/general/lesson".to_string();
    wiki.category = "wiki".to_string();

    // Unscoped mixed search: no blanket wiki ×1.15 (tachi#708 Phase D).
    assert_eq!(quality_multiplier(&wiki, None), 1.0);
    // Scoped wiki search retains the boost.
    assert_eq!(quality_multiplier(&wiki, Some("/wiki")), 1.15);
    assert_eq!(quality_multiplier(&wiki, Some("/wiki/general")), 1.15);
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
    assert_eq!(quality_multiplier(&sample, None), 0.45);

    let mut handoff = memory_entry(
        "handoff",
        "DaemonAdapterTimeoutFix operational handoff",
        &["daemon", "timeout", "fix"],
    );
    handoff.category = "handoff".to_string();
    handoff.importance = 0.95;
    assert_eq!(quality_multiplier(&handoff, None), 1.0);
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
    assert_eq!(quality_multiplier(&legacy, None), 0.55);

    let mut unnamed = memory_entry(
        "openclaw-unnamed",
        "Unnamed migrated memory should not dominate recall",
        &["openclaw", "unnamed"],
    );
    unnamed.importance = 0.95;
    unnamed.path = "/openclaw/agent-main/unnamed".to_string();
    assert_eq!(quality_multiplier(&unnamed, None), 0.55);
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
    assert!(is_search_noise_entry(&cache, None, false));
    assert!(is_search_noise_entry(
        &cache,
        Some("/openclaw/agent-main"),
        false
    ));
    assert!(!is_search_noise_entry(
        &cache,
        Some("/openclaw/agent-main/recall-cache"),
        false
    ));
}

#[test]
fn continuity_projections_require_an_explicit_matching_path_scope() {
    let mut conn = setup();
    insert(
        &mut conn,
        "ordinary",
        "ProjectionBoundaryNeedle ordinary memory",
        &["projection-boundary"],
    );
    let mut projection = memory_entry(
        "timeline-projection",
        "ProjectionBoundaryNeedle continuity timeline read model",
        &["projection-boundary"],
    );
    projection.path = "/timeline/session/example".to_string();
    projection.metadata = json!({"projection_kind": "timeline"});
    upsert(&mut conn, &projection, false).unwrap();

    let unscoped = hybrid_search(
        &conn,
        "ProjectionBoundaryNeedle",
        &SearchOptions {
            top_k: 5,
            record_access: true,
            ..Default::default()
        },
    )
    .unwrap();
    assert!(unscoped.iter().any(|result| result.entry.id == "ordinary"));
    assert!(!unscoped
        .iter()
        .any(|result| result.entry.id == "timeline-projection"));
    let projection_counts: (i64, i64) = conn
        .query_row(
            "SELECT access_count, scored_count FROM memories WHERE id = 'timeline-projection'",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!(projection_counts, (0, 0));

    let scoped = hybrid_search(
        &conn,
        "ProjectionBoundaryNeedle",
        &SearchOptions {
            top_k: 5,
            path_prefix: Some("/timeline".to_string()),
            record_access: false,
            ..Default::default()
        },
    )
    .unwrap();
    assert!(scoped
        .iter()
        .any(|result| result.entry.id == "timeline-projection"));

    assert!(!crate::is_continuity_projection_path(
        "/timeline-notes/example"
    ));
    assert!(!crate::is_continuity_projection_path(
        "/user/patternsmith/example"
    ));
    assert!(!crate::is_continuity_projection_path(
        "/outcomes-old/example"
    ));
    assert!(!crate::is_continuity_projection_path(
        "/project-cycles/example"
    ));
}
