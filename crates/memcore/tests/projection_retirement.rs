//! Public-store regressions, runnable without the large memcore lib-test binary.
//! These tests also compile against the pre-fix kernel for red/green evidence.

use memcore::{
    AuthorityLevel, EffectScope, MemoryEntry, MemoryStore, PortableImportEntry, ProjectionKind,
    TachiEventQuery, TachiEventRecord,
};
use serde_json::json;
use std::sync::mpsc;
use std::time::Duration;

const RETIREMENTS: &str = "continuity_projection_retirement";

fn projection(id: &str) -> MemoryEntry {
    serde_json::from_value(json!({
        "id": id,
        "path": "/timeline/engineering/retirement-regression",
        "text": "projection body is not retirement evidence",
        "timestamp": "2026-11-12T00:00:00Z",
        "source": "external:tachi_event_projection",
        "metadata": {"projection_kind": "timeline"},
    }))
    .unwrap()
}

#[test]
fn deleted_projection_rejects_concurrent_stale_writer_and_survives_reopen() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("retirement.db");
    let mut store = MemoryStore::open(path.to_str().unwrap()).unwrap();
    let entry = projection("projection-timeline-retired");
    let event = TachiEventRecord {
        id: "source-evidence".into(),
        source_repo: "tachi".into(),
        adapter: "retirement-regression".into(),
        project: "".into(),
        domain: "engineering".into(),
        session_id: "fixture".into(),
        actor: "test".into(),
        event_type: "timeline.observed".into(),
        authority: AuthorityLevel::CollectOnly,
        effects: vec![EffectScope::None],
        projection_hints: vec![ProjectionKind::Timeline],
        payload: json!({"projection_key": "retired", "text": "original source evidence"}),
        provenance: json!({"fixture": true}),
        created_at: "2026-11-12T00:00:00Z".into(),
    };
    store.insert_tachi_event(&event).unwrap();
    store.upsert(&entry).unwrap();
    let source_before = serde_json::to_vec(
        &store
            .list_tachi_events(&TachiEventQuery::default())
            .unwrap(),
    )
    .unwrap();
    let (read_tx, read_rx) = mpsc::channel();
    let (resume_tx, resume_rx) = mpsc::channel();
    let writer_path = path.clone();
    let id = entry.id.clone();
    let writer = std::thread::spawn(move || {
        let mut replay = MemoryStore::open(writer_path.to_str().unwrap()).unwrap();
        let stale = replay.get(&id).unwrap().unwrap();
        read_tx.send(()).unwrap();
        resume_rx.recv_timeout(Duration::from_secs(10)).unwrap();
        let error = replay.upsert(&stale).expect_err(
            "a projector that read before deletion must not recreate the committed deleted row",
        );
        assert!(error.to_string().contains("retired"), "{error}");
    });
    let ready = read_rx.recv_timeout(Duration::from_secs(10));
    if ready.is_err() {
        let _ = resume_tx.send(());
        let _ = writer.join();
        panic!("stale writer did not reach read barrier: {ready:?}");
    }
    let deleted = store.delete(&entry.id);
    let _ = resume_tx.send(());
    let replay_result = writer.join();
    assert!(deleted.unwrap());
    replay_result.unwrap();
    drop(store);

    let mut reopened = MemoryStore::open(path.to_str().unwrap()).unwrap();
    assert!(reopened.get(&entry.id).unwrap().is_none());
    let marker = reopened
        .get_state_kv(RETIREMENTS, &entry.id)
        .unwrap()
        .unwrap();
    let value: serde_json::Value = serde_json::from_str(&marker.0).unwrap();
    assert_eq!(value["memory_id"], entry.id);
    assert_eq!(value.as_object().unwrap().len(), 3);
    assert!(value.get("expires_at").is_none());
    assert!(!reopened.delete(&entry.id).unwrap());
    assert_eq!(
        reopened
            .get_state_kv(RETIREMENTS, &entry.id)
            .unwrap()
            .unwrap(),
        marker
    );
    assert_eq!(
        serde_json::to_vec(
            &reopened
                .list_tachi_events(&TachiEventQuery::default())
                .unwrap()
        )
        .unwrap(),
        source_before,
        "retirement and replay must preserve source events"
    );
    reopened
        .upsert(&projection("projection-timeline-fresh"))
        .unwrap();
    let absent = projection("projection-timeline-not-yet-created");
    assert!(!reopened.delete(&absent.id).unwrap());
    reopened.upsert(&absent).unwrap();
    let mut ordinary = projection("projection-timeline-ordinary");
    ordinary.source = "manual".into();
    reopened.upsert(&ordinary).unwrap();
    assert!(reopened.delete(&ordinary.id).unwrap());
    reopened.upsert(&ordinary).unwrap();
    assert!(reopened
        .get_state_kv(RETIREMENTS, &ordinary.id)
        .unwrap()
        .is_none());
}

#[test]
fn retired_identity_blocks_alternative_writers_and_generic_state_mutations() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("retirement-seams.db");
    let mut store = MemoryStore::open(path.to_str().unwrap()).unwrap();
    let entry = projection("projection-timeline-protected");
    store.upsert(&entry).unwrap();
    assert!(store.delete(&entry.id).unwrap());
    for result in [
        store.upsert(&entry).map(|_| ()),
        store.insert_if_absent(&entry).map(|_| ()),
        store.upsert_idless(&entry, "stale-projection").map(|_| ()),
    ] {
        let error = result.expect_err("every ordinary write seam must refuse a retired ID");
        assert!(error.to_string().contains("retired"), "{error}");
    }
    let fresh = projection("projection-timeline-rollback-control");
    assert!(store.upsert_batch(&[fresh.clone(), entry.clone()]).is_err());
    assert!(
        store.get(&fresh.id).unwrap().is_none(),
        "batch must roll back atomically"
    );
    let imported = PortableImportEntry {
        created_at: entry.timestamp.clone(),
        updated_at: entry.timestamp.clone(),
        superseded_by: None,
        entry: entry.clone(),
    };
    assert!(store.import_snapshot_batch(&[imported]).is_err());
    assert!(store.get(&entry.id).unwrap().is_none());

    let marker = store.get_state_kv(RETIREMENTS, &entry.id).unwrap().unwrap();
    assert!(store.set_state(RETIREMENTS, &entry.id, "{}").is_err());
    assert!(store
        .insert_state_if_absent(RETIREMENTS, "forged", "{}")
        .is_err());
    assert!(store
        .set_state_if_version(RETIREMENTS, &entry.id, "{}", marker.1)
        .is_err());
    assert!(store.delete_state(RETIREMENTS, &entry.id).is_err());
    assert_eq!(
        store
            .backfill_missing_expires_at(RETIREMENTS, "2000-01-01T00:00:00Z", None)
            .unwrap(),
        0
    );
    store
        .set_state(
            "ordinary-state",
            "expired",
            r#"{"expires_at":"2000-01-01T00:00:00Z"}"#,
        )
        .unwrap();
    assert_eq!(store.reap_expired_state("2026-11-12T00:00:00Z").unwrap(), 1);
    assert_eq!(
        store.get_state_kv(RETIREMENTS, &entry.id).unwrap().unwrap(),
        marker
    );
}
