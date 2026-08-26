use super::*;

fn add_graph_edge(
    conn: &Connection,
    source_id: &str,
    target_id: &str,
    relation: &str,
    weight: f64,
) {
    add_edge(
        conn,
        &MemoryEdge {
            source_id: source_id.to_string(),
            target_id: target_id.to_string(),
            relation: relation.to_string(),
            weight,
            metadata: json!({}),
            created_at: String::new(),
            valid_from: String::new(),
            valid_to: None,
        },
    )
    .unwrap();
}

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

    let bounded_opts = SearchOptions {
        top_k: 1,
        record_access: false,
        graph_expand_hops: 1,
        ..Default::default()
    };
    let bounded = hybrid_search(&conn, "TrendLock", &bounded_opts).unwrap();
    assert_eq!(
        bounded
            .iter()
            .map(|result| result.entry.id.as_str())
            .collect::<Vec<_>>(),
        vec!["seed"],
        "graph expansion may fill unused top_k slots, but must not append hidden rows after the ranked set is already full"
    );

    let expanded_opts = SearchOptions {
        top_k: 3,
        ..bounded_opts
    };
    let results = hybrid_search(&conn, "TrendLock", &expanded_opts).unwrap();
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
        top_k: 2,
        record_access: false,
        graph_expand_hops: 1,
        ..Default::default()
    };
    let (results_on, receipt_on) =
        hybrid_search_with_receipt(&conn, "TrendLock", &opts_on).unwrap();
    let graph_on = receipt_on
        .graph_expansion
        .as_ref()
        .expect("graph_expansion receipt must be Some when sampled");
    assert!(graph_on.enabled, "graph_expand_hops=1 → enabled=true");
    assert!(
        !graph_on.failed,
        "a successful graph expansion is not failed"
    );
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
        ..Default::default()
    };
    let (results_off, receipt_off) =
        hybrid_search_with_receipt(&conn, "TrendLock", &opts_off).unwrap();
    let graph_off = receipt_off
        .graph_expansion
        .as_ref()
        .expect("graph_expansion receipt must still be Some (the function was called)");
    assert!(!graph_off.enabled, "graph_expand_hops=0 → enabled=false");
    assert!(!graph_off.failed, "a disabled graph phase is not failed");
    assert_eq!(graph_off.expanded_count, 0);
    assert!(
        !results_off.iter().any(|r| r.entry.id == "support"),
        "graph-disabled search must not surface the support neighbor"
    );
}

