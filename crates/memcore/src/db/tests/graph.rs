use super::*;

use crate::db::ConfirmedContradictionOutcome;
use crate::types::ExpectedMemoryState;

fn confirmed_contradiction_edges(
    source_id: &str,
    target_id: &str,
    completion_status: &str,
) -> (MemoryEdge, MemoryEdge, String) {
    let at = "2026-07-30T01:02:03Z".to_string();
    let metadata = json!({
        "auto_contradiction": true,
        "llm_verified": true,
        "confidence": 0.88,
        "reason": "newer fact conflicts",
        "provenance": {
            "model_invocation": {
                "schema": "model-invocation-v1",
                "lane": "extract",
                "engine_kind": "provider_http",
                "effective_provider": "test-provider",
                "effective_model": "test-model",
                "effective_version": null,
                "fallback_chain": [],
                "degraded": false,
                "completion_status": completion_status,
                "prompt_tokens": 12,
                "completion_tokens": 4,
                "total_tokens": 16,
                "latency_ms": 9
            }
        }
    });
    let edge = |relation: &str| MemoryEdge {
        source_id: source_id.to_string(),
        target_id: target_id.to_string(),
        relation: relation.to_string(),
        weight: 0.88,
        metadata: metadata.clone(),
        created_at: at.clone(),
        valid_from: String::new(),
        valid_to: None,
    };
    (edge("contradicts"), edge("supersedes"), at)
}

/// Snapshot a candidate row the way the contradiction read path does, through
/// the same loader the write transaction re-reads with.
fn expected_candidate_state(conn: &Connection, id: &str) -> ExpectedMemoryState {
    let ids = vec![id.to_string()];
    let entry = fetch_by_ids(conn, &ids, true)
        .unwrap()
        .remove(id)
        .expect("candidate row exists");
    let superseded_by: Option<String> = conn
        .query_row(
            "SELECT superseded_by FROM memories WHERE id = ?1",
            [id],
            |row| row.get(0),
        )
        .unwrap();
    ExpectedMemoryState::from_entry(&entry, superseded_by.as_deref())
}

