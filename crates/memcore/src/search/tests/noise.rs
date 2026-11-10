use super::*;

#[test]
fn test_namespace_noise_cannot_take_the_first_recall_slot() {
    let mut conn = setup();
    let mut real = memory_entry(
        "real-decision",
        "RecallBoundaryNeedle durable decision",
        &["RecallBoundaryNeedle"],
    );
    real.path = "/notes/decision".to_string();
    upsert(&mut conn, &real, false).unwrap();
    for index in 0..8 {
        let mut noise = memory_entry(
            &format!("noise-{index}"),
            "RecallBoundaryNeedle RecallBoundaryNeedle highly relevant synthetic observation",
            &["RecallBoundaryNeedle"],
        );
        noise.path = format!("/test/probe-{index}");
        noise.importance = 1.0;
        upsert(&mut conn, &noise, false).unwrap();
    }
    let opts = SearchOptions {
        top_k: 1,
        record_access: true,
        ..Default::default()
    };
    let results = hybrid_search(&conn, "RecallBoundaryNeedle", &opts).unwrap();
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].entry.id, "real-decision");
    let accesses: i64 = conn
        .query_row(
            "SELECT SUM(access_count + scored_count) FROM memories WHERE path LIKE '/test/%'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(
        accesses, 0,
        "hidden synthetic evidence must not accumulate recall feedback"
    );
    let scoped = hybrid_search(
        &conn,
        "RecallBoundaryNeedle",
        &SearchOptions {
            path_prefix: Some("/test/".to_string()),
            record_access: false,
            ..opts
        },
    )
    .unwrap();
    assert_eq!(scoped.len(), 1);
    assert!(scoped[0].entry.id.starts_with("noise-"));
}

#[test]
fn synthetic_candidates_are_excluded_before_top_k_and_counted_once_across_graph() {
    let mut conn = setup();
    let ranked_id = "22392239-2239-4239-8239-223922392239";
    for (id, text, path, marked) in [
        (
            "fixture-real",
            "SyntheticBoundaryNeedle durable decision",
            "/notes/decision",
            false,
        ),
        (
            ranked_id,
            "SyntheticBoundaryNeedle synthetic observation",
            "/scratch/tachi-test/probe",
            false,
        ),
        (
            "fixture-graph",
            "Fabricated graph-only detail",
            "/eval/avoidance",
            true,
        ),
    ] {
        let mut entry = memory_entry(id, text, &[]);
        entry.path = path.into();
        entry.metadata["synthetic_fixture"] = json!(marked);
        upsert(&mut conn, &entry, false).unwrap();
    }
    for target in [ranked_id, "fixture-graph"] {
        add_edge(
            &conn,
            &MemoryEdge {
                source_id: "fixture-real".into(),
                target_id: target.into(),
                relation: "supports".into(),
                weight: 1.0,
                metadata: json!({}),
                created_at: String::new(),
                valid_from: String::new(),
                valid_to: None,
            },
        )
        .unwrap();
    }
    let options = SearchOptions {
        top_k: 5,
        graph_expand_hops: 1,
        mmr_threshold: None,
        record_access: false,
        ..Default::default()
    };
    let (rows, receipt) =
        hybrid_search_with_receipt(&conn, "SyntheticBoundaryNeedle", &options).unwrap();
    assert_eq!(
        rows.iter()
            .map(|row| row.entry.id.as_str())
            .collect::<Vec<_>>(),
        ["fixture-real"]
    );
    assert_eq!(receipt.excluded_test_rows, 2);
    assert_eq!(receipt.rank.unwrap().excluded_test_rows, 1);
    assert_eq!(
        receipt.graph_expansion.unwrap().excluded_test_rows,
        1,
        "ranked fixture reached again via graph must not count twice"
    );
    let (rows, receipt) = hybrid_search_with_receipt(
        &conn,
        "SyntheticBoundaryNeedle",
        &SearchOptions {
            include_test: true,
            ..options
        },
    )
    .unwrap();
    assert_eq!(rows.len(), 3);
    assert_eq!(receipt.excluded_test_rows, 0);

    let (rows, receipt) = hybrid_search_with_receipt(
        &conn,
        ranked_id,
        &SearchOptions {
            graph_expand_hops: 0,
            record_access: false,
            ..Default::default()
        },
    )
    .unwrap();
    assert!(
        rows.is_empty(),
        "exact-id search must not bypass default recall isolation"
    );
    assert_eq!(
        receipt.excluded_test_rows, 1,
        "an empty result still has an observed candidate exclusion"
    );
}

#[test]
fn recent_context_filters_synthetic_rows_before_limit_and_keeps_audit_reads() {
    let mut conn = setup();
    for (id, marked, time) in [
        ("real-checkpoint", json!(false), "2026-11-01T00:00:00Z"),
        ("marked-checkpoint", json!(true), "2026-11-02T00:00:00Z"),
        ("string-checkpoint", json!("true"), "2026-11-03T00:00:00Z"),
    ] {
        let mut entry = memory_entry(id, "Checkpoint context", &[]);
        entry.path = format!("/agent/checkpoints/{id}");
        entry.timestamp = time.into();
        entry.metadata["synthetic_fixture"] = marked;
        upsert(&mut conn, &entry, false).unwrap();
    }
    let rows = crate::db::list_by_path_recent_for_recall(
        &conn,
        "/agent/checkpoints",
        2,
        false,
        false,
        false,
    )
    .unwrap();
    assert_eq!(
        rows.iter().map(|row| row.id.as_str()).collect::<Vec<_>>(),
        ["string-checkpoint", "real-checkpoint"]
    );
    let opted = crate::db::list_by_path_recent_for_recall(
        &conn,
        "/agent/checkpoints",
        2,
        false,
        false,
        true,
    )
    .unwrap();
    let audit =
        crate::db::list_by_path_recent(&conn, "/agent/checkpoints", 2, false, false).unwrap();
    assert_eq!(
        opted.iter().map(|row| &row.id).collect::<Vec<_>>(),
        audit.iter().map(|row| &row.id).collect::<Vec<_>>()
    );
    assert!(audit.iter().any(|row| row.id == "marked-checkpoint"));
    for (id, path, marker) in [
        ("reserved-probe", "/test/probe", true),
        ("uppercase-marked", "/TEST/probe", true),
        ("uppercase-ordinary", "/TEST/real", false),
    ] {
        let mut entry = memory_entry(id, "Test prefix boundary", &[]);
        entry.path = path.into();
        entry.metadata["synthetic_fixture"] = json!(marker);
        upsert(&mut conn, &entry, false).unwrap();
    }
    let scoped =
        crate::db::list_by_path_recent_for_recall(&conn, "/test", 10, false, false, false).unwrap();
    assert_eq!(scoped.len(), 2);
    assert!(
        !scoped.iter().any(|row| row.id == "uppercase-marked"),
        "a case-insensitive list prefix cannot opt in to a differently cased fixture path"
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
    assert!(is_search_noise_entry(&cache, None, false, false));
    assert!(is_search_noise_entry(
        &cache,
        Some("/openclaw/agent-main"),
        false,
        false
    ));
    assert!(!is_search_noise_entry(
        &cache,
        Some("/openclaw/agent-main/recall-cache"),
        false,
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
