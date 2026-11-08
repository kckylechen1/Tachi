use super::*;
use crate::db::graph::GRAPH_EXPANSION_EDGE_LIMIT;

#[test]
fn graph_add_and_get_edges() {
    let mut conn = make_conn();
    let e1 = make_entry("g1", "cause event");
    let e2 = make_entry("g2", "effect event");
    upsert(&mut conn, &e1, false).unwrap();
    upsert(&mut conn, &e2, false).unwrap();

    let edge = MemoryEdge {
        source_id: "g1".into(),
        target_id: "g2".into(),
        relation: "causes".into(),
        weight: 0.9,
        metadata: serde_json::json!({}),
        created_at: String::new(),
        valid_from: String::new(),
        valid_to: None,
    };
    add_edge(&conn, &edge).unwrap();

    let out = get_edges(&conn, "g1", "outgoing", None).unwrap();
    assert_eq!(out.len(), 1);
    assert_eq!(out[0].target_id, "g2");
    assert_eq!(out[0].relation, "causes");

    let inc = get_edges(&conn, "g2", "incoming", None).unwrap();
    assert_eq!(inc.len(), 1);
    assert_eq!(inc[0].source_id, "g1");
}

#[test]
fn graph_get_edges_returns_row_decode_errors() {
    let mut conn = make_conn();
    let e1 = make_entry("bad-edge-source", "source");
    let e2 = make_entry("bad-edge-target", "target");
    upsert(&mut conn, &e1, false).unwrap();
    upsert(&mut conn, &e2, false).unwrap();

    conn.execute(
        "INSERT INTO memory_edges
         (source_id, target_id, relation, weight, metadata, created_at, valid_from, valid_to)
         VALUES (?1, ?2, ?3, ?4, '{}', ?5, ?5, NULL)",
        rusqlite::params![
            "bad-edge-source",
            "bad-edge-target",
            "causes",
            "not-a-number",
            "2026-06-14T00:00:00Z"
        ],
    )
    .unwrap();

    let err = get_edges(&conn, "bad-edge-source", "outgoing", None).unwrap_err();
    let msg = err.to_string();
    assert!(
        msg.contains("Invalid column type") || msg.contains("not-a-number"),
        "expected row decode error, got: {msg}"
    );
}

#[test]
fn graph_limited_queries_stop_in_sql_before_decoding_rows_past_the_ceiling() {
    let mut conn = make_conn();
    for id in ["limit-root", "limit-good-a", "limit-good-b", "limit-z-bad"] {
        upsert(&mut conn, &make_entry(id, id), false).unwrap();
    }
    for target in ["limit-good-a", "limit-good-b"] {
        add_edge(
            &conn,
            &MemoryEdge {
                source_id: "limit-root".into(),
                target_id: target.into(),
                relation: "supports".into(),
                weight: 1.0,
                metadata: serde_json::json!({}),
                created_at: String::new(),
                valid_from: String::new(),
                valid_to: None,
            },
        )
        .unwrap();
    }
    conn.execute(
        "INSERT INTO memory_edges
         (source_id, target_id, relation, weight, metadata, created_at, valid_from, valid_to)
         VALUES (?1, ?2, ?3, ?4, '{}', ?5, ?5, NULL)",
        rusqlite::params![
            "limit-root",
            "limit-z-bad",
            "supports",
            "not-a-number",
            "2026-07-25T00:00:00Z"
        ],
    )
    .unwrap();

    let edges = get_edges_limited(&conn, "limit-root", "outgoing", None, 2).unwrap();
    assert_eq!(edges.len(), 2);
    assert!(get_edges_limited(&conn, "limit-root", "outgoing", None, 0)
        .unwrap()
        .is_empty());

    let expanded = graph_expand_limited(&conn, &["limit-root".into()], 1, None, 2, false).unwrap();
    assert_eq!(expanded.edges.len(), 2);
    assert_eq!(expanded.entries.len(), 2);
}

