use super::*;

#[test]
fn delete_existing() {
    let mut conn = make_conn();
    let e = make_entry("del-1", "to be deleted");
    upsert(&mut conn, &e, false).unwrap();

    let deleted = delete(&mut conn, "del-1", false, StoreProfile::TachiFull).unwrap();
    assert!(deleted, "should return true for existing entry");

    // Verify it's gone from main table
    let count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM memories WHERE id = 'del-1'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(count, 0);

    // Verify it's gone from FTS
    let fts_results =
        search_fts(&conn, "deleted", 5, false, false, None, None, None, false).unwrap();
    assert!(!fts_results.contains_key("del-1"));
}

#[test]
fn delete_returns_vector_cleanup_errors_and_rolls_back() {
    let mut conn = make_conn();
    let has_vec: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE name = 'memories_vec'",
            [],
            |row| row.get(0),
        )
        .unwrap_or(0);
    if has_vec == 0 {
        return;
    }

    let e = make_entry("del-vec-error", "to be deleted after vector cleanup");
    upsert(&mut conn, &e, false).unwrap();
    conn.execute("DROP TABLE memories_vec", []).unwrap();

    let err = delete(&mut conn, "del-vec-error", true, StoreProfile::TachiFull)
        .expect_err("vector cleanup errors must be returned");
    assert!(
        err.to_string().contains("memories_vec") || err.to_string().contains("no such table"),
        "unexpected error: {err}"
    );

    let count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM memories WHERE id = 'del-vec-error'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(count, 1, "failed vector cleanup should roll back delete");
}

#[test]
fn delete_nonexistent() {
    let mut conn = make_conn();
    let deleted = delete(&mut conn, "nonexistent-id", false, StoreProfile::TachiFull).unwrap();
    assert!(!deleted, "should return false for non-existent entry");
}

#[test]
fn delete_cascades_access_history_and_known_state() {
    let mut conn = make_conn();
    let e = make_entry("del-cascade", "delete target");
    upsert(&mut conn, &e, false).unwrap();

    record_access(&conn, &["del-cascade".to_string()], &[], None).unwrap();
    update_agent_known_state(
        &conn,
        "agent-delete-test",
        &[("del-cascade".to_string(), 1)],
    )
    .unwrap();

    let ah_before: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM access_history WHERE memory_id = ?1",
            params!["del-cascade"],
            |row| row.get(0),
        )
        .unwrap();
    let aks_before: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM agent_known_state WHERE memory_id = ?1",
            params!["del-cascade"],
            |row| row.get(0),
        )
        .unwrap();
    assert!(ah_before > 0);
    assert!(aks_before > 0);

    delete(&mut conn, "del-cascade", false, StoreProfile::TachiFull).unwrap();

    let ah_after: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM access_history WHERE memory_id = ?1",
            params!["del-cascade"],
            |row| row.get(0),
        )
        .unwrap();
    let aks_after: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM agent_known_state WHERE memory_id = ?1",
            params!["del-cascade"],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(ah_after, 0, "access_history should be cleaned up on delete");
    assert_eq!(
        aks_after, 0,
        "agent_known_state should be cleaned up on delete"
    );
}

#[test]
fn projection_delete_rejects_stale_replay_and_preserves_source_events() {
    let mut conn = make_conn();
    let event = TachiEventRecord {
        id: "retirement-source-event".into(),
        source_repo: "tachi".into(),
        adapter: "retirement-regression".into(),
        project: "Tachi".into(),
        domain: "engineering".into(),
        session_id: "retirement-session".into(),
        actor: "test".into(),
        event_type: "timeline.observed".into(),
        authority: AuthorityLevel::CollectOnly,
        effects: vec![EffectScope::None],
        projection_hints: vec![ProjectionKind::Timeline],
        payload: json!({"projection_key": "closed-work", "text": "original evidence"}),
        provenance: json!({"source": "fixture"}),
        created_at: "2026-11-12T00:00:00Z".into(),
    };
    insert_tachi_event(&conn, &event).unwrap();
    let query = TachiEventQuery::default();
    let source_before = serde_json::to_string(&list_tachi_events(&conn, &query).unwrap()).unwrap();
    let mut projection = make_entry("projection-timeline-closed-work", "closed work projection");
    projection.path = "/timeline/engineering/closed-work".into();
    projection.source = "external:tachi_event_projection".into();
    projection.metadata = json!({"projection_kind": "timeline", "projection_key": "closed-work"});
    upsert(&mut conn, &projection, false).unwrap();

    assert!(delete(&mut conn, &projection.id, false, StoreProfile::TachiFull).unwrap());
    let error = upsert(&mut conn, &projection, false)
        .expect_err("a projector snapshot read before deletion must not recreate the projection");
    assert!(error.to_string().contains("retired"), "{error}");
    assert!(fetch_by_ids(&conn, &[projection.id.clone()], true)
        .unwrap()
        .is_empty());
    assert_eq!(
        serde_json::to_string(&list_tachi_events(&conn, &query).unwrap()).unwrap(),
        source_before,
        "retirement must not delete or rewrite source evidence"
    );

    projection.id = "projection-timeline-new-work".into();
    projection.path = "/timeline/engineering/new-work".into();
    projection.metadata = json!({"projection_kind": "timeline", "projection_key": "new-work"});
    upsert(&mut conn, &projection, false).expect("a distinct projection identity remains eligible");
}

