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
fn add_edge_rejects_illegal_relation() {
    let mut conn = make_conn();
    let e1 = make_entry("illegal-src", "source");
    let e2 = make_entry("illegal-tgt", "target");
    upsert(&mut conn, &e1, false).unwrap();
    upsert(&mut conn, &e2, false).unwrap();

    let edge = MemoryEdge {
        source_id: "illegal-src".into(),
        target_id: "illegal-tgt".into(),
        relation: "shares_entities".into(),
        weight: 0.5,
        metadata: serde_json::json!({}),
        created_at: String::new(),
        valid_from: String::new(),
        valid_to: None,
    };
    let err = add_edge(&conn, &edge).unwrap_err();
    assert!(err.to_string().contains("shares_entities"));

    // Never reached the INSERT.
    let out = get_edges(&conn, "illegal-src", "outgoing", None).unwrap();
    assert!(out.is_empty(), "rejected edge must not be persisted");
}

#[test]
fn add_edge_rejects_related_to_new_writes() {
    let mut conn = make_conn();
    let e1 = make_entry("dep-src", "source");
    let e2 = make_entry("dep-tgt", "target");
    upsert(&mut conn, &e1, false).unwrap();
    upsert(&mut conn, &e2, false).unwrap();

    let edge = MemoryEdge {
        source_id: "dep-src".into(),
        target_id: "dep-tgt".into(),
        relation: "related_to".into(),
        weight: 0.5,
        metadata: serde_json::json!({}),
        created_at: String::new(),
        valid_from: String::new(),
        valid_to: None,
    };
    let err = add_edge(&conn, &edge).unwrap_err();
    assert!(err.to_string().contains("related_to"));
}

#[test]
fn add_edge_accepts_ontology_v1_and_about() {
    let mut conn = make_conn();
    let e1 = make_entry("ok-src", "source");
    let e2 = make_entry("ok-tgt", "target");
    upsert(&mut conn, &e1, false).unwrap();
    upsert(&mut conn, &e2, false).unwrap();

    for relation in ["causes", "about", "supports", "supersedes"] {
        let edge = MemoryEdge {
            source_id: "ok-src".into(),
            target_id: "ok-tgt".into(),
            relation: relation.into(),
            weight: 0.5,
            metadata: serde_json::json!({}),
            created_at: String::new(),
            valid_from: String::new(),
            valid_to: None,
        };
        add_edge(&conn, &edge).unwrap_or_else(|e| panic!("{relation} should be legal: {e}"));
    }
    let out = get_edges(&conn, "ok-src", "outgoing", None).unwrap();
    assert_eq!(out.len(), 4);
}

/// Insert a legacy `related_to` edge with an open `valid_to` (simulating a
/// pre-#773 row) via raw SQL, bypassing `add_edge`'s new-write validation —
/// exactly the grandfathered-read scenario this maintenance fn targets.
fn insert_legacy_related_to_edge(conn: &Connection, source: &str, target: &str) {
    conn.execute(
        "INSERT INTO memory_edges (source_id, target_id, relation, weight, metadata, created_at, valid_from, valid_to)
         VALUES (?1, ?2, 'related_to', 0.5, '{}', ?3, ?3, NULL)",
        params![source, target, "2026-01-01T00:00:00.000Z"],
    )
    .unwrap();
}

#[test]
fn close_related_to_fog_closes_open_rows_and_excludes_from_get_edges() {
    let mut conn = make_conn();
    let e1 = make_entry("fog-src", "source");
    let e2 = make_entry("fog-tgt", "target");
    upsert(&mut conn, &e1, false).unwrap();
    upsert(&mut conn, &e2, false).unwrap();
    insert_legacy_related_to_edge(&conn, "fog-src", "fog-tgt");

    // Red: before closure, the legacy fog edge is still traversable.
    let before = get_edges(&conn, "fog-src", "outgoing", None).unwrap();
    assert_eq!(before.len(), 1, "legacy related_to edge should be open pre-closure");
    assert!(before[0].valid_to.is_none());

    let closed = close_related_to_fog(&conn).unwrap();
    assert_eq!(closed, 1);

    // Green: closed edge leaves get_edges immediately (valid_to <= now).
    let after = get_edges(&conn, "fog-src", "outgoing", None).unwrap();
    assert!(
        after.is_empty(),
        "closed related_to edge must leave get_edges traversal, got {after:?}"
    );
}

#[test]
fn close_related_to_fog_closed_edge_valid_to_exactly_now_is_closed_not_active() {
    // Boundary pin (matches PR #1013's discrimination test): a same-day
    // closed edge must not stay lexically "active" against SQLite's
    // differently-formatted datetime('now') text.
    let mut conn = make_conn();
    let e1 = make_entry("fog-boundary-src", "source");
    let e2 = make_entry("fog-boundary-tgt", "target");
    upsert(&mut conn, &e1, false).unwrap();
    upsert(&mut conn, &e2, false).unwrap();
    insert_legacy_related_to_edge(&conn, "fog-boundary-src", "fog-boundary-tgt");

    close_related_to_fog(&conn).unwrap();

    let out = get_edges(&conn, "fog-boundary-src", "outgoing", None).unwrap();
    assert!(
        out.is_empty(),
        "edge closed at ~now must be excluded immediately, not stay lexically active: {out:?}"
    );
}

#[test]
fn close_related_to_fog_is_idempotent() {
    let mut conn = make_conn();
    let e1 = make_entry("fog-idem-src", "source");
    let e2 = make_entry("fog-idem-tgt", "target");
    upsert(&mut conn, &e1, false).unwrap();
    upsert(&mut conn, &e2, false).unwrap();
    insert_legacy_related_to_edge(&conn, "fog-idem-src", "fog-idem-tgt");

    let first = close_related_to_fog(&conn).unwrap();
    assert_eq!(first, 1);

    // Second pass: nothing left open, so nothing is closed again.
    let second = close_related_to_fog(&conn).unwrap();
    assert_eq!(second, 0, "idempotent re-run must not re-touch already-closed rows");
}

#[test]
fn close_related_to_fog_leaves_other_relations_untouched() {
    let mut conn = make_conn();
    let e1 = make_entry("fog-other-src", "source");
    let e2 = make_entry("fog-other-tgt", "target");
    upsert(&mut conn, &e1, false).unwrap();
    upsert(&mut conn, &e2, false).unwrap();
    add_edge(
        &conn,
        &MemoryEdge {
            source_id: "fog-other-src".into(),
            target_id: "fog-other-tgt".into(),
            relation: "causes".into(),
            weight: 0.9,
            metadata: serde_json::json!({}),
            created_at: String::new(),
            valid_from: String::new(),
            valid_to: None,
        },
    )
    .unwrap();

    let closed = close_related_to_fog(&conn).unwrap();
    assert_eq!(closed, 0, "no related_to rows exist; causes edge must be untouched");

    let out = get_edges(&conn, "fog-other-src", "outgoing", None).unwrap();
    assert_eq!(out.len(), 1);
    assert!(out[0].valid_to.is_none(), "non-related_to edge must stay open");
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
