use super::*;

#[test]
fn tachi_event_ledger_round_trips_typed_projection_metadata() {
    let conn = make_conn();
    let event = TachiEventRecord {
        id: "event-1".to_string(),
        source_repo: "sigil".to_string(),
        adapter: "memory-server-test".to_string(),
        project: "sigil".to_string(),
        domain: "architecture".to_string(),
        session_id: "session-1".to_string(),
        actor: "agent".to_string(),
        event_type: "pattern.observed".to_string(),
        authority: AuthorityLevel::DerivedEvidence,
        effects: vec![EffectScope::Recall, EffectScope::ProjectCycle],
        projection_hints: vec![ProjectionKind::Pattern, ProjectionKind::ProjectCycle],
        payload: json!({"summary": "shared continuity event ABI"}),
        provenance: json!({"files": ["crates/memory-core/src/types.rs"]}),
        created_at: "2026-06-22T00:00:00.000Z".to_string(),
    };

    insert_tachi_event(&conn, &event).expect("insert event");

    let rows = list_tachi_events(
        &conn,
        &TachiEventQuery {
            project: Some("sigil".to_string()),
            domain: Some("architecture".to_string()),
            event_type: Some("pattern.observed".to_string()),
            session_id: Some("session-1".to_string()),
            source_repo: Some("sigil".to_string()),
            adapter: Some("memory-server-test".to_string()),
            limit: 10,
        },
    )
    .expect("list events");

    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].id, "event-1");
    assert_eq!(rows[0].authority, AuthorityLevel::DerivedEvidence);
    assert_eq!(
        rows[0].effects,
        vec![EffectScope::Recall, EffectScope::ProjectCycle]
    );
    assert_eq!(
        rows[0].projection_hints,
        vec![ProjectionKind::Pattern, ProjectionKind::ProjectCycle]
    );
    assert_eq!(
        rows[0].payload["summary"],
        json!("shared continuity event ABI")
    );
    assert_eq!(
        rows[0].provenance["files"][0],
        json!("crates/memory-core/src/types.rs")
    );
}

#[test]
fn try_claim_event_deduplicates() {
    let conn = make_conn();
    let claimed_first = try_claim_event(&conn, "hash-claim-1", "evt-1", "ingest").unwrap();
    assert!(claimed_first, "first claim should succeed");

    let claimed_again = try_claim_event(&conn, "hash-claim-1", "evt-1", "ingest").unwrap();
    assert!(!claimed_again, "duplicate claim should be rejected");
}

#[test]
fn release_event_claim_allows_reclaim() {
    let conn = make_conn();
    try_claim_event(&conn, "hash-rel-1", "evt-1", "ingest").unwrap();
    release_event_claim(&conn, "hash-rel-1", "ingest").unwrap();

    let reclaimed = try_claim_event(&conn, "hash-rel-1", "evt-1", "ingest").unwrap();
    assert!(reclaimed, "should be able to reclaim after release");
}

#[test]
fn release_event_claim_idempotent() {
    let conn = make_conn();
    release_event_claim(&conn, "nonexistent", "worker").unwrap();
}
