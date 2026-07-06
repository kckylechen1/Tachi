use super::*;
use std::collections::HashMap;

fn test_entry(id: &str) -> crate::types::MemoryEntry {
    crate::types::MemoryEntry {
        id: id.into(),
        path: "/test".into(),
        summary: String::new(),
        text: String::new(),
        importance: 0.7,
        timestamp: Utc::now().to_rfc3339(),
        valid_from: String::new(),
        valid_until: None,
        category: "fact".into(),
        topic: String::new(),
        keywords: vec![],
        persons: vec![],
        entities: vec![],
        location: String::new(),
        source: "manual".into(),
        scope: "general".into(),
        archived: false,
        access_count: 0,
        last_access: None,
        revision: 1,
        metadata: serde_json::json!({}),
        retention_policy: None,
        domain: None,
        vector: None,
        recall_count: 0,
        query_diversity: 0,
        tier: "raw".to_string(),
    }
}

#[test]
fn cosine_identity() {
    let v = vec![1.0_f32, 0.0, 0.0];
    assert!((cosine_similarity(&v, &v) - 1.0).abs() < 1e-9);
}

#[test]
fn cosine_orthogonal() {
    let a = vec![1.0_f32, 0.0];
    let b = vec![0.0_f32, 1.0];
    assert!((cosine_similarity(&a, &b)).abs() < 1e-9);
}

#[test]
fn cosine_dimension_mismatch_returns_zero() {
    let a = vec![1.0_f32, 0.0];
    let b = vec![1.0_f32, 0.0, 999.0];
    assert_eq!(cosine_similarity(&a, &b), 0.0);
}

#[test]
fn symbolic_exact_match() {
    let score = symbolic_score("hello world", "hello world", &[], &[]);
    assert!(score > 0.9, "score={score}");
}

#[test]
fn symbolic_exact_query_match_is_not_diluted_by_long_text() {
    let long_text = format!(
        "{} mcp handshake {}",
        "filler ".repeat(200),
        "extra ".repeat(200)
    );
    let score = symbolic_score("mcp handshake", &long_text, &[], &[]);
    assert!(score > 0.9, "score={score}");
}

#[test]
fn symbolic_score_credits_exact_entity_token() {
    let entities = vec!["688981".to_string()];
    let score = symbolic_score("688981", "无关正文", &[], &entities);
    assert!(score > 0.9, "score={score}");
    assert!(score <= 1.0, "score={score}");
}

#[test]
fn rrf_respects_channel_weights() {
    let a = test_entry("a");
    let b = test_entry("b");
    let entries = HashMap::from([("a".to_string(), &a), ("b".to_string(), &b)]);
    let vec_scores = HashMap::from([("a".to_string(), 0.9), ("b".to_string(), 0.8)]);
    let fts_scores = HashMap::from([("a".to_string(), 0.1), ("b".to_string(), 0.9)]);
    let symbolic_scores = HashMap::new();
    let access_times = HashMap::new();

    let vector_heavy = HybridWeights {
        semantic: 0.9,
        fts: 0.1,
        symbolic: 0.0,
        decay: 0.0,
        use_rrf: true,
    };
    let vector_ranked = hybrid_score(
        &entries,
        &vec_scores,
        &fts_scores,
        &symbolic_scores,
        &vector_heavy,
        &access_times,
    );
    assert!(
        vector_ranked["a"].final_score > vector_ranked["b"].final_score,
        "vector-heavy RRF should prefer vector rank"
    );

    let fts_heavy = HybridWeights {
        semantic: 0.1,
        fts: 0.9,
        symbolic: 0.0,
        decay: 0.0,
        use_rrf: true,
    };
    let fts_ranked = hybrid_score(
        &entries,
        &vec_scores,
        &fts_scores,
        &symbolic_scores,
        &fts_heavy,
        &access_times,
    );
    assert!(
        fts_ranked["b"].final_score > fts_ranked["a"].final_score,
        "fts-heavy RRF should prefer FTS rank"
    );
}

