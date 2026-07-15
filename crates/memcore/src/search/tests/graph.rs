use super::*;

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
            // similar_to shares related_to's 0.55 weight class (tachi#773 S1:
            // related_to is legal-but-deprecated for new writes; this test
            // only cares about the weak-vs-strong edge ranking, not the
            // specific deprecated relation name).
            relation: "similar_to".to_string(),
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

/// #1097 S1 D7-②: extend the existing graph-enabled fixture (above) rather
/// than minting a parallel one. Asserts the receipt covers BOTH branches of
/// `append_graph_expansion`'s early-return guard (graph_expansion.rs:24-26):
/// the "graph enabled" branch (hops > 0, expansion runs) and the "graph
/// disabled" branch (hops == 0, immediate return). Red on the
/// pre-instrumentation code: `hybrid_search_with_receipt` and
/// `GraphPhaseReceipt` do not exist there.
#[test]
fn receipt_covers_graph_enabled_and_disabled_branches() {
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

    // Graph ENABLED — graph_expand_hops=1, the early-return guard at
    // graph_expansion.rs:24-26 falls through, expansion runs and surfaces
    // the "support" neighbor.
    let opts_on = SearchOptions {
        top_k: 1,
        record_access: false,
        graph_expand_hops: 1,
        collect_phase_receipt: true,
        ..Default::default()
    };
    let (results_on, receipt_on) =
        hybrid_search_with_receipt(&conn, "TrendLock", &opts_on).unwrap();
    let graph_on = receipt_on
        .graph_expansion
        .as_ref()
        .expect("graph_expansion receipt must be Some when sampled");
    assert!(graph_on.enabled, "graph_expand_hops=1 → enabled=true");
    assert_eq!(
        graph_on.expanded_count, 1,
        "exactly the support neighbor should be expanded"
    );
    assert!(
        results_on.iter().any(|r| r.entry.id == "support"),
        "graph expansion must surface the support neighbor"
    );

    // Graph DISABLED — graph_expand_hops=0, the early-return guard at
    // graph_expansion.rs:24-26 fires immediately. The receipt still records
    // the phase (enabled=false) so "graph disabled" is visibly distinct from
    // "graph ran but expanded nothing".
    let opts_off = SearchOptions {
        top_k: 1,
        record_access: false,
        graph_expand_hops: 0,
        collect_phase_receipt: true,
        ..Default::default()
    };
    let (results_off, receipt_off) =
        hybrid_search_with_receipt(&conn, "TrendLock", &opts_off).unwrap();
    let graph_off = receipt_off
        .graph_expansion
        .as_ref()
        .expect("graph_expansion receipt must still be Some (the function was called)");
    assert!(!graph_off.enabled, "graph_expand_hops=0 → enabled=false");
    assert_eq!(graph_off.expanded_count, 0);
    assert!(
        !results_off.iter().any(|r| r.entry.id == "support"),
        "graph-disabled search must not surface the support neighbor"
    );
}