fn assert_no_contradiction_mutation(conn: &Connection, target_id: &str) {
    let edge_count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM memory_edges WHERE relation IN ('contradicts', 'supersedes')",
            [],
            |row| row.get(0),
        )
        .unwrap();
    let observation_count: i64 = conn
        .query_row("SELECT COUNT(*) FROM edge_observations", [], |row| {
            row.get(0)
        })
        .unwrap();
    let state: (Option<String>, Option<String>) = conn
        .query_row(
            "SELECT superseded_by, valid_until FROM memories WHERE id = ?1",
            [target_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!(edge_count, 0, "failed mutation must leave no graph rows");
    assert_eq!(
        observation_count, 0,
        "failed mutation must leave no edge observations"
    );
    assert_eq!(state, (None, None), "failed mutation must not supersede");
}

#[test]
fn confirmed_contradiction_transaction_commits_edges_observations_and_lifecycle() {
    let mut conn = make_conn();
    upsert(&mut conn, &make_entry("confirmed-new", "new fact"), false).unwrap();
    upsert(&mut conn, &make_entry("confirmed-old", "old fact"), false).unwrap();
    let (contradicts, supersedes, at) =
        confirmed_contradiction_edges("confirmed-new", "confirmed-old", "complete");
    let expected = expected_candidate_state(&conn, "confirmed-old");

    let tx = conn
        .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
        .unwrap();
    let outcome =
        persist_confirmed_contradiction_within_tx(&tx, &contradicts, &supersedes, &at, &expected)
            .unwrap();
    tx.commit().unwrap();
    assert_eq!(outcome, ConfirmedContradictionOutcome::Committed);

    let edge_count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM memory_edges WHERE source_id = 'confirmed-new' AND target_id = 'confirmed-old'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    let observation_count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM edge_observations WHERE source_id = 'confirmed-new' AND target_id = 'confirmed-old'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    let state: (Option<String>, Option<String>) = conn
        .query_row(
            "SELECT superseded_by, valid_until FROM memories WHERE id = 'confirmed-old'",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!(edge_count, 2);
    assert_eq!(observation_count, 2);
    assert_eq!(state.0.as_deref(), Some("confirmed-new"));
    assert_eq!(state.1.as_deref(), Some("2026-07-30T01:02:03.000Z"));
}

#[test]
fn confirmed_contradiction_transaction_rolls_back_at_every_side_effect_boundary() {
    let faults = [
        (
            "first_edge",
            "CREATE TEMP TRIGGER fail_confirmed_first BEFORE INSERT ON memory_edges \
             WHEN NEW.relation = 'contradicts' BEGIN SELECT RAISE(ABORT, 'first edge'); END;",
        ),
        (
            "second_edge",
            "CREATE TEMP TRIGGER fail_confirmed_second BEFORE INSERT ON memory_edges \
             WHEN NEW.relation = 'supersedes' BEGIN SELECT RAISE(ABORT, 'second edge'); END;",
        ),
        (
            "lifecycle",
            "CREATE TEMP TRIGGER fail_confirmed_lifecycle BEFORE UPDATE OF superseded_by ON memories \
             BEGIN SELECT RAISE(ABORT, 'lifecycle'); END;",
        ),
    ];

    for (fault_name, trigger) in faults {
        let mut conn = make_conn();
        upsert(&mut conn, &make_entry("rollback-new", "new fact"), false).unwrap();
        upsert(&mut conn, &make_entry("rollback-old", "old fact"), false).unwrap();
        conn.execute_batch(trigger).unwrap();
        let (contradicts, supersedes, at) =
            confirmed_contradiction_edges("rollback-new", "rollback-old", "complete");
        let expected = expected_candidate_state(&conn, "rollback-old");

        let tx = conn
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
            .unwrap();
        let error = persist_confirmed_contradiction_within_tx(
            &tx,
            &contradicts,
            &supersedes,
            &at,
            &expected,
        )
        .expect_err("injected side-effect failure must abort the mutation");
        assert!(
            error
                .to_string()
                .contains(fault_name.split('_').next().unwrap()),
            "unexpected {fault_name} error: {error}"
        );
        drop(tx);
        assert_no_contradiction_mutation(&conn, "rollback-old");
    }
}

#[test]
fn confirmed_contradiction_transaction_rejects_truncated_receipt_before_writes() {
    let mut conn = make_conn();
    upsert(&mut conn, &make_entry("truncated-new", "new fact"), false).unwrap();
    upsert(&mut conn, &make_entry("truncated-old", "old fact"), false).unwrap();
    let (contradicts, supersedes, at) =
        confirmed_contradiction_edges("truncated-new", "truncated-old", "truncated");
    let expected = expected_candidate_state(&conn, "truncated-old");

    let tx = conn
        .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
        .unwrap();
    persist_confirmed_contradiction_within_tx(&tx, &contradicts, &supersedes, &at, &expected)
        .expect_err("truncated verification receipt must be rejected");
    drop(tx);
    assert_no_contradiction_mutation(&conn, "truncated-old");
}

#[test]
fn confirmed_contradiction_transaction_rejects_non_allowlisted_receipt_fields() {
    let mut conn = make_conn();
    upsert(&mut conn, &make_entry("unsafe-new", "new fact"), false).unwrap();
    upsert(&mut conn, &make_entry("unsafe-old", "old fact"), false).unwrap();
    let (mut contradicts, mut supersedes, at) =
        confirmed_contradiction_edges("unsafe-new", "unsafe-old", "complete");
    for edge in [&mut contradicts, &mut supersedes] {
        edge.metadata
            .pointer_mut("/provenance/model_invocation")
            .and_then(serde_json::Value::as_object_mut)
            .expect("receipt object")
            .insert(
                "raw_provider_error".to_string(),
                json!("must never become durable"),
            );
    }

    let expected = expected_candidate_state(&conn, "unsafe-old");
    let tx = conn
        .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
        .unwrap();
    persist_confirmed_contradiction_within_tx(&tx, &contradicts, &supersedes, &at, &expected)
        .expect_err("receipt fields outside the persistence allowlist must fail closed");
    drop(tx);
    assert_no_contradiction_mutation(&conn, "unsafe-old");
}

#[test]
fn confirmed_contradiction_transaction_rolls_back_when_lifecycle_cas_loses() {
    let mut conn = make_conn();
    upsert(&mut conn, &make_entry("cas-new", "new fact"), false).unwrap();
    upsert(&mut conn, &make_entry("cas-old", "old fact"), false).unwrap();
    conn.execute(
        "UPDATE memories SET superseded_by = 'existing-winner' WHERE id = 'cas-old'",
        [],
    )
    .unwrap();
    let (contradicts, supersedes, at) =
        confirmed_contradiction_edges("cas-new", "cas-old", "complete");
    // Snapshot after the pre-existing supersession so the state gate passes and
    // this test still exercises the `superseded_by IS NULL` predicate it was
    // written for, rather than short-circuiting on a stale snapshot.
    let expected = expected_candidate_state(&conn, "cas-old");

    let tx = conn
        .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
        .unwrap();
    persist_confirmed_contradiction_within_tx(&tx, &contradicts, &supersedes, &at, &expected)
        .expect_err("lost lifecycle CAS must abort both edge writes");
    drop(tx);

    let edge_count: i64 = conn
        .query_row("SELECT COUNT(*) FROM memory_edges", [], |row| row.get(0))
        .unwrap();
    let observation_count: i64 = conn
        .query_row("SELECT COUNT(*) FROM edge_observations", [], |row| {
            row.get(0)
        })
        .unwrap();
    let winner: Option<String> = conn
        .query_row(
            "SELECT superseded_by FROM memories WHERE id = 'cas-old'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(edge_count, 0);
    assert_eq!(observation_count, 0);
    assert_eq!(winner.as_deref(), Some("existing-winner"));
}

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

    let expanded = graph_expand_limited(&conn, &["limit-root".into()], 1, None, 2).unwrap();
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

    let expanded = graph_expand_limited(&conn, &["a-root".into()], 2, None, 2).unwrap();
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
        .query_row("SELECT COUNT(*) FROM edge_observations", [], |row| {
            row.get(0)
        })
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
    let ignored = list_observations_for_edge(&conn, "cg-obs-src", "cg-obs-tgt", "IGNORED").unwrap();
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

/// Read the weight column exactly as it sits on disk — no `get_edges` decode,
/// no scorer clamp — so these tests can only pass if the *write* path stored a
/// governed value. Returns `None` when the column is NULL (which is what
/// SQLite stores for a bound `NaN`).
fn stored_weight(conn: &Connection, source: &str, target: &str, relation: &str) -> Option<f64> {
    conn.query_row(
        "SELECT weight FROM memory_edges WHERE source_id = ?1 AND target_id = ?2 AND relation = ?3",
        params![source, target, relation],
        |row| row.get::<_, Option<f64>>(0),
    )
    .unwrap()
}

fn clamp_edge(source: &str, target: &str, weight: f64) -> MemoryEdge {
    MemoryEdge {
        source_id: source.into(),
        target_id: target.into(),
        relation: "causes".into(),
        weight,
        metadata: serde_json::json!({}),
        created_at: String::new(),
        valid_from: String::new(),
        valid_to: None,
    }
}

/// #1460: `write_edge_row` clamps `weight` into `[0, 1]` at write time.
/// Writers such as the continuity timeline projection lift `weight` straight
/// out of an event payload, so an out-of-range value must be stored clamped —
/// not stored raw and only tamed later by the read-side clamp.
#[test]
fn add_edge_clamps_out_of_range_weight_at_write_time() {
    let mut conn = make_conn();
    for id in ["clamp-src", "clamp-hi", "clamp-lo", "clamp-ok"] {
        upsert(&mut conn, &make_entry(id, id), false).unwrap();
    }

    add_edge(&conn, &clamp_edge("clamp-src", "clamp-hi", 42.0)).unwrap();
    add_edge(&conn, &clamp_edge("clamp-src", "clamp-lo", -3.5)).unwrap();
    add_edge(&conn, &clamp_edge("clamp-src", "clamp-ok", 0.25)).unwrap();

    assert_eq!(
        stored_weight(&conn, "clamp-src", "clamp-hi", "causes"),
        Some(1.0),
        "weight above the band must be stored clamped to 1.0, not raw"
    );
    assert_eq!(
        stored_weight(&conn, "clamp-src", "clamp-lo", "causes"),
        Some(0.0),
        "negative weight must be stored clamped to 0.0, not raw"
    );
    assert_eq!(
        stored_weight(&conn, "clamp-src", "clamp-ok", "causes"),
        Some(0.25),
        "in-band weight must be stored untouched"
    );
}

/// The `ON CONFLICT ... DO UPDATE SET weight = ?4` arm binds the same
/// parameter as the INSERT, so re-observing an existing edge must not be able
/// to smuggle an out-of-range weight past the gate.
#[test]
fn add_edge_clamps_out_of_range_weight_on_upsert() {
    let mut conn = make_conn();
    for id in ["clamp-up-src", "clamp-up-tgt"] {
        upsert(&mut conn, &make_entry(id, id), false).unwrap();
    }

    add_edge(&conn, &clamp_edge("clamp-up-src", "clamp-up-tgt", 0.5)).unwrap();
    assert_eq!(
        stored_weight(&conn, "clamp-up-src", "clamp-up-tgt", "causes"),
        Some(0.5)
    );

    add_edge(&conn, &clamp_edge("clamp-up-src", "clamp-up-tgt", 9.5)).unwrap();
    assert_eq!(
        stored_weight(&conn, "clamp-up-src", "clamp-up-tgt", "causes"),
        Some(1.0),
        "the DO UPDATE arm must clamp too, not just the INSERT"
    );
}

/// Every non-finite weight — `NaN`, `+inf`, `-inf` alike — is treated as
/// **malformed input and collapses to `0.0`**, not as a very large weight that
/// saturates at the upper bound. Two reasons this is the frozen direction:
///
/// - it is what the shipped seed-weight path already does
///   (`scorer/graph.rs:99`: `if weight.is_finite() { *weight } else { 0.0 }
///   .clamp(0.0, 1.0)`), and one subsystem must not hold two rules for the
///   same malformed value;
/// - #1460 exists because untrusted writers influence this field, so the
///   malformed case must fail **closed** (zero influence) rather than open
///   (maximum edge weight).
///
/// Mechanically the guard must also come first: `f64::clamp` propagates `NaN`
/// rather than pinning it to a bound, and SQLite has no NaN — a bound `NaN`
/// lands as NULL, which then breaks the `get_edges` f64 decode.
#[test]
fn add_edge_collapses_non_finite_weight_to_zero() {
    let mut conn = make_conn();
    for id in [
        "clamp-nan-src",
        "clamp-nan-tgt",
        "clamp-posinf-tgt",
        "clamp-neginf-tgt",
    ] {
        upsert(&mut conn, &make_entry(id, id), false).unwrap();
    }

    for (target, weight) in [
        ("clamp-nan-tgt", f64::NAN),
        ("clamp-posinf-tgt", f64::INFINITY),
        ("clamp-neginf-tgt", f64::NEG_INFINITY),
    ] {
        add_edge(&conn, &clamp_edge("clamp-nan-src", target, weight)).unwrap();
        assert_eq!(
            stored_weight(&conn, "clamp-nan-src", target, "causes"),
            Some(0.0),
            "non-finite weight ({weight}) is malformed input: it must fail closed to 0.0 \
             (matching scorer/graph.rs:99), never saturate to the upper bound — and never \
             land as NULL, which is what SQLite stores for a bound NaN"
        );
    }

    // And the rows stay decodable through the normal read door.
    let edges = get_edges(&conn, "clamp-nan-src", "outgoing", None).unwrap();
    assert_eq!(edges.len(), 3);
}

/// The typed governance door shares `write_edge_row`, so it inherits the same
/// clamp — the gate lives at the choke point, not on one caller.
#[test]
fn component_governance_edge_clamps_weight_too() {
    let mut conn = make_conn();
    for id in ["clamp-cg-src", "clamp-cg-tgt"] {
        upsert(&mut conn, &make_entry(id, id), false).unwrap();
    }

    let edge = MemoryEdge {
        relation: "IGNORED".into(),
        ..clamp_edge("clamp-cg-src", "clamp-cg-tgt", 7.0)
    };
    add_component_governance_edge(&conn, &edge, ComponentGovernanceRelation::Owns).unwrap();

    assert_eq!(
        stored_weight(&conn, "clamp-cg-src", "clamp-cg-tgt", "owns"),
        Some(1.0),
        "the grandfathered typed door must clamp as well"
    );
}

/// The killer case for the inverse of tachi#1551: enrichment rewrites the
/// candidate's `summary` **without** bumping `revision`, so neither a revision
/// guard nor the `superseded_by IS NULL` predicate notices that the verdict is
/// now about content the database no longer holds.
#[test]
fn confirmed_contradiction_skips_a_verdict_about_content_enrichment_rewrote() {
    let mut conn = make_conn();
    upsert(&mut conn, &make_entry("stale-new", "new fact"), false).unwrap();
    upsert(&mut conn, &make_entry("stale-old", "old fact"), false).unwrap();
    let expected = expected_candidate_state(&conn, "stale-old");
    let revision_before: i64 = conn
        .query_row(
            "SELECT revision FROM memories WHERE id = 'stale-old'",
            [],
            |row| row.get(0),
        )
        .unwrap();

    assert!(update_enrichment_fields(
        &mut conn,
        "stale-old",
        Some("rewritten while the model was being consulted"),
        None,
        None,
        None,
        revision_before,
        None,
        None,
        None,
    )
    .unwrap());
    let revision_after: i64 = conn
        .query_row(
            "SELECT revision FROM memories WHERE id = 'stale-old'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(
        revision_after, revision_before,
        "enrichment must not bump revision — that is exactly why revision cannot carry this guard"
    );

    let (contradicts, supersedes, at) =
        confirmed_contradiction_edges("stale-new", "stale-old", "complete");
    let tx = conn
        .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
        .unwrap();
    let outcome =
        persist_confirmed_contradiction_within_tx(&tx, &contradicts, &supersedes, &at, &expected)
            .unwrap();
    tx.commit().unwrap();

    assert_eq!(outcome, ConfirmedContradictionOutcome::StaleSkipped);
    assert_no_contradiction_mutation(&conn, "stale-old");
}

/// Revision-bumping drift is caught by the same gate, and is likewise reported
/// as a skip rather than an error.
#[test]
fn confirmed_contradiction_skips_a_verdict_after_a_revision_bumping_rewrite() {
    let mut conn = make_conn();
    upsert(&mut conn, &make_entry("rev-new", "new fact"), false).unwrap();
    upsert(&mut conn, &make_entry("rev-old", "old fact"), false).unwrap();
    let expected = expected_candidate_state(&conn, "rev-old");
    let revision_before: i64 = conn
        .query_row(
            "SELECT revision FROM memories WHERE id = 'rev-old'",
            [],
            |row| row.get(0),
        )
        .unwrap();

    assert!(update_with_revision(
        &mut conn,
        "rev-old",
        "old fact, rewritten",
        "old fact, rewritten",
        "test",
        "{}",
        None,
        revision_before,
    )
    .unwrap());

    let (contradicts, supersedes, at) =
        confirmed_contradiction_edges("rev-new", "rev-old", "complete");
    let tx = conn
        .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
        .unwrap();
    let outcome =
        persist_confirmed_contradiction_within_tx(&tx, &contradicts, &supersedes, &at, &expected)
            .unwrap();
    tx.commit().unwrap();

    assert_eq!(outcome, ConfirmedContradictionOutcome::StaleSkipped);
    assert_no_contradiction_mutation(&conn, "rev-old");
}

/// A verdict about a row that has since been deleted must also fail closed:
/// the snapshot cannot match a row that is not there.
#[test]
fn confirmed_contradiction_skips_a_verdict_about_a_deleted_candidate() {
    let mut conn = make_conn();
    upsert(&mut conn, &make_entry("gone-new", "new fact"), false).unwrap();
    upsert(&mut conn, &make_entry("gone-old", "old fact"), false).unwrap();
    let expected = expected_candidate_state(&conn, "gone-old");
    assert!(delete(&mut conn, "gone-old", false).unwrap());

    let (contradicts, supersedes, at) =
        confirmed_contradiction_edges("gone-new", "gone-old", "complete");
    let tx = conn
        .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
        .unwrap();
    let outcome =
        persist_confirmed_contradiction_within_tx(&tx, &contradicts, &supersedes, &at, &expected)
            .unwrap();
    tx.commit().unwrap();

    assert_eq!(outcome, ConfirmedContradictionOutcome::StaleSkipped);
    let edge_count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM memory_edges WHERE relation IN ('contradicts', 'supersedes')",
            [],
            |row| row.get(0),
        )
        .unwrap();
    let observation_count: i64 = conn
        .query_row("SELECT COUNT(*) FROM edge_observations", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert_eq!(edge_count, 0);
    assert_eq!(observation_count, 0);
}