#[test]
fn graph_limited_queries_do_not_let_seen_parent_edges_consume_the_budget() {
    let mut conn = make_conn();
    for id in ["a-root", "z-child", "z-grandchild"] {
        upsert(&mut conn, &make_entry(id, id), false).unwrap();
    }
    for (source_id, target_id) in [("a-root", "z-child"), ("z-child", "z-grandchild")] {
        add_edge(
            &conn,
            &MemoryEdge {
                source_id: source_id.into(),
                target_id: target_id.into(),
                relation: "supports".into(),
                weight: 1.0,
                metadata: serde_json::json!({}),
                created_at: String::new(),
                valid_from: String::new(),
                valid_to: None,
            },
        )
        .unwrap();
    }

    let expanded = graph_expand_limited(&conn, &["a-root".into()], 2, None, 2, false).unwrap();
    assert_eq!(expanded.edges.len(), 2);
    assert_eq!(expanded.entries.len(), 2);
}

#[test]
fn graph_expand_bfs() {
    let mut conn = make_conn();
    // Create chain: a -> b -> c
    for id in &["a", "b", "c", "d"] {
        upsert(&mut conn, &make_entry(id, &format!("node {}", id)), false).unwrap();
    }
    add_edge(
        &conn,
        &MemoryEdge {
            source_id: "a".into(),
            target_id: "b".into(),
            relation: "follows".into(),
            weight: 1.0,
            metadata: serde_json::json!({}),
            created_at: String::new(),
            valid_from: String::new(),
            valid_to: None,
        },
    )
    .unwrap();
    add_edge(
        &conn,
        &MemoryEdge {
            source_id: "b".into(),
            target_id: "c".into(),
            relation: "follows".into(),
            weight: 1.0,
            metadata: serde_json::json!({}),
            created_at: String::new(),
            valid_from: String::new(),
            valid_to: None,
        },
    )
    .unwrap();
    // d is disconnected

    // Expand 1 hop from "a"
    let r1 = graph_expand(&conn, &["a".into()], 1, None, false).unwrap();
    assert_eq!(r1.entries.len(), 1); // should find b
    assert!(r1.distances.contains_key("b"));
    assert!(!r1.distances.contains_key("c")); // c is 2 hops

    // Expand 2 hops from "a"
    let r2 = graph_expand(&conn, &["a".into()], 2, None, false).unwrap();
    assert_eq!(r2.entries.len(), 2); // b and c
    assert!(r2.distances.contains_key("c"));
    assert!(!r2.distances.contains_key("d")); // d is disconnected
}

#[test]
fn graph_expand_limited_records_first_injection_edge_at_traversal_time() {
    let mut conn = make_conn();
    for id in ["seed", "aa-parent", "zz-parent", "leaf"] {
        upsert(&mut conn, &make_entry(id, &format!("node {id}")), false).unwrap();
    }

    let add = |source_id: &str, target_id: &str, relation: &str, weight: f64| {
        add_edge(
            &conn,
            &MemoryEdge {
                source_id: source_id.into(),
                target_id: target_id.into(),
                relation: relation.into(),
                weight,
                metadata: serde_json::json!({}),
                created_at: String::new(),
                valid_from: String::new(),
                valid_to: None,
            },
        )
        .unwrap();
    };
    add("seed", "aa-parent", "references", 1.0);
    add("seed", "zz-parent", "references", 1.0);
    add("aa-parent", "leaf", "follows", 0.1);
    add("zz-parent", "leaf", "supports", 1.0);

    let expanded =
        graph_expand_limited(&conn, &["seed".into()], 2, None, usize::MAX, false).unwrap();
    let leaf_injection = expanded
        .injection_edges
        .get("leaf")
        .expect("leaf insertion edge must be recorded at first visit");
    assert_eq!(leaf_injection.source_id, "aa-parent");
    assert_eq!(leaf_injection.target_id, "leaf");
    assert_eq!(
        leaf_injection.relation, "follows",
        "BFS provenance records the first processed edge, not the later stronger parent"
    );
    assert_eq!(leaf_injection.weight, 0.1);
    assert_eq!(leaf_injection.depth, 2);
    assert_eq!(expanded.distances.get("leaf"), Some(&2));
}

