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

/// Sol post-adjudication kill-test ②: the generic `add_edge` choke point —
/// the same one the NAPI `add_edge` surface and continuity projection funnel
/// through — must REJECT every #772 grandfathered relation and leave zero rows.
/// This is what shuts the "launder a string relation into the graph" path.
#[test]
fn add_edge_rejects_component_governance_grandfathered_on_generic_path() {
    let mut conn = make_conn();
    let e1 = make_entry("gf-src", "source");
    let e2 = make_entry("gf-tgt", "target");
    upsert(&mut conn, &e1, false).unwrap();
    upsert(&mut conn, &e2, false).unwrap();

    for relation in ["owns", "consumes", "backflow_candidate", "blocked_by"] {
        let edge = MemoryEdge {
            source_id: "gf-src".into(),
            target_id: "gf-tgt".into(),
            relation: relation.into(),
            weight: 0.5,
            metadata: serde_json::json!({}),
            created_at: String::new(),
            valid_from: String::new(),
            valid_to: None,
        };
        let err = add_edge(&conn, &edge).unwrap_err();
        assert!(
            err.to_string().contains(relation),
            "generic add_edge must reject grandfathered relation '{relation}', got: {err}"
        );
    }

    // Never reached the INSERT for any of the four.
    let out = get_edges(&conn, "gf-src", "outgoing", None).unwrap();
    assert!(
        out.is_empty(),
        "grandfathered relations must not persist via the generic path, got {out:?}"
    );
}

