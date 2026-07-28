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
        scored_count: 0,
        last_access: None,
        last_use_at: None,
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
        scored_count: 0,
        last_access: None,
        last_use_at: None,
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
        scored_count: 0,
        last_access: None,
        last_use_at: None,
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
        scored_count: 0,
        last_access: None,
        last_use_at: None,
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
fn default_decay_policy_preserves_existing_decay_entry_points() {
    use chrono::Duration;
    let mut entry = test_entry("default-policy");
    entry.timestamp = (Utc::now() - Duration::days(10)).to_rfc3339();
    entry.last_access = Some((Utc::now() - Duration::days(2)).to_rfc3339());
    entry.access_count = 3;

    let access_ages = [3_600.0, 7_200.0];
    assert_eq!(
        decay_score_with_config(&entry, RecallConfig::get()),
        decay_score_with_policy(&entry, RecallConfig::get(), &DEFAULT_DECAY_POLICY)
    );
    assert_eq!(
        decay_score_actr_with_config(&entry, Some(&access_ages), RecallConfig::get()),
        decay_score_actr_with_policy(
            &entry,
            Some(&access_ages),
            RecallConfig::get(),
            &DEFAULT_DECAY_POLICY,
        )
    );
}

/// tachi#1446 lever 1, at the single line that reads the timestamp rather than
/// through the whole search stack: which column `recency` ages against, and
/// that the default is unchanged.
///
/// `importance = 0.0` removes the `importance * 0.3` floor so the raw `recency`
/// term is observable, and `text` is empty so `leading_event_datetime` cannot
/// supply a reference. The subject is 240 days stale (eight raw half-lives,
/// `recency ~ 0.004`) but was "accessed" a moment ago — i.e. exactly the shape
/// the exposure loop manufactures.
#[test]
fn use_provenance_recency_moves_the_age_reference_off_last_access() {
    let mut entry = test_entry("provenance-recency");
    entry.importance = 0.0;
    entry.timestamp = (Utc::now() - chrono::Duration::days(240)).to_rfc3339();
    entry.last_access = Some(Utc::now().to_rfc3339());

    let off = RecallConfig {
        use_provenance_recency: false,
        ..RecallConfig::default()
    };
    let on = RecallConfig::default();

    let with_knob_off = decay_score_with_config(&entry, &off);
    assert!(
        with_knob_off > 0.99,
        "explicit legacy config must keep reading last_access: a row touched a moment ago has \
         age_days ~ 0, so recency ~ 1.0; got {with_knob_off}"
    );

    let with_knob_on = decay_score_with_config(&entry, &on);
    assert!(
        with_knob_on < 0.01,
        "with the knob on, a NULL last_use_at must fall through to the content timestamp — \
         240 days at a 30-day half-life is recency ~ 0.004, not {with_knob_on}"
    );

    // And the new column is genuinely read, not merely ignored: populate it and
    // the knob-on path tracks it.
    entry.last_use_at = Some(Utc::now().to_rfc3339());
    let with_use_recorded = decay_score_with_config(&entry, &on);
    assert!(
        with_use_recorded > 0.99,
        "a populated last_use_at must drive recency when the knob is on; got {with_use_recorded}"
    );

    // The off path is indifferent to the new column in both states. Asserted as
    // a band, not an equality: `default_decay_score_with_config` re-reads
    // `Utc::now()` on every call, so two calls that straddle a second boundary
    // legitimately differ in the last decimals.
    let off_after_write = decay_score_with_config(&entry, &off);
    assert!(
        off_after_write > 0.99,
        "writing last_use_at must not change what explicit legacy config reads; got {off_after_write}"
    );
}

/// tachi#1446 **lever 3** — the surprise score's `overlooked` component, whose
/// polarity is the inverse of every other lever: exposure does not earn this
/// bonus, it permanently *revokes* it.
///
/// `access_count == 0 && importance > 0.7` awards `0.20 * 0.3 = 0.06` of the
/// composite. Nothing ever decrements `access_count`, so the first search that
/// returns a high-importance memory takes it from 0 to 1 and the memory stops
/// counting as overlooked forever — on the strength of the system having
/// looked at it once.
///
/// With the knob on the predicate is `last_use_at IS NULL`, which needs no new
/// column: nothing but `db::record_memory_use` writes that column, so an
/// exposed-but-never-used memory keeps the bonus and a genuinely used one
/// loses it.
#[test]
fn use_provenance_recency_moves_the_overlooked_bonus_off_exposure() {
    let off = RecallConfig {
        use_provenance_recency: false,
        ..RecallConfig::default()
    };
    let on = RecallConfig::default();

    // Same everything except the two provenance columns. `importance = 0.8`
    // clears the `> 0.7` gate; `avg_importance = 0.8` zeroes component 1, and
    // a `total_same_topic` of 2 fixes component 3, so the ONLY term that can
    // differ between these calls is `overlooked`.
    let never_touched = {
        let mut e = test_entry("overlooked-never-touched");
        e.importance = 0.8;
        e
    };
    let exposed_only = {
        let mut e = never_touched.clone();
        e.id = "overlooked-exposed".into();
        e.access_count = 12;
        e.last_access = Some(Utc::now().to_rfc3339());
        e
    };
    let genuinely_used = {
        let mut e = exposed_only.clone();
        e.id = "overlooked-used".into();
        e.last_use_at = Some(Utc::now().to_rfc3339());
        e
    };

    let score = |entry: &crate::types::MemoryEntry, config: &RecallConfig| {
        surprise_score_with_config(entry, 0.8, 0, 2, config)
    };

    let baseline = score(&never_touched, &off);
    // 0.20 * 0.3 = 0.06 is the whole magnitude of this lever.
    assert!(
        (baseline - score(&exposed_only, &off) - 0.06).abs() < 1e-12,
        "control: under explicit legacy config, exposure alone must cost exactly the 0.06 overlooked \
         component ({baseline} vs {})",
        score(&exposed_only, &off)
    );

    assert!(
        (score(&exposed_only, &on) - baseline).abs() < 1e-12,
        "with the knob on, an exposed-but-never-used memory must keep the overlooked bonus: \
         {} != {baseline}",
        score(&exposed_only, &on)
    );
    assert!(
        (baseline - score(&genuinely_used, &on) - 0.06).abs() < 1e-12,
        "with the knob on, a genuinely used memory must lose it — otherwise the lever is \
         deleted, not switched: {} vs {baseline}",
        score(&genuinely_used, &on)
    );

    // The default path is indifferent to the new column, in both directions.
    assert!(
        (score(&genuinely_used, &off) - score(&exposed_only, &off)).abs() < 1e-12,
        "writing last_use_at must not change what explicit legacy config computes"
    );
    assert!(
        (surprise_score(&never_touched, 0.8, 0, 2) - baseline).abs() < 1e-12,
        "the config-free entry point must keep returning the default-config number"
    );
}

