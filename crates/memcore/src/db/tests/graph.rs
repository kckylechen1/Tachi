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

/// Snapshot a row (entry or candidate) the way the contradiction read path
/// does, through the same loader the write transaction re-reads with.
fn expected_state(conn: &Connection, id: &str) -> ExpectedMemoryState {
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
    let expected_entry = expected_state(&conn, "confirmed-new");
    let expected = expected_state(&conn, "confirmed-old");
    let revision_before: i64 = conn
        .query_row(
            "SELECT revision FROM memories WHERE id = 'confirmed-old'",
            [],
            |row| row.get(0),
        )
        .unwrap();

    let tx = conn
        .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
        .unwrap();
    let outcome = persist_confirmed_contradiction_within_tx(
        &tx,
        &contradicts,
        &supersedes,
        &at,
        &expected_entry,
        &expected,
    )
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
    let state: (Option<String>, Option<String>, i64) = conn
        .query_row(
            "SELECT superseded_by, valid_until, revision FROM memories WHERE id = 'confirmed-old'",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .unwrap();
    assert_eq!(edge_count, 2);
    assert_eq!(observation_count, 2);
    assert_eq!(state.0.as_deref(), Some("confirmed-new"));
    assert_eq!(state.1.as_deref(), Some("2026-07-30T01:02:03.000Z"));
    assert_eq!(
        state.2,
        revision_before + 1,
        "supersession must advance revision so revision-scoped CAS callers (e.g. \
         update_enrichment_fields) notice the lifecycle transition"
    );
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
        let expected_entry = expected_state(&conn, "rollback-new");
        let expected = expected_state(&conn, "rollback-old");

        let tx = conn
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
            .unwrap();
        let error = persist_confirmed_contradiction_within_tx(
            &tx,
            &contradicts,
            &supersedes,
            &at,
            &expected_entry,
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
    let expected_entry = expected_state(&conn, "truncated-new");
    let expected = expected_state(&conn, "truncated-old");

    let tx = conn
        .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
        .unwrap();
    persist_confirmed_contradiction_within_tx(
        &tx,
        &contradicts,
        &supersedes,
        &at,
        &expected_entry,
        &expected,
    )
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

    let expected_entry = expected_state(&conn, "unsafe-new");
    let expected = expected_state(&conn, "unsafe-old");
    let tx = conn
        .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
        .unwrap();
    persist_confirmed_contradiction_within_tx(
        &tx,
        &contradicts,
        &supersedes,
        &at,
        &expected_entry,
        &expected,
    )
    .expect_err("receipt fields outside the persistence allowlist must fail closed");
    drop(tx);
    assert_no_contradiction_mutation(&conn, "unsafe-old");
}

/// #1558: a receipt carrying the new `content_hash`/`memory_id`/`revision`
/// binding fields is not "unknown field" garbage to the shadow validator --
/// the contract is unified, not merely tolerated as noise.
#[test]
fn confirmed_contradiction_transaction_accepts_receipt_with_content_binding() {
    let mut conn = make_conn();
    upsert(&mut conn, &make_entry("bound-new", "new fact"), false).unwrap();
    upsert(&mut conn, &make_entry("bound-old", "old fact"), false).unwrap();
    let (mut contradicts, mut supersedes, at) =
        confirmed_contradiction_edges("bound-new", "bound-old", "complete");
    for edge in [&mut contradicts, &mut supersedes] {
        edge.metadata
            .pointer_mut("/provenance/model_invocation")
            .and_then(serde_json::Value::as_object_mut)
            .expect("receipt object")
            .insert(
                "content_hash".to_string(),
                json!("deadbeefcafefeed0011223344556677"),
            );
    }

    let expected_entry = expected_state(&conn, "bound-new");
    let expected = expected_state(&conn, "bound-old");
    let tx = conn
        .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
        .unwrap();
    let outcome = persist_confirmed_contradiction_within_tx(
        &tx,
        &contradicts,
        &supersedes,
        &at,
        &expected_entry,
        &expected,
    )
    .expect("a content-hash-bound receipt is still an allowlisted model-invocation-v1 receipt");
    tx.commit().unwrap();
    assert_eq!(outcome, ConfirmedContradictionOutcome::Committed);
}

#[test]
fn confirmed_contradiction_transaction_rejects_blank_content_binding() {
    let mut conn = make_conn();
    upsert(&mut conn, &make_entry("blank-bound-new", "new fact"), false).unwrap();
    upsert(&mut conn, &make_entry("blank-bound-old", "old fact"), false).unwrap();
    let (mut contradicts, mut supersedes, at) =
        confirmed_contradiction_edges("blank-bound-new", "blank-bound-old", "complete");
    for edge in [&mut contradicts, &mut supersedes] {
        edge.metadata
            .pointer_mut("/provenance/model_invocation")
            .and_then(serde_json::Value::as_object_mut)
            .expect("receipt object")
            .insert("content_hash".to_string(), json!("   "));
    }

    let expected_entry = expected_state(&conn, "blank-bound-new");
    let expected = expected_state(&conn, "blank-bound-old");
    let tx = conn
        .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
        .unwrap();
    persist_confirmed_contradiction_within_tx(
        &tx,
        &contradicts,
        &supersedes,
        &at,
        &expected_entry,
        &expected,
    )
    .expect_err("a present-but-blank content_hash must fail closed, not silently pass through");
    drop(tx);
    assert_no_contradiction_mutation(&conn, "blank-bound-old");
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
    let expected_entry = expected_state(&conn, "cas-new");
    let expected = expected_state(&conn, "cas-old");

    let tx = conn
        .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
        .unwrap();
    persist_confirmed_contradiction_within_tx(
        &tx,
        &contradicts,
        &supersedes,
        &at,
        &expected_entry,
        &expected,
    )
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
            authority: None,
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
            authority: None,
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

    delete(&mut conn, "del-e1", false, StoreProfile::TachiFull).unwrap();
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
    let expected_entry = expected_state(&conn, "stale-new");
    let expected = expected_state(&conn, "stale-old");
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
    let outcome = persist_confirmed_contradiction_within_tx(
        &tx,
        &contradicts,
        &supersedes,
        &at,
        &expected_entry,
        &expected,
    )
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
    let expected_entry = expected_state(&conn, "rev-new");
    let expected = expected_state(&conn, "rev-old");
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
    let outcome = persist_confirmed_contradiction_within_tx(
        &tx,
        &contradicts,
        &supersedes,
        &at,
        &expected_entry,
        &expected,
    )
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
    let expected_entry = expected_state(&conn, "gone-new");
    let expected = expected_state(&conn, "gone-old");
    assert!(delete(&mut conn, "gone-old", false, StoreProfile::TachiFull).unwrap());

    let (contradicts, supersedes, at) =
        confirmed_contradiction_edges("gone-new", "gone-old", "complete");
    let tx = conn
        .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
        .unwrap();
    let outcome = persist_confirmed_contradiction_within_tx(
        &tx,
        &contradicts,
        &supersedes,
        &at,
        &expected_entry,
        &expected,
    )
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

/// tachi#1563 item 1: the entry (the newer fact that triggered detection) can
/// be rewritten by enrichment during the LLM round-trip exactly like a
/// candidate can — mirrors
/// `confirmed_contradiction_skips_a_verdict_about_content_enrichment_rewrote`
/// above, but on the entry side of the pair. Before this fix the write path
/// never re-read the entry, so this same rewrite would have gone unnoticed
/// and the commit would have proceeded.
#[test]
fn confirmed_contradiction_skips_a_verdict_about_content_enrichment_rewrote_on_the_entry_side() {
    let mut conn = make_conn();
    upsert(&mut conn, &make_entry("entry-stale-new", "new fact"), false).unwrap();
    upsert(&mut conn, &make_entry("entry-stale-old", "old fact"), false).unwrap();
    let expected_entry = expected_state(&conn, "entry-stale-new");
    let expected = expected_state(&conn, "entry-stale-old");
    let revision_before: i64 = conn
        .query_row(
            "SELECT revision FROM memories WHERE id = 'entry-stale-new'",
            [],
            |row| row.get(0),
        )
        .unwrap();

    assert!(update_enrichment_fields(
        &mut conn,
        "entry-stale-new",
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

    let (contradicts, supersedes, at) =
        confirmed_contradiction_edges("entry-stale-new", "entry-stale-old", "complete");
    let tx = conn
        .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
        .unwrap();
    let outcome = persist_confirmed_contradiction_within_tx(
        &tx,
        &contradicts,
        &supersedes,
        &at,
        &expected_entry,
        &expected,
    )
    .unwrap();
    tx.commit().unwrap();

    assert_eq!(outcome, ConfirmedContradictionOutcome::StaleSkipped);
    assert_no_contradiction_mutation(&conn, "entry-stale-old");
}

/// The entry side of the same guard must also fail closed when the entry was
/// archived (not merely rewritten) between the read that fed the model and
/// the write — `archived` is one of the compared fields in
/// `ExpectedMemoryState::matches`, so no separate lifecycle check is needed
/// (tachi#1563).
#[test]
fn confirmed_contradiction_skips_a_verdict_when_the_entry_was_archived() {
    let mut conn = make_conn();
    upsert(
        &mut conn,
        &make_entry("entry-archived-new", "new fact"),
        false,
    )
    .unwrap();
    upsert(
        &mut conn,
        &make_entry("entry-archived-old", "old fact"),
        false,
    )
    .unwrap();
    let expected_entry = expected_state(&conn, "entry-archived-new");
    let expected = expected_state(&conn, "entry-archived-old");

    assert!(
        archive_memory(&conn, "entry-archived-new").unwrap(),
        "fixture must actually archive the entry"
    );

    let (contradicts, supersedes, at) =
        confirmed_contradiction_edges("entry-archived-new", "entry-archived-old", "complete");
    let tx = conn
        .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
        .unwrap();
    let outcome = persist_confirmed_contradiction_within_tx(
        &tx,
        &contradicts,
        &supersedes,
        &at,
        &expected_entry,
        &expected,
    )
    .unwrap();
    tx.commit().unwrap();

    assert_eq!(outcome, ConfirmedContradictionOutcome::StaleSkipped);
    assert_no_contradiction_mutation(&conn, "entry-archived-old");
}

/// A verdict about an entry that has since been deleted must also fail
/// closed, mirroring `confirmed_contradiction_skips_a_verdict_about_a_deleted_candidate`
/// but for the entry side (tachi#1563).
#[test]
fn confirmed_contradiction_skips_a_verdict_when_the_entry_was_deleted() {
    let mut conn = make_conn();
    upsert(&mut conn, &make_entry("entry-gone-new", "new fact"), false).unwrap();
    upsert(&mut conn, &make_entry("entry-gone-old", "old fact"), false).unwrap();
    let expected_entry = expected_state(&conn, "entry-gone-new");
    let expected = expected_state(&conn, "entry-gone-old");
    assert!(delete(&mut conn, "entry-gone-new", false, StoreProfile::TachiFull).unwrap());

    let (contradicts, supersedes, at) =
        confirmed_contradiction_edges("entry-gone-new", "entry-gone-old", "complete");
    let tx = conn
        .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
        .unwrap();
    let outcome = persist_confirmed_contradiction_within_tx(
        &tx,
        &contradicts,
        &supersedes,
        &at,
        &expected_entry,
        &expected,
    )
    .unwrap();
    tx.commit().unwrap();

    assert_eq!(outcome, ConfirmedContradictionOutcome::StaleSkipped);
    assert_no_contradiction_mutation(&conn, "entry-gone-old");
}

/// tachi#1563 item 2: the lifecycle `UPDATE` that supersedes the candidate
/// must advance `revision`, or a revision-scoped CAS (chief among them
/// `update_enrichment_fields`) cannot notice the transition and can still
/// write into a row that has already been superseded.
#[test]
fn confirmed_contradiction_supersede_advances_revision_and_blocks_a_stale_enrichment_cas() {
    let mut conn = make_conn();
    upsert(&mut conn, &make_entry("revcas-new", "new fact"), false).unwrap();
    upsert(&mut conn, &make_entry("revcas-old", "old fact"), false).unwrap();
    let expected_entry = expected_state(&conn, "revcas-new");
    let expected = expected_state(&conn, "revcas-old");
    let revision_before: i64 = conn
        .query_row(
            "SELECT revision FROM memories WHERE id = 'revcas-old'",
            [],
            |row| row.get(0),
        )
        .unwrap();

    let (contradicts, supersedes, at) =
        confirmed_contradiction_edges("revcas-new", "revcas-old", "complete");
    let tx = conn
        .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
        .unwrap();
    let outcome = persist_confirmed_contradiction_within_tx(
        &tx,
        &contradicts,
        &supersedes,
        &at,
        &expected_entry,
        &expected,
    )
    .unwrap();
    tx.commit().unwrap();
    assert_eq!(outcome, ConfirmedContradictionOutcome::Committed);

    let revision_after: i64 = conn
        .query_row(
            "SELECT revision FROM memories WHERE id = 'revcas-old'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(
        revision_after,
        revision_before + 1,
        "supersede must advance revision so revision-scoped observers see the lifecycle transition"
    );

    // An enrichment write that read the row before supersession (and so is
    // still carrying the pre-supersession revision) must lose its CAS rather
    // than land fields on a row that has already been superseded — this is
    // the product-visible reason the revision bump exists: without it, an
    // in-flight enrichment write would silently resurrect content on a
    // memory the graph has already retired.
    let cas_result = update_enrichment_fields(
        &mut conn,
        "revcas-old",
        Some("late enrichment racing the supersede"),
        None,
        None,
        None,
        revision_before,
        None,
        None,
        None,
    )
    .unwrap();
    assert!(
        !cas_result,
        "enrichment write against the pre-supersession revision must be refused"
    );

    // The same drift is visible through the read-path snapshot: a candidate
    // matcher built from the pre-supersession row must now disagree with the
    // stored row, both because `revision` moved and because `superseded_by`
    // is no longer NULL.
    let ids = vec!["revcas-old".to_string()];
    let current = fetch_by_ids(&conn, &ids, true)
        .unwrap()
        .remove("revcas-old")
        .expect("row still exists");
    let superseded_by: Option<String> = conn
        .query_row(
            "SELECT superseded_by FROM memories WHERE id = 'revcas-old'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert!(
        !expected.matches(&current, superseded_by.as_deref()),
        "the pre-supersession snapshot must no longer match the post-supersede row"
    );
}

/// Review follow-up on tachi#1563: `ExpectedMemoryState::matches` no longer
/// compares `recall_count` / `query_diversity` (see the docstring on
/// [`ExpectedMemoryState`]). Bump both, through the real search-recording
/// path, on the entry *and* the candidate between the frozen snapshot and the
/// write — the exact race unrelated search traffic hitting the hottest row
/// during the LLM round-trip produces — and confirm the verdict still
/// commits instead of spuriously landing in `StaleSkipped`.
#[test]
fn confirmed_contradiction_commits_despite_recall_count_drift_on_both_sides() {
    let mut conn = make_conn();
    upsert(&mut conn, &make_entry("hot-new", "new fact"), false).unwrap();
    upsert(&mut conn, &make_entry("hot-old", "old fact"), false).unwrap();
    let expected_entry = expected_state(&conn, "hot-new");
    let expected = expected_state(&conn, "hot-old");

    // Unrelated search traffic lands on both rows while this verdict's model
    // round-trip is still in flight.
    record_access(
        &conn,
        &["hot-new".to_string(), "hot-old".to_string()],
        &["hot-new".to_string(), "hot-old".to_string()],
        Some("unrelated query"),
    )
    .unwrap();
    let (recall_new, recall_old, diversity_new, diversity_old): (i64, i64, i64, i64) = conn
        .query_row(
            "SELECT
                (SELECT recall_count FROM memories WHERE id = 'hot-new'),
                (SELECT recall_count FROM memories WHERE id = 'hot-old'),
                (SELECT query_diversity FROM memories WHERE id = 'hot-new'),
                (SELECT query_diversity FROM memories WHERE id = 'hot-old')",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )
        .unwrap();
    assert!(
        recall_new > 0 && recall_old > 0 && diversity_new > 0 && diversity_old > 0,
        "setup must actually bump recall_count and query_diversity on both rows: \
         recall_new={recall_new} recall_old={recall_old} diversity_new={diversity_new} \
         diversity_old={diversity_old}"
    );

    let (contradicts, supersedes, at) =
        confirmed_contradiction_edges("hot-new", "hot-old", "complete");
    let tx = conn
        .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
        .unwrap();
    let outcome = persist_confirmed_contradiction_within_tx(
        &tx,
        &contradicts,
        &supersedes,
        &at,
        &expected_entry,
        &expected,
    )
    .unwrap();
    tx.commit().unwrap();

    assert_eq!(
        outcome,
        ConfirmedContradictionOutcome::Committed,
        "recall_count/query_diversity drift alone must not sink an otherwise-valid verdict"
    );
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

// ─── tachi#1646: EdgeAuthority stamping + backward-compat read ─────────────

/// A caller that does not classify itself (the plain `add_edge` door,
/// `EdgeProvenance::default()`) must leave `metadata.authority` unset — this
/// is what makes stamping additive rather than a blanket rewrite of every
/// existing writer's persisted metadata. The name used to say
/// "leaves...unstamped", which was only true when the caller's own metadata
/// happened not to carry the reserved key; it now also covers the case where
/// it does, so the plain door **scrubs** rather than merely "doesn't add".
#[test]
fn add_edge_without_authority_scrubs_reserved_key() {
    let mut conn = make_conn();
    for id in ["auth-none-src", "auth-none-tgt"] {
        upsert(&mut conn, &make_entry(id, id), false).unwrap();
    }
    add_edge(
        &conn,
        &MemoryEdge {
            source_id: "auth-none-src".into(),
            target_id: "auth-none-tgt".into(),
            relation: "causes".into(),
            weight: 0.5,
            metadata: serde_json::json!({"existing": "field"}),
            created_at: String::new(),
            valid_from: String::new(),
            valid_to: None,
        },
    )
    .unwrap();

    let edges = get_edges(&conn, "auth-none-src", "outgoing", Some("causes")).unwrap();
    assert_eq!(edges.len(), 1);
    assert!(
        edges[0].metadata.get("authority").is_none(),
        "unclassified caller must not gain an authority key: {:?}",
        edges[0].metadata
    );
    assert_eq!(edges[0].metadata["existing"], serde_json::json!("field"));
    assert_eq!(
        edge_authority(&edges[0]),
        None,
        "no authority claim was made for this edge"
    );
}

/// Authority spoofing via the unclassified door (tachi#1646):
/// a caller that never goes through `add_edge_with_provenance`
/// (so `EdgeProvenance::authority` is `None`) but hands `add_edge` a
/// pre-baked `metadata.authority` string — e.g. a trusted class like
/// `"model_receipt_backed"` copy-pasted from an existing row, or crafted by
/// an untrusted N-API `edge_json` blob — must not have that string persist.
/// `stamp_authority` scrubs the reserved key on the `None` path precisely so
/// `edge_authority` cannot read back a classification no writer ever
/// actually made.
#[test]
fn add_edge_with_prebaked_authority_metadata_reads_back_none() {
    let mut conn = make_conn();
    for id in ["auth-spoof-src", "auth-spoof-tgt"] {
        upsert(&mut conn, &make_entry(id, id), false).unwrap();
    }
    add_edge(
        &conn,
        &MemoryEdge {
            source_id: "auth-spoof-src".into(),
            target_id: "auth-spoof-tgt".into(),
            relation: "causes".into(),
            weight: 0.5,
            metadata: serde_json::json!({
                "authority": EdgeAuthority::ModelReceiptBacked.as_str(),
                "existing": "field",
            }),
            created_at: String::new(),
            valid_from: String::new(),
            valid_to: None,
        },
    )
    .unwrap();

    let edges = get_edges(&conn, "auth-spoof-src", "outgoing", Some("causes")).unwrap();
    assert_eq!(edges.len(), 1);
    assert!(
        edges[0].metadata.get("authority").is_none(),
        "a plain add_edge must not persist a caller-supplied authority key: {:?}",
        edges[0].metadata
    );
    assert_eq!(edges[0].metadata["existing"], serde_json::json!("field"));
    assert_eq!(
        edge_authority(&edges[0]),
        None,
        "no writer classified this edge, so no authority claim may read back — spoofed metadata must not be trusted"
    );
}

/// `add_edge_with_provenance` with an explicit authority stamps
/// `metadata.authority`, and `edge_authority` reads the same variant back —
/// spot-checked for each of the four classes.
#[test]
fn add_edge_with_provenance_stamps_and_reads_back_every_authority_class() {
    let mut conn = make_conn();
    let classes = [
        (EdgeAuthority::ModelReceiptBacked, "auth-mrb"),
        (EdgeAuthority::DerivedHeuristic, "auth-dh"),
        (EdgeAuthority::CallerAsserted, "auth-ca"),
        (EdgeAuthority::StructuralBookkeeping, "auth-sb"),
    ];
    for (_, tgt) in &classes {
        upsert(&mut conn, &make_entry(tgt, tgt), false).unwrap();
    }
    upsert(&mut conn, &make_entry("auth-src", "auth-src"), false).unwrap();

    for (authority, tgt) in classes {
        add_edge_with_provenance(
            &conn,
            &MemoryEdge {
                source_id: "auth-src".into(),
                target_id: tgt.into(),
                relation: "causes".into(),
                weight: 0.5,
                metadata: serde_json::json!({}),
                created_at: String::new(),
                valid_from: String::new(),
                valid_to: None,
            },
            &EdgeProvenance {
                authority: Some(authority),
                ..EdgeProvenance::default()
            },
        )
        .unwrap();

        let edge = get_edges(&conn, "auth-src", "outgoing", Some("causes"))
            .unwrap()
            .into_iter()
            .find(|edge| edge.target_id == tgt)
            .expect("stamped edge exists");
        assert_eq!(
            edge.metadata["authority"],
            serde_json::json!(authority.as_str())
        );
        assert_eq!(edge_authority(&edge), Some(authority));
    }
}

/// Backward compat (tachi#1646 step 4): a pre-#1646 row (no
/// `metadata.authority` key at all) and a row carrying an unrecognized
/// `authority` string both read back `None` from `edge_authority` — "no
/// authority claim was made for this edge", never fabricated as a real
/// class.
#[test]
fn edge_authority_reads_legacy_and_malformed_metadata_as_none() {
    let mut conn = make_conn();
    for id in ["auth-legacy-src", "auth-legacy-tgt", "auth-junk-tgt"] {
        upsert(&mut conn, &make_entry(id, id), false).unwrap();
    }
    // Pre-#1646 row shape: no authority key at all.
    add_edge(
        &conn,
        &MemoryEdge {
            source_id: "auth-legacy-src".into(),
            target_id: "auth-legacy-tgt".into(),
            relation: "causes".into(),
            weight: 0.5,
            metadata: serde_json::json!({}),
            created_at: String::new(),
            valid_from: String::new(),
            valid_to: None,
        },
    )
    .unwrap();
    // A row that carries a string this build's enum does not recognize (a
    // hand-edited row, or a future build's variant).
    add_edge(
        &conn,
        &MemoryEdge {
            source_id: "auth-legacy-src".into(),
            target_id: "auth-junk-tgt".into(),
            relation: "causes".into(),
            weight: 0.5,
            metadata: serde_json::json!({"authority": "not_a_real_class"}),
            created_at: String::new(),
            valid_from: String::new(),
            valid_to: None,
        },
    )
    .unwrap();

    let edges = get_edges(&conn, "auth-legacy-src", "outgoing", Some("causes")).unwrap();
    assert_eq!(edges.len(), 2);
    for edge in &edges {
        assert_eq!(
            edge_authority(edge),
            None,
            "legacy/malformed authority must read back None, not a guessed class: {edge:?}"
        );
    }
}

/// `stamp_authority` **overwrites** the reserved key when the writer passes
/// `Some(authority)`, it does not merge with whatever the caller's own
/// metadata already had there — a writer that explicitly classifies an edge
/// is authoritative over that field even if the caller-supplied payload
/// disagrees.
#[test]
fn add_edge_with_provenance_overwrites_caller_supplied_authority() {
    let mut conn = make_conn();
    for id in ["auth-overwrite-src", "auth-overwrite-tgt"] {
        upsert(&mut conn, &make_entry(id, id), false).unwrap();
    }
    add_edge_with_provenance(
        &conn,
        &MemoryEdge {
            source_id: "auth-overwrite-src".into(),
            target_id: "auth-overwrite-tgt".into(),
            relation: "causes".into(),
            weight: 0.5,
            metadata: serde_json::json!({"authority": "model_receipt_backed"}),
            created_at: String::new(),
            valid_from: String::new(),
            valid_to: None,
        },
        &EdgeProvenance {
            authority: Some(EdgeAuthority::DerivedHeuristic),
            ..EdgeProvenance::default()
        },
    )
    .unwrap();

    let edge = get_edges(&conn, "auth-overwrite-src", "outgoing", Some("causes"))
        .unwrap()
        .into_iter()
        .next()
        .expect("stamped edge exists");
    assert_eq!(
        edge.metadata["authority"],
        serde_json::json!("derived_heuristic"),
        "the writer's classification must win, not the caller's payload"
    );
    assert_eq!(edge_authority(&edge), Some(EdgeAuthority::DerivedHeuristic));
}

/// `add_component_governance_edge_with_provenance` shares the same stamping
/// as the generic door — spot-checked for `StructuralBookkeeping`, the
/// census class for component-registry seeding.
#[test]
fn component_governance_edge_with_provenance_stamps_authority() {
    let mut conn = make_conn();
    for id in ["auth-cg-src", "auth-cg-tgt"] {
        upsert(&mut conn, &make_entry(id, id), false).unwrap();
    }
    let edge = MemoryEdge {
        relation: "IGNORED".into(),
        ..clamp_edge("auth-cg-src", "auth-cg-tgt", 0.5)
    };
    add_component_governance_edge_with_provenance(
        &conn,
        &edge,
        ComponentGovernanceRelation::Owns,
        &EdgeProvenance {
            authority: Some(EdgeAuthority::StructuralBookkeeping),
            ..EdgeProvenance::default()
        },
    )
    .unwrap();

    let edges = get_edges(&conn, "auth-cg-src", "outgoing", Some("owns")).unwrap();
    assert_eq!(edges.len(), 1);
    assert_eq!(
        edge_authority(&edges[0]),
        Some(EdgeAuthority::StructuralBookkeeping)
    );
}

/// tachi#1646 spot check: the contradiction pipeline — the only writer
/// census-classified `ModelReceiptBacked` — stamps that class on both graph
/// projections it writes, unconditionally (hard-coded in
/// `persist_confirmed_contradiction_within_tx`, not caller-configurable).
#[test]
fn confirmed_contradiction_transaction_stamps_model_receipt_backed_authority() {
    let mut conn = make_conn();
    upsert(&mut conn, &make_entry("auth-mrb-new", "new fact"), false).unwrap();
    upsert(&mut conn, &make_entry("auth-mrb-old", "old fact"), false).unwrap();
    let (contradicts, supersedes, at) =
        confirmed_contradiction_edges("auth-mrb-new", "auth-mrb-old", "complete");
    let expected_entry = expected_state(&conn, "auth-mrb-new");
    let expected = expected_state(&conn, "auth-mrb-old");

    let tx = conn
        .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
        .unwrap();
    persist_confirmed_contradiction_within_tx(
        &tx,
        &contradicts,
        &supersedes,
        &at,
        &expected_entry,
        &expected,
    )
    .unwrap();
    tx.commit().unwrap();

    let edges = get_edges(&conn, "auth-mrb-new", "outgoing", None).unwrap();
    assert_eq!(edges.len(), 2, "contradicts + supersedes");
    for edge in &edges {
        assert_eq!(
            edge_authority(edge),
            Some(EdgeAuthority::ModelReceiptBacked),
            "both contradiction-pipeline projections must stamp ModelReceiptBacked: {edge:?}"
        );
    }
}

#[test]
fn graph_expand_as_of_anchors_edge_validity_at_the_instant() {
    // Hyperion #3010: a point-in-time expansion must anchor BOTH edge bounds
    // at the requested instant — a replay neither leaks an edge created after
    // the instant nor loses an edge that was valid then but has since
    // expired. Plain expansion keeps the historical now-anchored predicate.
    let mut conn = make_conn();
    for id in [
        "root",
        "always-nbr",
        "future-nbr",
        "lapsed-nbr",
        "blank-nbr",
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

    let replay = graph_expand_as_of(
        &conn,
        &["root".into()],
        1,
        None,
        false,
        "2026-01-01T00:00:00Z",
    )
    .unwrap();
    assert!(replay.distances.contains_key("always-nbr"));
    assert!(
        replay.distances.contains_key("lapsed-nbr"),
        "an edge expired today was valid at the replay instant"
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
}
