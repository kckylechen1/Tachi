use super::*;

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