fn projection_fixture(id: &str) -> MemoryEntry {
    let mut entry = make_entry(id, "projection lifecycle fixture");
    entry.path = "/timeline/engineering/retirement".into();
    entry.source = "external:tachi_event_projection".into();
    entry.metadata = json!({"projection_kind": "timeline"});
    entry
}

#[test]
fn projection_retirement_survives_reopen_and_concurrent_stale_writer() {
    use crate::{MemoryError, MemoryStore, PortableImportEntry};
    use std::sync::mpsc;
    use std::time::Duration;

    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("projection-retirement.db");
    let mut store = MemoryStore::open(path.to_str().unwrap()).unwrap();
    let projection = projection_fixture("projection-timeline-race");
    store.upsert(&projection).unwrap();
    let (read_tx, read_rx) = mpsc::channel();
    let (resume_tx, resume_rx) = mpsc::channel();
    let writer_path = path.clone();
    let id = projection.id.clone();
    let writer = std::thread::spawn(move || {
        let mut writer = MemoryStore::open(writer_path.to_str().unwrap()).unwrap();
        let stale = writer.get(&id).unwrap().unwrap();
        assert!(!writer.is_projection_retired(&id).unwrap());
        read_tx.send(()).unwrap();
        resume_rx.recv_timeout(Duration::from_secs(10)).unwrap();
        assert!(matches!(
            writer.upsert(&stale),
            Err(MemoryError::ProjectionRetired { memory_id }) if memory_id == id
        ));
    });
    read_rx.recv_timeout(Duration::from_secs(10)).unwrap();
    assert!(store.delete(&projection.id).unwrap());
    resume_tx.send(()).unwrap();
    writer.join().unwrap();
    drop(store);

    let mut reopened = MemoryStore::open(path.to_str().unwrap()).unwrap();
    assert!(reopened.get(&projection.id).unwrap().is_none());
    assert!(reopened.is_projection_retired(&projection.id).unwrap());
    assert!(matches!(
        reopened.insert_if_absent(&projection),
        Err(MemoryError::ProjectionRetired { .. })
    ));
    let ordinary = make_entry("unrelated-batch-row", "must roll back with retired row");
    assert!(matches!(
        reopened.upsert_batch(&[ordinary.clone(), projection.clone()]),
        Err(MemoryError::ProjectionRetired { .. })
    ));
    assert!(reopened.get(&ordinary.id).unwrap().is_none());
    let imported = PortableImportEntry {
        created_at: projection.timestamp.clone(),
        updated_at: projection.timestamp.clone(),
        superseded_by: None,
        entry: projection.clone(),
    };
    assert!(matches!(
        reopened.import_snapshot_batch(&[imported]),
        Err(MemoryError::ProjectionRetired { .. })
    ));
    assert!(reopened.get(&projection.id).unwrap().is_none());
    let marker = reopened
        .get_state_kv("continuity_projection_retirement", &projection.id)
        .unwrap();
    assert!(!reopened.delete(&projection.id).unwrap());
    assert_eq!(
        reopened
            .get_state_kv("continuity_projection_retirement", &projection.id)
            .unwrap(),
        marker
    );
}

