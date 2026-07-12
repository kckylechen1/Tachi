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