#[test]
fn precision_multiplier_for_id_like_exact_probe() {
    use chrono::Utc;
    let entry = crate::types::MemoryEntry {
        id: "recall-probe-alpha-20260607".into(),
        path: "/scratch/tachi/recall-probe-alpha-20260607".into(),
        summary: "alpha recall probe".into(),
        text: "RECALL_PROBE_ALPHA_20260607 clean-cli bridge behavior".into(),
        importance: 0.7,
        timestamp: Utc::now().to_rfc3339(),
        valid_from: String::new(),
        valid_until: None,
        category: "fact".into(),
        topic: String::new(),
        keywords: vec![],
        persons: vec![],
        entities: vec![],
        location: String::new(),
        source: "manual".into(),
        scope: "project".into(),
        archived: false,
        access_count: 0,
        last_access: None,
        revision: 1,
        metadata: serde_json::json!({}),
        retention_policy: None,
        domain: None,
        vector: None,
        recall_count: 0,
        query_diversity: 0,
        tier: "raw".to_string(),
    };
    assert!(is_id_like_exact_query("RECALL_PROBE_ALPHA_20260607"));
    assert!(generic_precision_multiplier("RECALL_PROBE_ALPHA_20260607", &entry) >= 10.0);
    assert_eq!(
        generic_precision_multiplier("recall probe alpha", &entry),
        1.0
    );
}

#[test]
fn decay_never_accessed() {
    use chrono::Duration;
    let mut entry = crate::types::MemoryEntry {
        id: "test".into(),
        path: "/".into(),
        summary: "".into(),
        text: "".into(),
        importance: 0.7,
        timestamp: (Utc::now() - Duration::days(60)).to_rfc3339(),
        valid_from: String::new(),
        valid_until: None,
        category: "fact".into(),
        topic: "".into(),
        keywords: vec![],
        persons: vec![],
        entities: vec![],
        location: "".into(),
        source: "".into(),
        scope: "general".into(),
        archived: false,
        access_count: 0,
        last_access: None,
        revision: 1,
        metadata: serde_json::Value::Object(Default::default()),
        vector: None,
        retention_policy: None,
        domain: None,
        recall_count: 0,
        query_diversity: 0,
        tier: "raw".to_string(),
    };
    let s = decay_score(&entry);
    // 60-day old, no access → recency ~ exp(-0.693*2) ≈ 0.25, floor=0.7*0.3=0.21 → ~0.25
    assert!(s > 0.1 && s < 0.5, "unexpected decay={s}");

    // With importance floor
    entry.importance = 1.0;
    let s2 = decay_score(&entry);
    assert!(s2 >= 0.3, "importance floor violated: {s2}");
}

#[test]
fn decay_invalid_timestamp_does_not_rank_as_fresh() {
    let mut entry = test_entry("bad-timestamp");
    entry.timestamp = "not-a-timestamp".to_string();
    entry.importance = 0.1;
    entry.last_access = None;
    entry.text.clear();

    let score = decay_score(&entry);

    assert!(
        score <= 0.05,
        "invalid timestamp should fall back to stale reference, got {score}"
    );
}

#[test]
fn actr_access_history_uses_day_scale() {
    use chrono::Duration;
    let entry = crate::types::MemoryEntry {
        id: "test".into(),
        path: "/".into(),
        summary: "".into(),
        text: "".into(),
        importance: 0.7,
        timestamp: (Utc::now() - Duration::days(60)).to_rfc3339(),
        valid_from: String::new(),
        valid_until: None,
        category: "fact".into(),
        topic: "".into(),
        keywords: vec![],
        persons: vec![],
        entities: vec![],
        location: "".into(),
        source: "".into(),
        scope: "general".into(),
        archived: false,
        access_count: 0,
        last_access: None,
        revision: 1,
        metadata: serde_json::Value::Object(Default::default()),
        vector: None,
        retention_policy: None,
        domain: None,
        recall_count: 0,
        query_diversity: 0,
        tier: "raw".to_string(),
    };

    let never = decay_score_actr(&entry, None);
    let old = decay_score_actr(&entry, Some(&[60.0 * 86_400.0]));
    let recent = decay_score_actr(&entry, Some(&[3_600.0, 7_200.0]));

    assert!(old >= never, "old={old}, never={never}");
    assert!(recent > old, "recent={recent}, old={old}");
}

#[test]
fn rrf_blend_rewards_absolute_vector_similarity_without_penalizing_missing_vector() {
    let vec_scores = HashMap::from([
        ("a".to_string(), 0.99),
        ("b".to_string(), 0.98),
        ("c".to_string(), 0.97),
    ]);
    let base = 0.02;

    let blended = blend_rrf_with_vector_signal("c", base, &vec_scores, 1.0);
    assert!(blended > base, "blended={blended}, base={base}");

    let missing = blend_rrf_with_vector_signal("x", base, &vec_scores, 1.0);
    assert_eq!(missing, base);
}

