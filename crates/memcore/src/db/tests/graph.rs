use super::*;

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
    let r1 = graph_expand(&conn, &["a".into()], 1, None).unwrap();
    assert_eq!(r1.entries.len(), 1); // should find b
    assert!(r1.distances.contains_key("b"));
    assert!(!r1.distances.contains_key("c")); // c is 2 hops

    // Expand 2 hops from "a"
    let r2 = graph_expand(&conn, &["a".into()], 2, None).unwrap();
    assert_eq!(r2.entries.len(), 2); // b and c
    assert!(r2.distances.contains_key("c"));
    assert!(!r2.distances.contains_key("d")); // d is disconnected
}

#[test]
fn closed_edge_valid_to_now_is_excluded_immediately() {
    // Sol correction 4 (#773 v3): add_edge stores valid_to UNNORMALIZED, while
    // edge-active reads compare that raw text against SQLite's differently
    // formatted datetime('now'). An RFC3339 valid_to (e.g. "...T...Z") sorts
    // lexically GREATER than SQLite's "YYYY-MM-DD HH:MM:SS" datetime('now')
    // output regardless of actual instant, so a same-day closed edge stays
    // "active" forever under the raw/unnormalized write path.
    let mut conn = make_conn();
    let e1 = make_entry("close-e1", "source");
    let e2 = make_entry("close-e2", "target");
    upsert(&mut conn, &e1, false).unwrap();
    upsert(&mut conn, &e2, false).unwrap();

    let edge = MemoryEdge {
        source_id: "close-e1".into(),
        target_id: "close-e2".into(),
        relation: "causes".into(),
        weight: 1.0,
        metadata: serde_json::json!({}),
        created_at: String::new(),
        valid_from: String::new(),
        valid_to: None,
    };
    add_edge(&conn, &edge).unwrap();

    // Sanity: edge is active before closure.
    let before = get_edges(&conn, "close-e1", "outgoing", None).unwrap();
    assert_eq!(before.len(), 1, "edge should be active before closure");

    // Close the edge "now" using the same RFC3339-with-millis format the
    // rest of the codebase uses for timestamps (see now_utc_iso).
    let now_rfc3339 = now_utc_iso();
    let mut closed_edge = edge.clone();
    closed_edge.valid_to = Some(now_rfc3339);
    add_edge(&conn, &closed_edge).unwrap();

    let after = get_edges(&conn, "close-e1", "outgoing", None).unwrap();
    assert!(
        after.is_empty(),
        "edge closed with valid_to = now should be excluded immediately, got: {after:?}"
    );

    let incoming_after = get_edges(&conn, "close-e2", "incoming", None).unwrap();
    assert!(
        incoming_after.is_empty(),
        "incoming direction should also exclude the closed edge, got: {incoming_after:?}"
    );
}

#[test]
fn edge_valid_to_exactly_now_is_closed_not_active() {
    // Half-open interval semantics: [valid_from, valid_to) — an edge whose
    // valid_to is exactly "now" must be treated as closed (excluded), not
    // active. This pins the boundary condition down (`>` vs `>=`/off-by-one
    // after the format-agnostic fix lands).
    let mut conn = make_conn();
    let e1 = make_entry("boundary-e1", "source");
    let e2 = make_entry("boundary-e2", "target");
    upsert(&mut conn, &e1, false).unwrap();
    upsert(&mut conn, &e2, false).unwrap();

    // Insert directly with a valid_to that exactly equals the DB's current
    // datetime('now') reading, bypassing add_edge's write-path normalization
    // so this test pins the read-side boundary comparison in isolation.
    let db_now: String = conn
        .query_row("SELECT datetime('now')", [], |row| row.get(0))
        .unwrap();
    conn.execute(
        "INSERT INTO memory_edges
         (source_id, target_id, relation, weight, metadata, created_at, valid_from, valid_to)
         VALUES (?1, ?2, ?3, ?4, '{}', ?5, ?5, ?6)",
        rusqlite::params![
            "boundary-e1",
            "boundary-e2",
            "causes",
            1.0,
            "2026-06-14T00:00:00.000Z",
            db_now,
        ],
    )
    .unwrap();

    let out = get_edges(&conn, "boundary-e1", "outgoing", None).unwrap();
    assert!(
        out.is_empty(),
        "edge with valid_to exactly equal to datetime('now') must be closed (half-open [from,to)), got: {out:?}"
    );
}

#[test]
fn delete_cascades_edges() {
    let mut conn = make_conn();
    let e1 = make_entry("del-e1", "source");
    let e2 = make_entry("del-e2", "target");
    upsert(&mut conn, &e1, false).unwrap();
    upsert(&mut conn, &e2, false).unwrap();
    add_edge(
        &conn,
        &MemoryEdge {
            source_id: "del-e1".into(),
            target_id: "del-e2".into(),
            relation: "causes".into(),
            weight: 1.0,
            metadata: serde_json::json!({}),
            created_at: String::new(),
            valid_from: String::new(),
            valid_to: None,
        },
    )
    .unwrap();

    delete(&mut conn, "del-e1", false).unwrap();
    let edges = get_edges(&conn, "del-e2", "both", None).unwrap();
    assert!(edges.is_empty(), "edges should be cleaned up on delete");
}