/// tachi#1647 2d: two-hop fixture — `seed` matches the FTS query directly
/// (an ordinary hit); `hop1` is reachable only via a `supports` edge from
/// `seed`; `hop2` is reachable only via an `elaborates` edge from `hop1`,
/// two hops from `seed`. Covers every conformance clause in the packet in
/// one fixture: every injected result is attributable, the ordinary hit is
/// never marked, expansion-off carries no graph fields, and the receipt's
/// per-relation counts match what was actually returned.
#[test]
fn graph_injection_provenance_attributes_every_result_to_its_seed() {
    let mut conn = setup();
    insert(
        &mut conn,
        "seed",
        "TrendLock durable decision rule",
        &["trendlock"],
    );
    insert(
        &mut conn,
        "hop1",
        "One-hop neighbor only reachable by graph",
        &["hop1"],
    );
    insert(
        &mut conn,
        "hop2",
        "Two-hop neighbor only reachable by graph",
        &["hop2"],
    );
    add_edge(
        &conn,
        &MemoryEdge {
            source_id: "seed".to_string(),
            target_id: "hop1".to_string(),
            relation: "supports".to_string(),
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
            source_id: "hop1".to_string(),
            target_id: "hop2".to_string(),
            relation: "elaborates".to_string(),
            weight: 1.0,
            metadata: json!({}),
            created_at: String::new(),
            valid_from: String::new(),
            valid_to: None,
        },
    )
    .unwrap();

    let opts = SearchOptions {
        top_k: 3,
        record_access: false,
        graph_expand_hops: 2,
        ..Default::default()
    };
    let (results, receipt) = hybrid_search_with_receipt(&conn, "TrendLock", &opts).unwrap();
    let ids = results
        .iter()
        .map(|r| r.entry.id.as_str())
        .collect::<Vec<_>>();
    assert_eq!(
        ids,
        vec!["seed", "hop1", "hop2"],
        "both graph hops must be injected to fill the unused top_k slots"
    );

    // Ordinary hit: never marked.
    let seed = results.iter().find(|r| r.entry.id == "seed").unwrap();
    assert!(
        !seed.graph_injected,
        "the FTS-matched seed is not a graph injection"
    );
    assert!(
        seed.graph_provenance.is_none(),
        "an ordinary hit must never carry graph_provenance"
    );

    // Every injected result is attributable: marker + relation + from_id.
    let hop1 = results.iter().find(|r| r.entry.id == "hop1").unwrap();
    assert!(hop1.graph_injected);
    let hop1_provenance = hop1
        .graph_provenance
        .as_ref()
        .expect("hop1 must be attributable — it was discovered via graph BFS");
    assert_eq!(
        hop1_provenance.via_edge, "supports",
        "hop1's discovery edge is seed->hop1 (supports)"
    );
    assert_eq!(hop1_provenance.from_id, "seed");
    assert_eq!(hop1_provenance.distance, 1);
    assert!(
        hop1_provenance.activation.is_finite() && hop1_provenance.activation > 0.0,
        "a real BFS discovery must carry non-zero spreading activation"
    );

    let hop2 = results.iter().find(|r| r.entry.id == "hop2").unwrap();
    assert!(hop2.graph_injected);
    let hop2_provenance = hop2
        .graph_provenance
        .as_ref()
        .expect("hop2 must be attributable — it was discovered via graph BFS");
    assert_eq!(
        hop2_provenance.via_edge, "elaborates",
        "hop2's discovery edge is hop1->hop2 (elaborates)"
    );
    assert_eq!(
        hop2_provenance.from_id, "seed",
        "the parent chain traces back through hop1 to the originating seed"
    );
    assert_eq!(hop2_provenance.distance, 2);
    assert!(
        hop2_provenance.activation.is_finite() && hop2_provenance.activation > 0.0,
        "a real BFS discovery must carry non-zero spreading activation"
    );

    // Receipt counts match what was actually returned, keyed by relation.
    let graph_receipt = receipt
        .graph_expansion
        .as_ref()
        .expect("graph_expansion receipt must be Some when sampled");
    assert_eq!(graph_receipt.expanded_count, 2);
    let expected_counts: std::collections::BTreeMap<String, usize> =
        [("elaborates".to_string(), 1), ("supports".to_string(), 1)]
            .into_iter()
            .collect();
    assert_eq!(graph_receipt.relation_counts, expected_counts);
    assert_eq!(
        graph_receipt.relation_counts.values().sum::<usize>(),
        graph_receipt.expanded_count,
        "relation_counts must always sum to expanded_count"
    );

    // Expansion-off: no graph fields survive on any result — zero new bytes.
    let opts_off = SearchOptions {
        top_k: 1,
        record_access: false,
        graph_expand_hops: 0,
        ..Default::default()
    };
    let (results_off, receipt_off) =
        hybrid_search_with_receipt(&conn, "TrendLock", &opts_off).unwrap();
    assert!(
        results_off
            .iter()
            .all(|r| !r.graph_injected && r.graph_provenance.is_none()),
        "expansion-off must not mark or attribute any result"
    );
    let graph_receipt_off = receipt_off.graph_expansion.as_ref().unwrap();
    assert!(!graph_receipt_off.enabled);
    assert!(graph_receipt_off.relation_counts.is_empty());
}

/// tachi#1647 follow-up: the receipt must name the edge that actually
/// inserted a node during BFS, not a stronger same-depth parent found later.
#[test]
fn graph_injection_provenance_keeps_first_inserting_edge_over_stronger_parent() {
    let mut conn = setup();
    insert(
        &mut conn,
        "seed",
        "NeedleRoot durable decision rule",
        &["needleroot"],
    );
    for id in ["aa-parent", "zz-parent", "leaf"] {
        insert(
            &mut conn,
            id,
            "Graph-only neighbor with no query terms",
            &[id],
        );
    }

    add_graph_edge(&conn, "seed", "aa-parent", "references", 1.0);
    add_graph_edge(&conn, "seed", "zz-parent", "references", 1.0);
    add_graph_edge(&conn, "aa-parent", "leaf", "follows", 0.1);
    add_graph_edge(&conn, "zz-parent", "leaf", "supports", 1.0);

    let opts = SearchOptions {
        top_k: 4,
        record_access: false,
        graph_expand_hops: 2,
        ..Default::default()
    };
    let (results, receipt) = hybrid_search_with_receipt(&conn, "NeedleRoot", &opts).unwrap();
    let leaf = results.iter().find(|r| r.entry.id == "leaf").unwrap();
    let provenance = leaf
        .graph_provenance
        .as_ref()
        .expect("leaf must carry graph injection provenance");
    assert_eq!(
        provenance.via_edge, "follows",
        "aa-parent->leaf sorts first and injected leaf even though zz-parent->leaf is stronger"
    );
    assert_eq!(provenance.from_id, "seed");
    assert_eq!(provenance.distance, 2);

    let expected_counts: std::collections::BTreeMap<String, usize> = results
        .iter()
        .filter_map(|result| {
            result
                .graph_provenance
                .as_ref()
                .map(|provenance| provenance.via_edge.clone())
        })
        .fold(std::collections::BTreeMap::new(), |mut acc, relation| {
            *acc.entry(relation).or_insert(0) += 1;
            acc
        });
    let graph_receipt = receipt
        .graph_expansion
        .as_ref()
        .expect("graph_expansion receipt must be Some when sampled");
    assert_eq!(
        graph_receipt.relation_counts, expected_counts,
        "receipt relation_counts must be derived from the returned rows' own provenance"
    );
    assert_eq!(graph_receipt.relation_counts.get("follows"), Some(&1));
}