struct FakeTradingStyleDecay;

impl DecayPolicy for FakeTradingStyleDecay {
    fn score_decay(
        &self,
        entry: &crate::types::MemoryEntry,
        _recall_config: &RecallConfig,
        access_ages: Option<&[f64]>,
    ) -> f64 {
        let latest_access_days = access_ages
            .and_then(|ages| ages.iter().copied().reduce(f64::min))
            .map(|secs| secs / 86_400.0)
            .unwrap_or(30.0);
        if entry.tier == "pattern" && latest_access_days <= 1.0 {
            0.95
        } else {
            0.20
        }
    }
}

struct FakeAffectStyleDecay;

impl DecayPolicy for FakeAffectStyleDecay {
    fn score_decay(
        &self,
        entry: &crate::types::MemoryEntry,
        _recall_config: &RecallConfig,
        _access_ages: Option<&[f64]>,
    ) -> f64 {
        entry
            .metadata
            .get("affect_weight")
            .and_then(serde_json::Value::as_f64)
            .unwrap_or(0.10)
    }
}

#[test]
fn fake_trading_style_decay_policy_can_drive_hybrid_decay_without_domain_names() {
    let mut recent_signal = test_entry("recent-signal");
    recent_signal.tier = "pattern".to_string();
    let mut stale_signal = test_entry("stale-signal");
    stale_signal.tier = "pattern".to_string();
    let entries = HashMap::from([
        ("recent-signal".to_string(), &recent_signal),
        ("stale-signal".to_string(), &stale_signal),
    ]);
    let vec_scores = HashMap::from([
        ("recent-signal".to_string(), 0.1),
        ("stale-signal".to_string(), 0.1),
    ]);
    let fts_scores = HashMap::new();
    let symbolic_scores = HashMap::new();
    let access_times = HashMap::from([
        ("recent-signal".to_string(), vec![30.0 * 60.0]),
        ("stale-signal".to_string(), vec![14.0 * 86_400.0]),
    ]);
    let weights = HybridWeights {
        semantic: 0.0,
        fts: 0.0,
        symbolic: 0.0,
        decay: 1.0,
        use_rrf: false,
    };

    let scored = hybrid_score_with_policy(
        &entries,
        &vec_scores,
        &fts_scores,
        &symbolic_scores,
        &weights,
        &access_times,
        DecayPolicyContext::new(RecallConfig::get(), &FakeTradingStyleDecay),
    );

    assert_eq!(scored["recent-signal"].decay, 0.95);
    assert_eq!(scored["stale-signal"].decay, 0.20);
    assert!(scored["recent-signal"].final_score > scored["stale-signal"].final_score);
}

#[test]
fn fake_chat_affect_decay_policy_can_use_adapter_metadata_without_kernel_names() {
    let mut calm = test_entry("calm");
    calm.metadata = serde_json::json!({ "affect_weight": 0.25 });
    let mut urgent = test_entry("urgent");
    urgent.metadata = serde_json::json!({ "affect_weight": 0.85 });

    let calm_score =
        decay_score_actr_with_policy(&calm, None, RecallConfig::get(), &FakeAffectStyleDecay);
    let urgent_score =
        decay_score_actr_with_policy(&urgent, None, RecallConfig::get(), &FakeAffectStyleDecay);

    assert_eq!(calm_score, 0.25);
    assert_eq!(urgent_score, 0.85);
    assert!(urgent_score > calm_score);
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

#[test]
fn graph_spreading_activation_rejects_non_finite_edge_weight() {
    use crate::types::MemoryEdge;

    let seeds = HashMap::from([("seed".to_string(), 1.0)]);
    let edges = vec![MemoryEdge {
        source_id: "seed".to_string(),
        target_id: "poisoned".to_string(),
        relation: "supports".to_string(),
        weight: f64::NAN,
        metadata: serde_json::json!({}),
        created_at: String::new(),
        valid_from: String::new(),
        valid_to: None,
    }];

    let activation = graph_spreading_activation_with_seed_weights(&seeds, &edges, 1, 0.5);
    assert!(
        !activation.contains_key("poisoned"),
        "non-finite legacy/imported edge weights must be inert rather than propagate NaN"
    );
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