/// Sol post-adjudication kill-test ③: the typed, caller-scoped door
/// `add_component_governance_edge` accepts exactly the four grandfathered
/// relations (via the closed enum) and persists them — the one sanctioned
/// seeding path.
#[test]
fn add_component_governance_edge_accepts_exactly_the_four_typed_relations() {
    let mut conn = make_conn();
    // Distinct target per relation so the (source, target, relation) upsert key
    // keeps all four as separate rows.
    let src = make_entry("cg-src", "component");
    upsert(&mut conn, &src, false).unwrap();
    let variants = [
        ComponentGovernanceRelation::Owns,
        ComponentGovernanceRelation::Consumes,
        ComponentGovernanceRelation::BackflowCandidate,
        ComponentGovernanceRelation::BlockedBy,
    ];
    for (i, relation) in variants.iter().enumerate() {
        let tgt_id = format!("cg-tgt-{i}");
        let tgt = make_entry(&tgt_id, "component");
        upsert(&mut conn, &tgt, false).unwrap();
        let edge = MemoryEdge {
            source_id: "cg-src".into(),
            target_id: tgt_id.clone(),
            // Deliberately wrong string to prove the enum (not this field) is
            // authoritative for the stored relation.
            relation: "IGNORED".into(),
            weight: 1.0,
            metadata: serde_json::json!({}),
            created_at: String::new(),
            valid_from: String::new(),
            valid_to: None,
        };
        add_component_governance_edge(&conn, &edge, *relation)
            .unwrap_or_else(|e| panic!("typed door must accept {}: {e}", relation.as_str()));
    }

    let out = get_edges(&conn, "cg-src", "outgoing", None).unwrap();
    assert_eq!(out.len(), 4, "all four typed governance edges must persist");
    let mut stored: Vec<String> = out.iter().map(|e| e.relation.clone()).collect();
    stored.sort();
    assert_eq!(
        stored,
        vec![
            "backflow_candidate".to_string(),
            "blocked_by".to_string(),
            "consumes".to_string(),
            "owns".to_string(),
        ],
        "typed door must store the enum's as_str(), not edge.relation"
    );
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
    assert_eq!(
        before.len(),
        1,
        "legacy related_to edge should be open pre-closure"
    );
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
fn ensure_anchor_composes_with_add_edge_via_about_relation() {
    // tachi#773 item 4 guard (d): both edge endpoints must exist in the same
    // physical DB. Since ensure_anchor and add_edge share one Connection,
    // this composes naturally — a memory row can point an `about` edge at a
    // freshly-ensured anchor with no cross-DB id smuggling possible.
    let mut conn = make_conn();
    let memory = make_entry("about-src", "note about issue 773");
    upsert(&mut conn, &memory, false).unwrap();

    let anchor = ensure_anchor(&conn, AnchorKind::Issue, "kckylechen1/tachi:773").unwrap();
    assert_eq!(
        anchor,
        anchor_id(AnchorKind::Issue, "kckylechen1/tachi:773")
    );

    add_edge(
        &conn,
        &MemoryEdge {
            source_id: "about-src".into(),
            target_id: anchor.clone(),
            relation: "about".into(),
            weight: 1.0,
            metadata: serde_json::json!({}),
            created_at: String::new(),
            valid_from: String::new(),
            valid_to: None,
        },
    )
    .unwrap();

    let out = get_edges(&conn, "about-src", "outgoing", None).unwrap();
    assert_eq!(out.len(), 1);
    assert_eq!(out[0].target_id, anchor);
    assert_eq!(out[0].relation, "about");
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
    assert_eq!(
        second, 0,
        "idempotent re-run must not re-touch already-closed rows"
    );
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
    assert_eq!(
        closed, 0,
        "no related_to rows exist; causes edge must be untouched"
    );

    let out = get_edges(&conn, "fog-other-src", "outgoing", None).unwrap();
    assert_eq!(out.len(), 1);
    assert!(
        out[0].valid_to.is_none(),
        "non-related_to edge must stay open"
    );
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

/// #774 kill-test ①: two independent observations of the SAME
/// (source, target, relation) triple must accumulate TWO ledger rows while
/// `memory_edges` stays a single last-write-wins projection row holding the
/// most recent value. This is the whole point of the ledger — the graph
/// collapses re-observations, the ledger does not.
#[test]
fn edge_observations_accumulate_while_graph_row_collapses() {
    let mut conn = make_conn();
    let e1 = make_entry("obs-src", "source");
    let e2 = make_entry("obs-tgt", "target");
    upsert(&mut conn, &e1, false).unwrap();
    upsert(&mut conn, &e2, false).unwrap();

    let base = MemoryEdge {
        source_id: "obs-src".into(),
        target_id: "obs-tgt".into(),
        relation: "causes".into(),
        weight: 0.3,
        metadata: serde_json::json!({}),
        created_at: String::new(),
        valid_from: String::new(),
        valid_to: None,
    };

    // First observation.
    add_edge_with_provenance(
        &conn,
        &base,
        &EdgeProvenance {
            capture_event_kind: "recall".into(),
            capture_event_id: "evt-1".into(),
            actor: "projector".into(),
            reason_code: "co_occurrence".into(),
            evidence_hash: None,
        },
    )
    .unwrap();

    // Second, independent observation of the SAME triple, new capture event and
    // a new (last-write-wins) weight.
    let second = MemoryEdge {
        weight: 0.9,
        ..base.clone()
    };
    add_edge_with_provenance(
        &conn,
        &second,
        &EdgeProvenance {
            capture_event_kind: "recall".into(),
            capture_event_id: "evt-2".into(),
            actor: "projector".into(),
            reason_code: "co_occurrence".into(),
            evidence_hash: None,
        },
    )
    .unwrap();

    // Ledger: two rows, distinct observation ids, distinct capture events.
    let obs = list_observations_for_edge(&conn, "obs-src", "obs-tgt", "causes").unwrap();
    assert_eq!(obs.len(), 2, "each write must append one observation row");
    assert_ne!(
        obs[0].observation_id, obs[1].observation_id,
        "observation ids must be unique per write"
    );
    let mut event_ids: Vec<String> = obs.iter().map(|o| o.capture_event_id.clone()).collect();
    event_ids.sort();
    assert_eq!(event_ids, vec!["evt-1".to_string(), "evt-2".to_string()]);
    assert_eq!(
        count_active_observations(&conn, "obs-src", "obs-tgt", "causes").unwrap(),
        2
    );

    // Graph: exactly one row, holding the LAST-written value.
    let edges = get_edges(&conn, "obs-src", "outgoing", None).unwrap();
    assert_eq!(edges.len(), 1, "graph must collapse to one projection row");
    assert_eq!(
        edges[0].weight, 0.9,
        "graph row must hold the last-write-wins value"
    );
}

/// #774 kill-test ②: a rejected edge write (illegal relation) must leave ZERO
/// observations — the ledger never records an observation for a write that did
/// not persist. Validation fails before the savepoint opens, and the atomicity
/// guarantee covers any post-INSERT failure inside it.
#[test]
fn rejected_edge_write_appends_no_observation() {
    let mut conn = make_conn();
    let e1 = make_entry("rej-src", "source");
    let e2 = make_entry("rej-tgt", "target");
    upsert(&mut conn, &e1, false).unwrap();
    upsert(&mut conn, &e2, false).unwrap();

    let edge = MemoryEdge {
        source_id: "rej-src".into(),
        target_id: "rej-tgt".into(),
        relation: "shares_entities".into(), // illegal on the generic path
        weight: 0.5,
        metadata: serde_json::json!({}),
        created_at: String::new(),
        valid_from: String::new(),
        valid_to: None,
    };
    add_edge(&conn, &edge).unwrap_err();

    // No graph row and no ledger row.
    assert!(get_edges(&conn, "rej-src", "outgoing", None)
        .unwrap()
        .is_empty());
    let obs = list_observations_for_edge(&conn, "rej-src", "rej-tgt", "shares_entities").unwrap();
    assert!(
        obs.is_empty(),
        "a rejected write must not append an observation, got {obs:?}"
    );
    // Nothing anywhere in the ledger.
    let total: u32 = conn
        .query_row("SELECT COUNT(*) FROM edge_observations", [], |row| row.get(0))
        .unwrap();
    assert_eq!(total, 0, "ledger must be empty after a rejected write");
}

/// #774 kill-test ③: invalidation is a soft stamp — `count_active` drops by
/// one, but `list_observations_for_edge` still returns the row (history is
/// never deleted). A second invalidate of the same id is an idempotent no-op.
#[test]
fn invalidate_observation_drops_active_count_but_keeps_history() {
    let mut conn = make_conn();
    let e1 = make_entry("inv-src", "source");
    let e2 = make_entry("inv-tgt", "target");
    upsert(&mut conn, &e1, false).unwrap();
    upsert(&mut conn, &e2, false).unwrap();

    let edge = MemoryEdge {
        source_id: "inv-src".into(),
        target_id: "inv-tgt".into(),
        relation: "causes".into(),
        weight: 0.5,
        metadata: serde_json::json!({}),
        created_at: String::new(),
        valid_from: String::new(),
        valid_to: None,
    };
    add_edge(&conn, &edge).unwrap();
    add_edge(&conn, &edge).unwrap();

    assert_eq!(
        count_active_observations(&conn, "inv-src", "inv-tgt", "causes").unwrap(),
        2
    );
    let obs = list_observations_for_edge(&conn, "inv-src", "inv-tgt", "causes").unwrap();
    assert_eq!(obs.len(), 2);
    let victim = obs[0].observation_id.clone();

    // First invalidate transitions the row.
    let changed = invalidate_observation(&conn, &victim, "").unwrap();
    assert!(changed, "first invalidate must transition an active row");

    // Active count drops by one; history (list) still has both rows.
    assert_eq!(
        count_active_observations(&conn, "inv-src", "inv-tgt", "causes").unwrap(),
        1,
        "invalidated observation must not count as active"
    );
    let after = list_observations_for_edge(&conn, "inv-src", "inv-tgt", "causes").unwrap();
    assert_eq!(after.len(), 2, "history must be retained, not deleted");
    let invalidated = after
        .iter()
        .find(|o| o.observation_id == victim)
        .expect("invalidated row must still be listed");
    assert!(
        invalidated.invalidated_at.is_some(),
        "invalidated row must carry an invalidated_at stamp"
    );

    // Idempotent: re-invalidating the same id changes nothing.
    let changed_again = invalidate_observation(&conn, &victim, "").unwrap();
    assert!(!changed_again, "re-invalidate must be an idempotent no-op");
    assert_eq!(
        count_active_observations(&conn, "inv-src", "inv-tgt", "causes").unwrap(),
        1
    );
}

/// #774 kill-test ④: the typed component-governance write door lands an
/// observation too — the ledger covers BOTH edge-write paths, not just the
/// generic one. The observation's relation is the enum's `as_str()` (the same
/// authoritative value the graph row stores), not `edge.relation`.
#[test]
fn component_governance_edge_write_appends_observation() {
    let mut conn = make_conn();
    let src = make_entry("cg-obs-src", "component");
    let tgt = make_entry("cg-obs-tgt", "component");
    upsert(&mut conn, &src, false).unwrap();
    upsert(&mut conn, &tgt, false).unwrap();

    let edge = MemoryEdge {
        source_id: "cg-obs-src".into(),
        target_id: "cg-obs-tgt".into(),
        relation: "IGNORED".into(), // enum is authoritative, not this string
        weight: 1.0,
        metadata: serde_json::json!({}),
        created_at: String::new(),
        valid_from: String::new(),
        valid_to: None,
    };
    add_component_governance_edge(&conn, &edge, ComponentGovernanceRelation::Owns).unwrap();

    // Observation is recorded under the enum's relation ("owns"), and none
    // under the ignored "IGNORED" string.
    let owns = list_observations_for_edge(&conn, "cg-obs-src", "cg-obs-tgt", "owns").unwrap();
    assert_eq!(
        owns.len(),
        1,
        "typed governance write must append one observation under 'owns'"
    );
    assert_eq!(owns[0].relation, "owns");
    let ignored =
        list_observations_for_edge(&conn, "cg-obs-src", "cg-obs-tgt", "IGNORED").unwrap();
    assert!(
        ignored.is_empty(),
        "observation must key off the enum relation, not edge.relation"
    );
    assert_eq!(
        count_active_observations(&conn, "cg-obs-src", "cg-obs-tgt", "owns").unwrap(),
        1
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