#[test]
fn graph_spreading_activation_decays_by_hop_and_relation_type() {
    use crate::types::MemoryEdge;

    let seeds = HashMap::from([("a".to_string(), 1.0)]);
    let edges = vec![
        MemoryEdge {
            source_id: "a".to_string(),
            target_id: "b".to_string(),
            relation: "supports".to_string(),
            weight: 1.0,
            metadata: serde_json::json!({}),
            created_at: String::new(),
            valid_from: String::new(),
            valid_to: None,
        },
        MemoryEdge {
            source_id: "b".to_string(),
            target_id: "c".to_string(),
            relation: "causes".to_string(),
            weight: 1.0,
            metadata: serde_json::json!({}),
            created_at: String::new(),
            valid_from: String::new(),
            valid_to: None,
        },
        MemoryEdge {
            source_id: "a".to_string(),
            target_id: "d".to_string(),
            relation: "contradicts".to_string(),
            weight: 1.0,
            metadata: serde_json::json!({}),
            created_at: String::new(),
            valid_from: String::new(),
            valid_to: None,
        },
    ];

    let activation = graph_spreading_activation_with_seed_weights(&seeds, &edges, 2, 0.5);
    assert!(!activation.contains_key("a"));
    assert!(activation["b"] > activation["c"]);
    assert!(activation["b"] > activation["d"]);
    assert!(activation["c"] > 0.0);
}

#[test]
fn graph_spreading_activation_uses_weighted_seeds_and_converging_paths() {
    use crate::types::MemoryEdge;

    let mut seed_weights = HashMap::new();
    seed_weights.insert("strong".to_string(), 1.0);
    seed_weights.insert("weak".to_string(), 0.25);
    let edges = vec![
        MemoryEdge {
            source_id: "strong".to_string(),
            target_id: "shared".to_string(),
            relation: "supports".to_string(),
            weight: 1.0,
            metadata: serde_json::json!({}),
            created_at: String::new(),
            valid_from: String::new(),
            valid_to: None,
        },
        MemoryEdge {
            source_id: "weak".to_string(),
            target_id: "shared".to_string(),
            relation: "supports".to_string(),
            weight: 1.0,
            metadata: serde_json::json!({}),
            created_at: String::new(),
            valid_from: String::new(),
            valid_to: None,
        },
        MemoryEdge {
            source_id: "weak".to_string(),
            target_id: "weak-only".to_string(),
            relation: "supports".to_string(),
            weight: 1.0,
            metadata: serde_json::json!({}),
            created_at: String::new(),
            valid_from: String::new(),
            valid_to: None,
        },
    ];

    let activation = graph_spreading_activation_with_seed_weights(&seed_weights, &edges, 1, 0.5);
    assert!(!activation.contains_key("strong"));
    assert!(!activation.contains_key("weak"));
    assert!(activation["shared"] > activation["weak-only"]);
    assert!(activation["shared"] > 0.45);
}

/// tachi#718 CP2 — a score tie must break by TRUE instant, not lexical string
/// order. The three real-world timestamp formats below are mis-ordered by a raw
/// `str` compare: `.` (0x2E) < `Z` (0x5A), so `...00:00:00Z` sorts ABOVE the
/// strictly-newer `...00:00:00.500Z`; and a `+01:00` offset is compared as text,
/// ignoring the zone. Parsed to an instant the order is unambiguous. RED under
/// the string-compare tie-break, GREEN once the key is parsed epoch millis.
#[test]
fn recall_tiebreak_orders_mixed_timestamp_formats_by_true_instant() {
    let score = 1.0;
    // True instants (UTC): plain=00:00:00.000, millis=00:00:00.500,
    // offset(01:30+01:00)=00:30:00.000. Newest-first → offset, millis, plain.
    let mut rows = [
        (score, "2026-01-01T00:00:00Z", "plain"),
        (score, "2026-01-01T00:00:00.500Z", "millis"),
        (score, "2026-01-01T01:30:00+01:00", "offset"),
    ];
    rows.sort_by(|a, b| {
        cmp_recall_rank(
            (a.0, timestamp_epoch_millis(a.1), a.2),
            (b.0, timestamp_epoch_millis(b.1), b.2),
        )
    });
    let order: Vec<&str> = rows.iter().map(|r| r.2).collect();
    assert_eq!(
        order,
        ["offset", "millis", "plain"],
        "score-tied rows must order by parsed instant desc, not lexical string"
    );
}
