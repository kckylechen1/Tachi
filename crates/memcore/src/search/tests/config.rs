use super::*;

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

    let disabled_opts = SearchOptions {
        top_k: 3,
        candidates_per_channel: 20,
        record_access: false,
        recall_config: Some(RecallConfig {
            or_fallback_fts_score_factor: 0.0,
            ..RecallConfig::default()
        }),
        ..Default::default()
    };
    let query = "cleanup cli absent";
    let disabled_results = hybrid_search(&conn, query, &disabled_opts).unwrap();
    let disabled_partial = disabled_results
        .iter()
        .find(|result| result.entry.id == "partial-term")
        .expect("symbolic candidates should keep partial row visible");
    assert_eq!(
        disabled_partial.score.fts, 0.0,
        "or_fallback=0.0 keeps all-terms AND FTS precision"
    );

    let tuned_opts = SearchOptions {
        top_k: 3,
        candidates_per_channel: 20,
        record_access: false,
        // tachi#708 Gate 1 changed the default from off to the provisional
        // 0.55 coverage channel, so the default is now the live eval path.
        ..Default::default()
    };
    let tuned_results = hybrid_search(&conn, query, &tuned_opts).unwrap();
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