/// Equal raw edge weights do not make provenance consult scorer strength:
/// attribution stays the first eligible edge processed by BFS batch order.
#[test]
fn graph_injection_provenance_pins_equal_weight_tie_to_first_processed_edge() {
    let mut conn = setup();
    insert(
        &mut conn,
        "seed",
        "TieRoot durable decision rule",
        &["tieroot"],
    );
    for id in ["aa-parent", "zz-parent", "leaf"] {
        insert(
            &mut conn,
            id,
            "Graph-only neighbor with no query terms",
            &[id],
        );
    }

    add_graph_edge(&conn, "seed", "aa-parent", "references", 1.0);
    add_graph_edge(&conn, "seed", "zz-parent", "references", 1.0);
    add_graph_edge(&conn, "aa-parent", "leaf", "references", 1.0);
    add_graph_edge(&conn, "zz-parent", "leaf", "supports", 1.0);

    let opts = SearchOptions {
        top_k: 4,
        record_access: false,
        graph_expand_hops: 2,
        ..Default::default()
    };
    let (results, _) = hybrid_search_with_receipt(&conn, "TieRoot", &opts).unwrap();
    let leaf = results.iter().find(|r| r.entry.id == "leaf").unwrap();
    let provenance = leaf
        .graph_provenance
        .as_ref()
        .expect("leaf must carry graph injection provenance");
    assert_eq!(
        provenance.via_edge, "references",
        "aa-parent->leaf and zz-parent->leaf have equal edge.weight; the first processed edge wins"
    );
    assert_eq!(provenance.from_id, "seed");
    assert_eq!(provenance.distance, 2);
}

#[test]
fn as_of_search_anchors_graph_edge_validity_at_the_instant() {
    // Hyperion #3010 wiring: hybrid_search must route an as_of search's graph
    // expansion through the instant-anchored edge predicate. A future-dated
    // edge (valid_from 2027) links the seed to the neighbor: a plain expanded
    // search injects the neighbor, a 2026-01-01 replay must not — entry-level
    // validity alone cannot tell these apart because both entries carry
    // window-free validity.
    let mut conn = setup();
    let mut seed = memory_entry("seed", "ReplayEdge probe text", &["replayedge"]);
    seed.valid_from = "2019-01-01T00:00:00Z".to_string();
    insert_entry(&mut conn, seed);
    insert(
        &mut conn,
        "future-nbr",
        "Only reachable through a future-dated edge",
        &["irrelevantkw"],
    );
    add_edge(
        &conn,
        &MemoryEdge {
            source_id: "seed".to_string(),
            target_id: "future-nbr".to_string(),
            relation: "supports".to_string(),
            weight: 1.0,
            metadata: json!({}),
            created_at: "2026-08-01T00:00:00Z".to_string(),
            valid_from: "2027-01-01T00:00:00Z".to_string(),
            valid_to: None,
        },
    )
    .unwrap();

    let plain_opts = SearchOptions {
        top_k: 5,
        record_access: false,
        graph_expand_hops: 1,
        ..Default::default()
    };
    let plain = hybrid_search(&conn, "ReplayEdge probe text", &plain_opts).unwrap();
    let plain_ids: Vec<&str> = plain.iter().map(|r| r.entry.id.as_str()).collect();
    assert!(
        plain_ids.contains(&"future-nbr"),
        "now-anchored expansion ignores edge valid_from: {plain_ids:?}"
    );

    let replay_opts = SearchOptions {
        as_of: Some("2026-01-01T00:00:00Z".to_string()),
        ..plain_opts
    };
    let replay = hybrid_search(&conn, "ReplayEdge probe text", &replay_opts).unwrap();
    let replay_ids: Vec<&str> = replay.iter().map(|r| r.entry.id.as_str()).collect();
    assert_eq!(
        replay_ids,
        vec!["seed"],
        "the future edge must not leak into the replay"
    );
}