/// tachi#1569 (cross-vendor review, BUG 1): `graph_expand` is a public read
/// surface with no post-expansion Rust filter of its own. On the Wiki store an
/// ordinary seed that neighbours an internal row used to hand that row's full
/// body back in `GraphExpandResult.entries`.
///
/// The `false`/`true` pair is the discrimination: same graph, same seed —
/// deleting the store-identity argument from the entry fetch turns the second
/// half of this test red.
#[test]
fn graph_expansion_entries_are_gated_on_store_identity() {
    let mut conn = make_conn();
    upsert(&mut conn, &make_entry("gx-seed", "public seed row"), false).unwrap();
    upsert(
        &mut conn,
        &make_entry("gx-neighbour", "ordinary neighbour row"),
        false,
    )
    .unwrap();
    // An internal Wiki row reachable in one hop from the public seed. Its
    // internal identity is the `metadata.wiki_log` flag, which `upsert`
    // refuses to mint (the operation-log identity is reserved for the trusted
    // Wiki log seam), so the fixture sets it afterwards on the raw connection
    // — the same shape `search::tests::noise::hybrid_hides_operation_logs`
    // uses. The primary key is left alone.
    upsert(
        &mut conn,
        &make_entry("gx-internal", "wiki operation log body"),
        false,
    )
    .unwrap();
    conn.execute(
        "UPDATE memories SET metadata = json_set(COALESCE(NULLIF(metadata, ''), '{}'), '$.wiki_log', 1) WHERE id = 'gx-internal'",
        [],
    )
    .unwrap();

    // `references` for both arms: the seed points at each neighbour the same
    // way, so the only difference between them is the target's internal-ness —
    // which is the single variable this test is about. (`related_to` is
    // retired on new writes and is the relation `close_related_to_fog` closes,
    // so it would have been the wrong shape here even if it still validated.)
    for target in ["gx-neighbour", "gx-internal"] {
        add_edge(
            &conn,
            &MemoryEdge {
                source_id: "gx-seed".into(),
                target_id: target.into(),
                relation: "references".into(),
                weight: 1.0,
                metadata: serde_json::json!({}),
                created_at: String::new(),
                valid_from: String::new(),
                valid_to: None,
            },
        )
        .unwrap();
    }

    let ungated = graph_expand(&conn, &["gx-seed".into()], 1, None, false).unwrap();
    let ungated_ids = ungated
        .entries
        .iter()
        .map(|entry| entry.id.as_str())
        .collect::<std::collections::HashSet<_>>();
    assert!(
        ungated_ids.contains("gx-neighbour") && ungated_ids.contains("gx-internal"),
        "a non-wiki store must expand exactly as it did before: {ungated_ids:?}"
    );

    let gated = graph_expand(&conn, &["gx-seed".into()], 1, None, true).unwrap();
    let gated_ids = gated
        .entries
        .iter()
        .map(|entry| entry.id.as_str())
        .collect::<std::collections::HashSet<_>>();
    assert!(
        gated_ids.contains("gx-neighbour"),
        "gating must not cost the caller ordinary neighbours: {gated_ids:?}"
    );
    assert!(
        !gated_ids.contains("gx-internal"),
        "the wiki store must not hand an internal row back through graph expansion: {gated_ids:?}"
    );
    // The traversal itself is unchanged: the edge and the distance still
    // record that the graph reaches that node (see `graph_expand_limited`'s
    // note on why edges are not withheld).
    assert!(gated
        .edges
        .iter()
        .any(|edge| edge.target_id == "gx-internal"));
    assert!(gated.distances.contains_key("gx-internal"));
}

