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
        provenance: json!({"files": ["crates/memcore/src/types.rs"]}),
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
        json!("crates/memcore/src/types.rs")
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

#[test]
fn concurrent_same_id_different_payload_has_one_winner_and_one_collision() {
    crate::db::enable_simple_auto_extension().unwrap();
    register_sqlite_vec();
    let path = std::env::temp_dir().join(format!(
        "memcore-event-collision-{}.db",
        uuid::Uuid::new_v4()
    ));
    {
        let conn = Connection::open(&path).unwrap();
        init_schema(&conn).unwrap();
    }
    let base = TachiEventRecord {
        id: "shared-event-id".into(),
        source_repo: "sigil".into(),
        adapter: "concurrency-test".into(),
        project: "sigil".into(),
        domain: "testing".into(),
        session_id: "session-barrier".into(),
        actor: "agent".into(),
        event_type: "session.captured".into(),
        authority: AuthorityLevel::DerivedEvidence,
        effects: vec![EffectScope::Recall],
        projection_hints: vec![ProjectionKind::Pattern],
        payload: json!({"contender": "left"}),
        provenance: json!({"test": true}),
        created_at: "2026-07-19T00:00:00Z".into(),
    };
    let mut right = base.clone();
    right.payload = json!({"contender": "right"});
    let barrier = std::sync::Arc::new(std::sync::Barrier::new(2));
    let open_contender = || {
        let conn = Connection::open(&path).unwrap();
        conn.busy_timeout(std::time::Duration::from_secs(5))
            .unwrap();
        conn
    };
    // Open sequentially before the race. Connection initialization loads the
    // SQLite extensions and is not part of the event-admission invariant.
    let left_conn = open_contender();
    let right_conn = open_contender();
    let run = |conn: Connection, event: TachiEventRecord| {
        let barrier = barrier.clone();
        std::thread::spawn(move || {
            barrier.wait();
            insert_tachi_event_if_absent(&conn, &event).map(|inserted| (inserted, event))
        })
    };
    let left_handle = run(left_conn, base);
    let right_handle = run(right_conn, right);
    let results = [left_handle.join().unwrap(), right_handle.join().unwrap()];
    let winner = results
        .iter()
        .find_map(|result| match result {
            Ok((true, event)) => Some(event),
            _ => None,
        })
        .expect("one contender must insert");
    assert_eq!(
        results
            .iter()
            .filter(|result| matches!(result, Ok((true, _))))
            .count(),
        1
    );
    assert_eq!(
        results
            .iter()
            .filter(|result| matches!(result, Err(crate::MemoryError::InvalidArg(message)) if message.contains("collision")))
            .count(),
        1
    );
    let conn = Connection::open(&path).unwrap();
    let rows = list_tachi_events(
        &conn,
        &TachiEventQuery {
            limit: 10,
            ..Default::default()
        },
    )
    .unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].payload, winner.payload);
    drop(conn);
    let _ = std::fs::remove_file(path);
}
