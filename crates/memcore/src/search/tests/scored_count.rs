use super::*;

fn counts(conn: &Connection, id: &str) -> (i64, i64, i64) {
    conn.query_row(
        "SELECT access_count, scored_count, (SELECT COUNT(*) FROM access_history WHERE memory_id = ?1) FROM memories WHERE id = ?1",
        rusqlite::params![id],
        |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
    )
    .unwrap()
}

#[test]
fn scorer_counts_top_k_losers_without_recording_display_access() {
    let mut conn = setup();
    insert(
        &mut conn,
        "winner",
        "ScoredCountProbe winner precise lexical match",
        &["scoredcountprobe"],
    );
    insert(
        &mut conn,
        "loser",
        "ScoredCountProbe loser lexical match",
        &["scoredcountprobe"],
    );
    let opts = SearchOptions {
        top_k: 1,
        mmr_threshold: None,
        record_access: true,
        ..Default::default()
    };

    let results = hybrid_search(&conn, "ScoredCountProbe winner", &opts).unwrap();
    assert_eq!(results.len(), 1);
    let winner = &results[0].entry.id;
    let loser = if winner == "winner" {
        "loser"
    } else {
        "winner"
    };
    assert_eq!(counts(&conn, winner), (1, 1, 1));
    assert_eq!(
        results[0].entry.scored_count,
        counts(&conn, winner).1,
        "returned displayed entry must carry its post-write scored count"
    );
    assert_eq!(counts(&conn, loser), (0, 1, 0));

    hybrid_search(&conn, "ScoredCountProbe winner", &opts).unwrap();
    assert_eq!(counts(&conn, winner).1, 2, "one increment per search");
    assert_eq!(counts(&conn, loser).1, 2, "one increment per search");
}

#[test]
fn scorer_excludes_post_candidate_eligibility_rejections() {
    let mut conn = setup();
    let mut eligible = memory_entry(
        "eligible",
        "ScoredEligibilityProbe lexical candidate",
        &["scoredeligibilityprobe"],
    );
    eligible.path = "/inside".into();
    insert_entry(&mut conn, eligible);
    let mut filtered = memory_entry(
        "filtered",
        "ScoredEligibilityProbe lexical candidate",
        &["scoredeligibilityprobe"],
    );
    filtered.path = "/outside".into();
    insert_entry(&mut conn, filtered);

    let opts = SearchOptions {
        path_prefix: Some("/inside".into()),
        record_access: true,
        mmr_threshold: None,
        ..Default::default()
    };
    let results = hybrid_search(&conn, "ScoredEligibilityProbe", &opts).unwrap();
    assert_eq!(
        results
            .iter()
            .map(|r| r.entry.id.as_str())
            .collect::<Vec<_>>(),
        ["eligible"]
    );
    assert_eq!(counts(&conn, "eligible").1, 1);
    assert_eq!(
        counts(&conn, "filtered"),
        (0, 0, 0),
        "post-candidate path eligibility rejection must not receive scorer evidence"
    );
}

#[test]
fn top_k_zero_scores_without_display_and_advances_generation() {
    let mut conn = setup();
    insert(
        &mut conn,
        "scored-only",
        "ScoredOnlyGenerationProbe lexical candidate",
        &["scoredonlygenerationprobe"],
    );
    let cached_generation = crate::db::search_generation(&conn).unwrap();
    let opts = SearchOptions {
        top_k: 0,
        record_access: true,
        mmr_threshold: None,
        ..Default::default()
    };
    let results = hybrid_search(&conn, "ScoredOnlyGenerationProbe", &opts).unwrap();
    assert!(results.is_empty());
    assert_eq!(counts(&conn, "scored-only"), (0, 1, 0));
    let current_generation = crate::db::search_generation(&conn).unwrap();
    assert!(current_generation > cached_generation);
    assert_ne!(
        format!("generation:{cached_generation}"),
        format!("generation:{current_generation}"),
        "a cached generation fingerprint is stale after scored-only persistence"
    );
}

#[test]
fn scorer_count_respects_record_access_and_excludes_graph_only_results() {
    let mut conn = setup();
    insert(
        &mut conn,
        "seed",
        "ScoredCountGraph seed",
        &["scoredcountgraph"],
    );
    insert(&mut conn, "graph-only", "unmatched graph neighbor", &[]);
    add_edge(
        &conn,
        &MemoryEdge {
            source_id: "seed".into(),
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
    let no_record = SearchOptions {
        top_k: 1,
        record_access: false,
        graph_expand_hops: 1,
        ..Default::default()
    };
    hybrid_search(&conn, "ScoredCountGraph", &no_record).unwrap();
    assert_eq!(counts(&conn, "seed"), (0, 0, 0));
    assert_eq!(counts(&conn, "graph-only"), (0, 0, 0));

    conn.execute(
        "UPDATE memories SET scored_count = 7 WHERE id = 'graph-only'",
        [],
    )
    .unwrap();

    let record = SearchOptions {
        record_access: true,
        ..no_record
    };
    let results = hybrid_search(&conn, "ScoredCountGraph", &record).unwrap();
    assert!(results.iter().any(|result| result.entry.id == "graph-only"));
    assert_eq!(counts(&conn, "seed").1, 1);
    assert_eq!(counts(&conn, "graph-only"), (1, 7, 1));
}

#[test]
fn scored_count_is_not_a_ranking_input() {
    let mut conn = setup();
    insert(
        &mut conn,
        "a",
        "ScoredCountIsolation alpha",
        &["scoredcountisolation"],
    );
    insert(
        &mut conn,
        "b",
        "ScoredCountIsolation beta",
        &["scoredcountisolation"],
    );
    let opts = SearchOptions {
        record_access: false,
        mmr_threshold: None,
        ..Default::default()
    };
    let before = hybrid_search(&conn, "ScoredCountIsolation", &opts).unwrap();
    conn.execute("UPDATE memories SET scored_count = scored_count + 999", [])
        .unwrap();
    let after = hybrid_search(&conn, "ScoredCountIsolation", &opts).unwrap();
    let fingerprint = |results: Vec<SearchResult>| {
        results
            .into_iter()
            .map(|r| {
                (
                    r.entry.id,
                    r.score.vector.to_bits(),
                    r.score.fts.to_bits(),
                    r.score.symbolic.to_bits(),
                    r.score.decay.to_bits(),
                    r.score.final_score.to_bits(),
                )
            })
            .collect::<Vec<_>>()
    };
    assert_eq!(fingerprint(before), fingerprint(after));
}