#[test]
fn projection_retirement_is_atomic_and_does_not_reserve_ordinary_or_missing_ids() {
    use crate::db::{is_projection_retired, memory_crud::delete_memory_within_tx};

    let mut conn = make_conn();
    let projection = projection_fixture("projection-timeline-rolled-back");
    upsert(&mut conn, &projection, false).unwrap();
    assert!(
        delete_memory_within_tx(&conn, &projection.id, false, StoreProfile::TachiFull).is_err()
    );
    {
        let tx = conn
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
            .unwrap();
        assert!(
            delete_memory_within_tx(&tx, &projection.id, false, StoreProfile::TachiFull).unwrap()
        );
        assert!(is_projection_retired(&tx, &projection.id).unwrap());
        // A receipt/precommit failure must roll back the marker with the row.
        tx.rollback().unwrap();
    }
    assert!(!is_projection_retired(&conn, &projection.id).unwrap());
    assert_eq!(
        fetch_by_ids(&conn, &[projection.id.clone()], true)
            .unwrap()
            .len(),
        1
    );
    upsert(&mut conn, &projection, false).unwrap();

    let mut ordinary = make_entry("projection-timeline-ordinary", "ordinary timeline note");
    ordinary.path = "/timeline/engineering/ordinary".into();
    ordinary.metadata = json!({"projection_kind": "timeline"});
    upsert(&mut conn, &ordinary, false).unwrap();
    assert!(delete(&mut conn, &ordinary.id, false, StoreProfile::TachiFull).unwrap());
    assert!(!is_projection_retired(&conn, &ordinary.id).unwrap());
    upsert(&mut conn, &ordinary, false).unwrap();

    let future = projection_fixture("projection-timeline-not-yet-created");
    assert!(!delete(&mut conn, &future.id, false, StoreProfile::TachiFull).unwrap());
    assert!(!is_projection_retired(&conn, &future.id).unwrap());
    upsert(&mut conn, &future, false).unwrap();
}

#[test]
fn projection_retirement_namespace_rejects_generic_mutation_and_ttl_cleanup() {
    use crate::db::{
        backfill_missing_expires_at, delete_state, get_state, insert_state_if_absent,
        is_projection_retired, reap_expired_state, set_state, set_state_if_version,
    };

    let mut conn = make_conn();
    let projection = projection_fixture("projection-timeline-permanent");
    let namespace = "continuity_projection_retirement";
    assert!(set_state(&conn, namespace, "forged", "{}").is_err());
    assert!(insert_state_if_absent(&conn, namespace, "forged", "{}").is_err());
    assert!(get_state(&conn, namespace, "forged").unwrap().is_none());
    upsert(&mut conn, &projection, false).unwrap();
    delete(&mut conn, &projection.id, false, StoreProfile::TachiFull).unwrap();
    let before = get_state(&conn, namespace, &projection.id)
        .unwrap()
        .unwrap();
    let value: serde_json::Value = serde_json::from_str(&before.0).unwrap();
    assert_eq!(value["memory_id"], projection.id);
    assert_eq!(value["schema_version"], 1);
    assert!(value.get("expires_at").is_none());
    assert_eq!(
        value.as_object().unwrap().len(),
        3,
        "marker must not copy memory/event bodies"
    );
    assert!(set_state(&conn, namespace, &projection.id, "{}").is_err());
    assert!(insert_state_if_absent(&conn, namespace, &projection.id, "{}").is_err());
    assert!(set_state_if_version(&conn, namespace, &projection.id, "{}", before.1).is_err());
    assert!(delete_state(&conn, namespace, &projection.id).is_err());
    assert_eq!(
        backfill_missing_expires_at(&conn, namespace, "2000-01-01T00:00:00Z", None).unwrap(),
        0
    );
    assert_eq!(
        get_state(&conn, namespace, &projection.id)
            .unwrap()
            .unwrap(),
        before
    );

    // Direct fixture SQL models damaged/legacy state; no production API can
    // install a TTL or malformed value in this protected namespace.
    conn.execute(
        "UPDATE hard_state SET value_json = ?1 WHERE namespace = ?2 AND key = ?3",
        params![
            r#"{"expires_at":"2000-01-01T00:00:00Z"}"#,
            namespace,
            projection.id
        ],
    )
    .unwrap();
    set_state(
        &conn,
        "ordinary-expiring-state",
        "expired",
        r#"{"expires_at":"2000-01-01T00:00:00Z"}"#,
    )
    .unwrap();
    assert_eq!(
        reap_expired_state(&conn, "2026-11-12T00:00:00Z").unwrap(),
        1
    );
    assert!(is_projection_retired(&conn, &projection.id).unwrap());
    conn.execute(
        "UPDATE hard_state SET value_json = 'damaged marker' WHERE namespace = ?1 AND key = ?2",
        params![namespace, projection.id],
    )
    .unwrap();
    assert!(is_projection_retired(&conn, &projection.id).unwrap());
    assert!(matches!(
        upsert(&mut conn, &projection, false),
        Err(crate::MemoryError::ProjectionRetired { .. })
    ));
}