#[test]
fn graph_expand_as_of_anchors_edge_validity_at_the_instant() {
    // Hyperion #3010: a point-in-time expansion must anchor BOTH edge bounds
    // at the requested instant — a replay neither leaks an edge created after
    // the instant nor loses an edge that was valid then but has since
    // expired. Plain expansion keeps the historical now-anchored predicate.
    const REPLAY_AS_OF: &str = "2026-01-01T00:00:00Z";
    let mut conn = make_conn();
    for id in [
        "root",
        "always-nbr",
        "future-nbr",
        "lapsed-nbr",
        "blank-nbr",
        "starts-at-as-of",
        "ends-at-as-of",
    ] {
        upsert(&mut conn, &make_entry(id, "graph as-of fixture"), false).unwrap();
    }
    let edge = |target: &str, valid_from: &str, valid_to: Option<&str>| MemoryEdge {
        source_id: "root".into(),
        target_id: target.into(),
        relation: "follows".into(),
        weight: 1.0,
        metadata: serde_json::json!({}),
        created_at: String::new(),
        valid_from: valid_from.into(),
        valid_to: valid_to.map(str::to_string),
    };
    add_edge(&conn, &edge("always-nbr", "2019-01-01T00:00:00Z", None)).unwrap();
    // Not yet valid until 2027: visible to a now-anchored read, hidden to a
    // 2026 replay.
    add_edge(&conn, &edge("future-nbr", "2027-01-01T00:00:00Z", None)).unwrap();
    // Valid 2019→2026-06: expired today, still valid at a 2026-01-01 replay.
    add_edge(
        &conn,
        &edge(
            "lapsed-nbr",
            "2019-01-01T00:00:00Z",
            Some("2026-06-01T00:00:00Z"),
        ),
    )
    .unwrap();
    // The graph interval is half-open: an edge beginning exactly at the
    // replay instant is active, while one ending exactly at it is already
    // closed.
    add_edge(&conn, &edge("starts-at-as-of", REPLAY_AS_OF, None)).unwrap();
    add_edge(
        &conn,
        &edge("ends-at-as-of", "2019-01-01T00:00:00Z", Some(REPLAY_AS_OF)),
    )
    .unwrap();
    // A LEGACY row with a genuinely empty valid_from (written before
    // write_edge_row started defaulting it to created_at) stays always-valid.
    // New edges with an empty field normalize valid_from to now on write, so
    // they are correctly invisible to earlier replays — this row must bypass
    // add_edge to reproduce the pre-normalization shape.
    conn.execute(
        "INSERT INTO memory_edges (source_id, target_id, relation, weight, metadata, created_at, valid_from, valid_to)          VALUES ('root', 'blank-nbr', 'follows', 1.0, '{}', '', '', NULL)",
        [],
    )
    .unwrap();

    let plain = graph_expand(&conn, &["root".into()], 1, None, false).unwrap();
    assert!(plain.distances.contains_key("always-nbr"));
    assert!(
        plain.distances.contains_key("future-nbr"),
        "now-anchored reads ignore valid_from (historical behavior)"
    );
    assert!(plain.distances.contains_key("blank-nbr"));
    assert!(
        !plain.distances.contains_key("lapsed-nbr"),
        "now-anchored reads drop expired edges (historical behavior)"
    );

    let replay = graph_expand_as_of(&conn, &["root".into()], 1, None, false, REPLAY_AS_OF).unwrap();
    assert!(replay.distances.contains_key("always-nbr"));
    assert!(
        replay.distances.contains_key("lapsed-nbr"),
        "an edge expired today was valid at the replay instant"
    );
    assert!(
        replay.distances.contains_key("starts-at-as-of"),
        "valid_from == as_of must be included"
    );
    assert!(
        !replay.distances.contains_key("ends-at-as-of"),
        "valid_to == as_of must be excluded"
    );
    assert!(replay.distances.contains_key("blank-nbr"));
    assert!(
        !replay.distances.contains_key("future-nbr"),
        "an edge created after the instant must not leak into the replay"
    );

    let later = graph_expand_as_of(
        &conn,
        &["root".into()],
        1,
        None,
        false,
        "2028-01-01T00:00:00Z",
    )
    .unwrap();
    assert!(later.distances.contains_key("future-nbr"));

    // Sub-second precision: an edge beginning 500ms AFTER
    // the instant must not leak in. datetime() truncates to whole seconds
    // and admitted it; julianday() keeps the half-open interval aligned
    // with the entry-level Chrono comparison.
    upsert(
        &mut conn,
        &make_entry("ms-nbr", "sub-second fixture"),
        false,
    )
    .unwrap();
    add_edge(&conn, &edge("ms-nbr", "2026-01-01T00:00:00.500Z", None)).unwrap();
    let at_instant = graph_expand_as_of(
        &conn,
        &["root".into()],
        1,
        None,
        false,
        "2026-01-01T00:00:00.000Z",
    )
    .unwrap();
    assert!(
        !at_instant.distances.contains_key("ms-nbr"),
        "an edge starting 500ms after the instant must be hidden"
    );
    let after_edge = graph_expand_as_of(
        &conn,
        &["root".into()],
        1,
        None,
        false,
        "2026-01-01T00:00:00.600Z",
    )
    .unwrap();
    assert!(after_edge.distances.contains_key("ms-nbr"));

    // A malformed instant is a loud error at the public boundary, never a
    // silently partial graph (julianday(garbage) is NULL, which would drop
    // the validity predicate and hide every ordinary edge).
    assert!(
        graph_expand_as_of(&conn, &["root".into()], 1, None, false, "not-a-timestamp").is_err()
    );

    // An instant Chrono accepts but SQLite cannot anchor (year +10000 is
    // outside the julianday range) must fail loud the same way — NULL from
    // julianday would otherwise silently drop ordinary edges again.
    assert!(graph_expand_as_of(
        &conn,
        &["root".into()],
        1,
        None,
        false,
        "+10000-01-01T00:00:00Z"
    )
    .is_err());
}

