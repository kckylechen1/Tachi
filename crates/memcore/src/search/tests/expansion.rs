use super::*;

#[test]
fn fts_or_fallback_is_enabled_by_default_and_preserves_all_terms_precision() {
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

    let and_only = search_fts(
        &conn,
        "cleanup cli safe",
        10,
        false,
        false,
        None,
        None,
        None,
        false,
    )
    .unwrap();
    assert!(
        and_only.contains_key("all-terms"),
        "simple_query FTS should match the row containing every query term"
    );
    assert!(
        !and_only.contains_key("partial-term"),
        "simple_query FTS is intentionally all-terms AND across query tokens"
    );

    let default_config = RecallConfig::default();
    let fallback_query = "cleanup cli absent";
    let default_scores = search_fts_with_expansion_config(
        &conn,
        fallback_query,
        10,
        false,
        false,
        None,
        None,
        &default_config,
        // sample = false: existing #708 Gate-1 test does not inspect the
        // per-phase receipt; passing false keeps it on the not-sampled path
        // (no `Instant::now` reads, no group Vec) — that is the mechanism,
        // not a measured zero-overhead claim (tachi#1097 S1).
        false,
        None,
        false,
    )
    .unwrap()
    .0;
    assert!(
        default_scores.contains_key("partial-term"),
        "tachi#708 Gate 1 turns OR fallback on by default when conjunctive FTS returns no candidates"
    );
    assert_eq!(RecallConfig::default().or_fallback_fts_score_factor, 0.55);

    let tuned_config = RecallConfig {
        or_fallback_fts_score_factor: 0.3,
        ..RecallConfig::default()
    };
    let tuned_scores = search_fts_with_expansion_config(
        &conn,
        fallback_query,
        10,
        false,
        false,
        None,
        None,
        &tuned_config,
        false,
        None,
        false,
    )
    .unwrap()
    .0;
    let all_terms = tuned_scores
        .get("all-terms")
        .copied()
        .expect("all-terms score");
    let partial = tuned_scores
        .get("partial-term")
        .copied()
        .expect("partial term should enter through OR fallback");
    assert!(
        all_terms > 0.0 && all_terms <= tuned_config.or_fallback_fts_score_factor,
        "all-term fallback score should be bounded by the configured factor, got {all_terms}"
    );
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
fn fts_or_fallback_cjk_phrase_recovers_when_ascii_term_is_missing() {
    let mut conn = setup();
    insert(
        &mut conn,
        "cjk-target",
        "中文查询无法触发回退的诊断记录",
        &["中文", "回退"],
    );
    insert(
        &mut conn,
        "ascii-only",
        "diagnostic note about a missing lexical token",
        &["diagnostic"],
    );

    let and_only = search_fts(
        &conn,
        "中文查询 missing",
        10,
        false,
        false,
        None,
        None,
        None,
        false,
    )
    .unwrap();
    assert!(
        !and_only.contains_key("cjk-target"),
        "simple_query FTS requires the missing ASCII term and should zero the CJK target"
    );

    let scores = search_fts_with_expansion_config(
        &conn,
        "中文查询 missing",
        10,
        false,
        false,
        None,
        None,
        &RecallConfig::default(),
        false,
        None,
        false,
    )
    .unwrap()
    .0;
    let target = scores
        .get("cjk-target")
        .copied()
        .expect("quoted CJK phrase should enter through the OR fallback");
    assert!(
        target > 0.0,
        "CJK fallback phrase should contribute a positive FTS score"
    );
}

/// tachi#1143 kill-test: `simple` indexes a contiguous Han run as individual
/// tokenizer units. When an all-Han conjunctive query misses on two units, the
/// fallback must OR those same units; treating the run as one quoted phrase
/// skips fallback entirely because it appears to have only one term.
#[test]
fn fts_or_fallback_recovers_pure_han_query_with_tokenizer_units() {
    let mut conn = setup();
    insert(
        &mut conn,
        "pure-han-target",
        "量子风暴稳定控制记录",
        &["量子", "风暴", "控制"],
    );
    insert(
        &mut conn,
        "pure-han-common-fragment-decoy",
        "量子无关的运维记录",
        &["量子", "运维"],
    );

    let query = "量子风暴故障";
    let primary = search_fts(&conn, query, 10, false, false, None, None, None, false).unwrap();
    assert!(
        primary.is_empty(),
        "the missing 故/障 units must zero the primary conjunctive FTS query"
    );

    let opts = SearchOptions {
        top_k: 2,
        candidates_per_channel: 10,
        weights: HybridWeights {
            semantic: 0.0,
            fts: 1.0,
            symbolic: 0.0,
            decay: 0.0,
            use_rrf: false,
        },
        record_access: false,
        mmr_threshold: None,
        ..Default::default()
    };
    let (results, receipt) = hybrid_search_with_receipt(&conn, query, &opts).unwrap();
    let candidates = receipt.candidates.expect("candidate phase ran");
    let fallback_groups: Vec<&FtsExpansionGroupReceipt> = candidates
        .fts_groups
        .iter()
        .filter(|group| group.is_fallback)
        .collect();
    assert_eq!(
        fallback_groups.len(),
        1,
        "a pure-Han miss with multiple simple-tokenizer units gets exactly one fallback group"
    );
    assert!(
        fallback_groups[0].hit_count >= 2,
        "the fallback must recover both the target and its common-fragment decoy"
    );

    let target = results
        .iter()
        .find(|result| result.entry.id == "pure-han-target")
        .expect("the labeled pure-Han target must survive the fallback");
    let decoy = results
        .iter()
        .find(|result| result.entry.id == "pure-han-common-fragment-decoy")
        .expect("the common-fragment decoy proves this is not a phrase-only fixture");
    assert!(
        target.score.fts > 0.0,
        "the target enters through FTS fallback"
    );
    assert!(
        target.score.fts > decoy.score.fts,
        "more matching tokenizer units must outrank the common-fragment decoy"
    );

    let disabled = RecallConfig {
        or_fallback_fts_score_factor: 0.0,
        ..RecallConfig::default()
    };
    let disabled_scores = search_fts_with_expansion_config(
        &conn, query, 10, false, false, None, None, &disabled, false, None, false,
    )
    .unwrap()
    .0;
    assert!(
        disabled_scores.is_empty(),
        "the same pure-Han miss stays at zero when OR fallback is explicitly disabled"
    );
}

/// A repeated-only Han query is already a primary FTS hit: `simple_query`
/// tokenizes both characters to the same term, so it never enters fallback.
#[test]
fn repeated_han_units_stay_on_the_primary_fts_path() {
    let mut conn = setup();
    insert(
        &mut conn,
        "repeated-han-target",
        "哈运维诊断记录",
        &["哈", "诊断"],
    );

    let query = "哈哈";
    let primary = search_fts(&conn, query, 10, false, false, None, None, None, false).unwrap();
    assert!(
        primary.contains_key("repeated-han-target"),
        "duplicate Han query units match the same primary FTS term"
    );

    let opts = SearchOptions {
        top_k: 2,
        candidates_per_channel: 10,
        weights: HybridWeights {
            semantic: 0.0,
            fts: 1.0,
            symbolic: 0.0,
            decay: 0.0,
            use_rrf: false,
        },
        record_access: false,
        mmr_threshold: None,
        ..Default::default()
    };
    let (results, receipt) = hybrid_search_with_receipt(&conn, query, &opts).unwrap();
    let candidates = receipt.candidates.expect("candidate phase ran");
    let fallback_groups: Vec<&FtsExpansionGroupReceipt> = candidates
        .fts_groups
        .iter()
        .filter(|group| group.is_fallback)
        .collect();
    assert!(
        fallback_groups.is_empty(),
        "a primary hit must not enter the OR fallback path"
    );
    assert!(results
        .iter()
        .any(|result| result.entry.id == "repeated-han-target" && result.score.fts > 0.0));
}

#[test]
fn hybrid_search_treats_unbalanced_parentheses_as_literal_noise() {
    let mut conn = setup();
    insert(
        &mut conn,
        "paren-noise-target",
        "AbsentToken survives a search query with unbalanced parentheses.",
        &["AbsentToken"],
    );

    let opts = SearchOptions {
        top_k: 5,
        candidates_per_channel: 10,
        record_access: false,
        ..Default::default()
    };

    let results = hybrid_search(&conn, "))) AbsentToken (((", &opts)
        .expect("punctuation in user search text must not surface FTS5 syntax errors");

    assert!(
        results
            .iter()
            .any(|result| result.entry.id == "paren-noise-target"),
        "sanitized query should still find the lexical token"
    );
}

#[test]
fn hybrid_recovers_candidate_starved_partial_coverage_target_through_or_fallback() {
    let mut conn = setup();
    let mut target = memory_entry(
        "starved-target",
        "quartz beacon coverage repair note",
        &["quartz", "beacon"],
    );
    target.timestamp = "2026-01-01T00:00:00Z".to_string();
    insert_entry(&mut conn, target);

    for idx in 0..12 {
        let mut distractor = memory_entry(
            &format!("fresh-symbolic-{idx:02}"),
            "quartz scratch distractor",
            &["quartz"],
        );
        distractor.timestamp = format!("2026-01-02T00:00:{idx:02}Z");
        insert_entry(&mut conn, distractor);
    }

    let opts = SearchOptions {
        top_k: 1,
        candidates_per_channel: 1,
        weights: HybridWeights {
            semantic: 0.0,
            fts: 1.0,
            symbolic: 0.0,
            decay: 0.0,
            use_rrf: false,
        },
        record_access: false,
        mmr_threshold: None,
        ..Default::default()
    };
    let results = hybrid_search(&conn, "quartz beacon absent", &opts).unwrap();
    let top = results
        .first()
        .expect("OR fallback should supply a candidate despite conjunctive zero");
    assert_eq!(
        top.entry.id, "starved-target",
        "coverage-ranked OR fallback should rescue a target missing one query term"
    );
    assert!(
        top.score.fts > 0.0,
        "rescued target should enter via the FTS fallback stream"
    );
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
