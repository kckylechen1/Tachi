use super::*;
use crate::{
    db::{gc_tables, record_access_with_updates},
    recall_impressions::{
        RecallImpressionPayload, RecallImpressionRowDraft, IMPRESSION_GROUP_INSERT_SQL,
        IMPRESSION_ROW_INSERT_SQL,
    },
    scorer::{fuse_pre_boost_score, HybridWeights, PreBoostAdjustment},
    GcConfig, RecallConfig,
};

fn sampled_options() -> SearchOptions {
    SearchOptions {
        top_k: 10,
        candidates_per_channel: 32,
        mmr_threshold: None,
        recall_config: Some(RecallConfig {
            impression_sample_rate_bps: 10_000,
            ..RecallConfig::default()
        }),
        ..SearchOptions::default()
    }
}

#[test]
fn sampled_recall_replays_preboost_bits_and_persists_statuses() {
    let mut conn = setup();
    insert(
        &mut conn,
        "impression-a",
        "alpha beta recall ledger",
        &["alpha", "beta"],
    );
    insert(
        &mut conn,
        "impression-b",
        "alpha beta replay evidence",
        &["alpha", "beta"],
    );

    let results = hybrid_search(&conn, "alpha beta", &sampled_options()).unwrap();
    assert!(!results.is_empty());
    let group_id: String = conn
        .query_row("SELECT group_id FROM recall_impression_groups", [], |row| {
            row.get(0)
        })
        .unwrap();
    let report = crate::replay_recall_impression_group(&conn, &group_id).unwrap();
    assert_eq!(report.bit_identical_count, report.candidate_count);
    assert!(!report.post_boost_claimed);
    let (scored, returned, mandatory_access): (i64, i64, i64) = conn
        .query_row(
            "SELECT MIN(scored), SUM(scored_returned), MIN(access_count_at_recall) FROM recall_impressions WHERE group_id = ?1",
            [&group_id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .unwrap();
    assert_eq!(scored, 1);
    assert_eq!(returned as usize, results.len());
    assert_eq!(mandatory_access, 0);
}

#[test]
fn default_zero_is_deterministic_writes_zero_and_constructs_no_payload() {
    let mut conn = setup();
    insert(
        &mut conn,
        "default-off",
        "default off impression probe",
        &["default", "off"],
    );
    let before = super::ranking::impression_payload_constructions();
    let opts = SearchOptions {
        mmr_threshold: None,
        ..SearchOptions::default()
    };
    let first = hybrid_search(&conn, "default off", &opts).unwrap();
    let second = hybrid_search(&conn, "default off", &opts).unwrap();
    assert_eq!(
        first.iter().map(|row| &row.entry.id).collect::<Vec<_>>(),
        second.iter().map(|row| &row.entry.id).collect::<Vec<_>>()
    );
    assert_eq!(super::ranking::impression_payload_constructions(), before);
    let count: i64 = conn
        .query_row("SELECT COUNT(*) FROM recall_impression_groups", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert_eq!(count, 0);

    let before_no_access = super::ranking::impression_payload_constructions();
    let no_access = SearchOptions {
        record_access: false,
        recall_config: Some(RecallConfig {
            impression_sample_rate_bps: 10_000,
            ..RecallConfig::default()
        }),
        ..SearchOptions::default()
    };
    hybrid_search(&conn, "default off", &no_access).unwrap();
    assert_eq!(
        super::ranking::impression_payload_constructions(),
        before_no_access
    );
}

#[test]
#[ignore = "timing report only"]
fn impression_default_zero_hot_branch_timing_report() {
    const ITERATIONS: usize = 5_000_000;
    let baseline_start = std::time::Instant::now();
    for _ in 0..ITERATIONS {
        std::hint::black_box(false);
    }
    let baseline = baseline_start.elapsed();
    let measured_start = std::time::Instant::now();
    for _ in 0..ITERATIONS {
        std::hint::black_box(crate::recall_impressions::should_sample_query(
            std::hint::black_box("default zero hot path"),
            0,
        ));
    }
    let measured = measured_start.elapsed();
    println!(
        "impression_default0 iterations={ITERATIONS} baseline_ns={} measured_ns={} net_ns_per_call={:.3}",
        baseline.as_nanos(),
        measured.as_nanos(),
        measured.saturating_sub(baseline).as_nanos() as f64 / ITERATIONS as f64
    );
}

fn replay_row(
    memory_id: &str,
    pre_boost_rank: usize,
    decay: f64,
    weights: &HybridWeights,
) -> RecallImpressionRowDraft {
    let pre_boost_score = fuse_pre_boost_score(
        0.5,
        0.5,
        0.5,
        decay,
        Some(pre_boost_rank),
        Some(pre_boost_rank),
        Some(pre_boost_rank),
        weights,
        20.0,
    );
    RecallImpressionRowDraft {
        memory_id: memory_id.to_string(),
        vector_score: 0.5,
        fts_score: 0.5,
        symbolic_score: 0.5,
        decay_score: decay,
        vec_rank: Some(pre_boost_rank),
        fts_rank: Some(pre_boost_rank),
        sym_rank: Some(pre_boost_rank),
        merge_adjustment: PreBoostAdjustment::None,
        pre_boost_score,
        pre_boost_rank,
        tie_break_epoch_millis: pre_boost_rank as i64,
        final_score: pre_boost_score,
        final_rank: pre_boost_rank,
        scored: true,
        scored_returned: true,
        access_count_at_recall: 0,
    }
}

#[test]
fn multi_channel_order_and_pairwise_decay_zero_inversions_are_exact() {
    let mut conn = setup();
    for id in ["tie-a", "tie-b", "tie-c"] {
        insert(
            &mut conn,
            id,
            "multi channel replay tie",
            &["multi", "channel"],
        );
    }
    let weights = HybridWeights {
        semantic: 0.25,
        fts: 0.25,
        symbolic: 0.25,
        decay: 0.25,
        use_rrf: false,
    };
    let mut first = replay_row("tie-a", 1, 1.0, &weights);
    first.tie_break_epoch_millis = 10;
    let mut second = replay_row("tie-b", 2, 0.5, &weights);
    second.tie_break_epoch_millis = 20;
    let mut third = replay_row("tie-c", 3, 0.0, &weights);
    third.tie_break_epoch_millis = 0;
    let payload = RecallImpressionPayload {
        group_id: "pairwise-group".to_string(),
        created_at: "2026-07-29T00:00:00.000Z".to_string(),
        query_hash: crate::db::query_hash("multi channel"),
        weights_profile: "custom".to_string(),
        weights: weights.clone(),
        rrf_k: 20.0,
        top_k: 3,
        rows: vec![first, second, third],
        displayed_count: 3,
    };
    record_access_with_updates(
        &conn,
        &["tie-a".into(), "tie-b".into(), "tie-c".into()],
        &["tie-a".into(), "tie-b".into(), "tie-c".into()],
        &[],
        Some("multi channel"),
        &RecallConfig::default(),
        Some(&payload),
    )
    .unwrap();
    let report = crate::replay_recall_impression_group(&conn, "pairwise-group").unwrap();
    assert_eq!(report.bit_identical_count, 3);
    let expected = report
        .candidates
        .iter()
        .enumerate()
        .flat_map(|(left_index, left)| {
            report.candidates[left_index + 1..]
                .iter()
                .map(move |right| (left, right))
        })
        .filter(|(left, right)| {
            left.recorded_pre_boost_rank
                .cmp(&right.recorded_pre_boost_rank)
                != left.decay_zero_rank.cmp(&right.decay_zero_rank)
        })
        .count();
    assert_eq!(report.decay_zero_rank_inversions, expected);
    assert_eq!(report.decay_zero_rank_inversions, 1);
}

#[test]
fn mmr_rank_and_graph_display_counts_are_named_from_actual_output() {
    let mut conn = setup();
    insert(
        &mut conn,
        "mmr-seed",
        "NeedleRecall exact seed",
        &["needlerecall"],
    );
    insert(
        &mut conn,
        "graph-only",
        "unrelated graph neighbor",
        &["unrelated"],
    );
    add_edge(
        &conn,
        &MemoryEdge {
            source_id: "mmr-seed".into(),
            target_id: "graph-only".into(),
            relation: "supports".into(),
            weight: 1.0,
            metadata: json!({}),
            created_at: String::new(),
            valid_from: String::new(),
            valid_to: None,
        },
    )
    .unwrap();
    let mut opts = sampled_options();
    opts.top_k = 2;
    opts.graph_expand_hops = 1;
    opts.mmr_threshold = Some(0.85);
    let results = hybrid_search(&conn, "NeedleRecall", &opts).unwrap();
    assert_eq!(results.len(), 2);
    let (displayed, scored_returned): (i64, i64) = conn
        .query_row(
            "SELECT displayed_count, scored_returned_count FROM recall_impression_groups",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!(displayed, results.len() as i64);
    assert_eq!(scored_returned, 1);
    let final_rank: i64 = conn
        .query_row(
            "SELECT final_rank FROM recall_impressions WHERE memory_id = 'mmr-seed'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(final_rank, 1);
}

#[test]
fn exact_id_override_preserves_real_channel_components_and_replays_bits() {
    let mut conn = setup();
    let id = "018f0f51-7b0c-7c84-9000-000000000001";
    insert(&mut conn, id, "unrelated payload words", &["unrelated"]);
    hybrid_search(&conn, id, &sampled_options()).unwrap();
    let (vector, fts, symbolic, decay, pre): (f64, f64, f64, f64, f64) = conn
        .query_row(
            "SELECT vector_score, fts_score, symbolic_score, decay_score, pre_boost_score FROM recall_impressions WHERE memory_id = ?1",
            [id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?, row.get(4)?)),
        )
        .unwrap();
    assert_ne!((vector, fts, symbolic, decay), (1.0, 1.0, 1.0, 1.0));
    assert_eq!(pre, 10.0);
    let group_id: String = conn
        .query_row("SELECT group_id FROM recall_impression_groups", [], |row| {
            row.get(0)
        })
        .unwrap();
    let report = crate::replay_recall_impression_group(&conn, &group_id).unwrap();
    assert_eq!(report.bit_identical_count, report.candidate_count);
}

#[test]
fn impression_insert_failure_rolls_back_access_and_group_atomically() {
    let mut conn = setup();
    insert(
        &mut conn,
        "atomic-row",
        "atomic impression rollback",
        &["atomic"],
    );
    let weights = HybridWeights::default();
    let row = replay_row("atomic-row", 1, 1.0, &weights);
    let payload = RecallImpressionPayload {
        group_id: "atomic-group".to_string(),
        created_at: "2026-07-29T00:00:00.000Z".to_string(),
        query_hash: crate::db::query_hash("atomic"),
        weights_profile: "default".to_string(),
        weights,
        rrf_k: 20.0,
        top_k: 1,
        rows: vec![row.clone(), row],
        displayed_count: 1,
    };
    let ids = ["atomic-row".to_string()];
    assert!(record_access_with_updates(
        &conn,
        &ids,
        &ids,
        &[],
        Some("atomic"),
        &RecallConfig::default(),
        Some(&payload),
    )
    .is_err());
    let (access_count, groups): (i64, i64) = conn
        .query_row(
            "SELECT access_count, (SELECT COUNT(*) FROM recall_impression_groups) FROM memories WHERE id = 'atomic-row'",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!((access_count, groups), (0, 0));
}

#[test]
fn impression_retention_is_independent_and_memory_delete_preserves_history() {
    let mut conn = setup();
    insert(
        &mut conn,
        "retention-row",
        "retention impression",
        &["retention"],
    );
    for query in ["retention one", "retention two"] {
        hybrid_search(&conn, query, &sampled_options()).unwrap();
    }
    let history_before: i64 = conn
        .query_row("SELECT COUNT(*) FROM access_history", [], |row| row.get(0))
        .unwrap();
    let diversity_before: i64 = conn
        .query_row(
            "SELECT query_diversity FROM memories WHERE id = 'retention-row'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    gc_tables(
        &mut conn,
        &GcConfig {
            recall_impression_max_groups: 1,
            recall_impression_max_days: 10_000,
            ..GcConfig::default()
        },
    )
    .unwrap();
    assert_eq!(
        conn.query_row("SELECT COUNT(*) FROM recall_impression_groups", [], |row| {
            row.get::<_, i64>(0)
        })
        .unwrap(),
        1
    );
    assert_eq!(
        conn.query_row("SELECT COUNT(*) FROM access_history", [], |row| row
            .get::<_, i64>(0))
            .unwrap(),
        history_before
    );
    assert_eq!(
        conn.query_row(
            "SELECT query_diversity FROM memories WHERE id = 'retention-row'",
            [],
            |row| row.get::<_, i64>(0)
        )
        .unwrap(),
        diversity_before
    );
    conn.execute("DELETE FROM memories WHERE id = 'retention-row'", [])
        .unwrap();
    assert!(
        conn.query_row("SELECT COUNT(*) FROM recall_impressions", [], |row| row
            .get::<_, i64>(0))
            .unwrap()
            > 0
    );
}

#[test]
fn impression_inserts_are_mechanically_content_free() {
    let sql = format!("{IMPRESSION_GROUP_INSERT_SQL} {IMPRESSION_ROW_INSERT_SQL}").to_lowercase();
    for forbidden in [
        "query_text",
        " text",
        "entity",
        "path",
        "vector_embedding",
        "propensity",
        "cache_id",
        "content",
    ] {
        assert!(
            !sql.contains(forbidden),
            "forbidden INSERT field: {forbidden}"
        );
    }
}

#[test]
fn replay_is_read_only_and_bookkeeping_is_explicit() {
    let mut conn = setup();
    insert(&mut conn, "readonly-row", "readonly replay", &["readonly"]);
    hybrid_search(&conn, "readonly", &sampled_options()).unwrap();
    let group_id: String = conn
        .query_row("SELECT group_id FROM recall_impression_groups", [], |row| {
            row.get(0)
        })
        .unwrap();
    crate::replay_recall_impression_group(&conn, &group_id).unwrap();
    let before: i64 = conn
        .query_row(
            "SELECT replay_count FROM recall_impression_groups",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(before, 0);
    assert_eq!(
        crate::increment_recall_impression_replay_count(&conn, &group_id).unwrap(),
        1
    );
}