#[test]
fn graph_expansion_bounds_edge_decoding_and_node_hydration() {
    let mut conn = make_conn();
    upsert(&mut conn, &make_entry("cap-root", "cap fixture"), false).unwrap();
    for i in 0..(GRAPH_EXPANSION_EDGE_LIMIT + 10) {
        let target = format!("cap-nbr-{i:05}");
        upsert(&mut conn, &make_entry(&target, "cap neighbor"), false).unwrap();
        add_edge(
            &conn,
            &MemoryEdge {
                source_id: "cap-root".into(),
                target_id: target,
                relation: "follows".into(),
                weight: 1.0,
                metadata: serde_json::json!({}),
                created_at: "2019-01-01T00:00:00Z".into(),
                valid_from: "2019-01-01T00:00:00Z".into(),
                valid_to: Some("2020-01-01T00:00:00Z".into()),
            },
        )
        .unwrap();
    }
    // The SQL edge ceiling must apply before decoding, and the node ceiling
    // before hydrating memories. Each poisoned row lies just beyond its budget.
    conn.execute(
        "UPDATE memory_edges SET weight = 'not-a-number' WHERE target_id = ?1",
        [format!("cap-nbr-{GRAPH_EXPANSION_EDGE_LIMIT:05}")],
    )
    .unwrap();
    conn.execute(
        "UPDATE memories SET importance = 'not-a-number' WHERE id = 'cap-nbr-00049'",
        [],
    )
    .unwrap();

    let replay = graph_expand_as_of(
        &conn,
        &["cap-root".into()],
        1,
        None,
        false,
        "2019-06-01T00:00:00Z",
    )
    .unwrap();
    conn.execute("UPDATE memory_edges SET valid_to = NULL", [])
        .unwrap();
    let current = graph_expand(&conn, &["cap-root".into()], 1, None, false).unwrap();
    for expanded in [replay, current] {
        assert_eq!(
            expanded.distances.len(),
            50,
            "the seed consumes one node slot"
        );
        assert_eq!(expanded.entries.len(), 49);
        assert_eq!(expanded.edges.len(), 49);
        assert!(expanded.distances.contains_key("cap-nbr-00048"));
        assert!(!expanded.distances.contains_key("cap-nbr-00049"));
        assert_eq!(
            expanded.injection_edges["cap-nbr-00048"].source_id,
            "cap-root"
        );
    }
}
