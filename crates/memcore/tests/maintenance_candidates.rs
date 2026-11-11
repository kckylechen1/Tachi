use memcore::db::MaintenanceDisposition;
use memcore::{MemoryEntry, MemoryStore};
use serde_json::json;

const AS_OF: &str = "2026-08-13T00:00:00Z";

fn terminal(id: &str) -> MemoryEntry {
    serde_json::from_value(json!({
        "id": id, "path": format!("/kanban/cards/{id}"),
        "text": format!("PRIVATE-BODY-{id}"), "summary": format!("PRIVATE-SUMMARY-{id}"),
        "timestamp": "2020-01-01T00:00:00Z", "category": "kanban",
        "retention_policy": "ephemeral",
        "metadata": {"status": "resolved", "terminal_at": "2026-01-01T00:00:00Z"}
    }))
    .unwrap()
}

#[test]
fn background_sweep_uses_the_same_protections_as_candidate_inspection() {
    let mut store = MemoryStore::open_in_memory().unwrap();
    for id in [
        "eligible",
        "pinned",
        "recent-terminal",
        "unknown-terminal",
        "active",
        "referenced",
    ] {
        let mut entry = terminal(id);
        match id {
            "pinned" => entry.retention_policy = Some("pinned".to_string()),
            "recent-terminal" => entry.metadata["terminal_at"] = json!("2026-08-12T00:00:00Z"),
            "unknown-terminal" => {
                entry
                    .metadata
                    .as_object_mut()
                    .unwrap()
                    .remove("terminal_at");
            }
            "active" => {
                entry.category = "fact".to_string();
                entry.path = "/kanban/tasks/active".to_string();
                entry.metadata = json!({"a2a_state": "TASK_STATE_INPUT_REQUIRED"});
            }
            _ => {}
        }
        store.upsert(&entry).unwrap();
    }
    let mut source = terminal("source");
    source.category = "fact".to_string();
    source.path = "/notes/source".to_string();
    source.metadata = json!({"padding": "x".repeat(1200), "z": {"nested": [{"ref": "memory:referenced#revision-1"}]}});
    store.upsert(&source).unwrap();
    let report = store
        .inspect_maintenance_candidates("/kanban/", AS_OF, 30)
        .unwrap();
    assert!(report.reference_scan_complete);
    assert_eq!(
        report
            .candidates
            .iter()
            .filter(|c| c.disposition == MaintenanceDisposition::Eligible)
            .map(|c| c.id.as_str())
            .collect::<Vec<_>>(),
        vec!["eligible"]
    );
    assert_eq!(store.gc_expired_kanban_cards(AS_OF, 30).unwrap(), 1);
    assert!(store.get("eligible").unwrap().is_none());
    for id in [
        "pinned",
        "recent-terminal",
        "unknown-terminal",
        "active",
        "referenced",
        "source",
    ] {
        assert!(
            store.get(id).unwrap().is_some(),
            "protected row {id} was deleted"
        );
    }
}

#[test]
fn literal_prefix_inspection_is_body_free_and_does_not_classify_other_types_as_garbage() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("tachi-memory.db");
    let mut store = MemoryStore::open(&path.to_string_lossy()).unwrap();
    for (id, path) in [
        ("literal", "/test/%_/literal"),
        ("wildcard-neighbor", "/test/xx/neighbor"),
    ] {
        let mut entry = terminal(id);
        entry.path = path.to_string();
        entry.category = "fact".to_string();
        store.upsert(&entry).unwrap();
    }
    drop(store);
    let before = std::fs::read(&path).unwrap();
    let store = MemoryStore::open_read_only_immutable(&path.to_string_lossy()).unwrap();
    let report = store
        .inspect_maintenance_candidates("/test/%_/", AS_OF, 30)
        .unwrap();
    assert_eq!(report.candidates.len(), 1);
    assert_eq!(report.candidates[0].id, "literal");
    assert_eq!(
        report.candidates[0].disposition,
        MaintenanceDisposition::ManualReview
    );
    assert_eq!(report.candidates[0].row_hash.len(), 64);
    drop(store);
    assert_eq!(std::fs::read(&path).unwrap(), before);
    let serialized = serde_json::to_string(&report).unwrap();
    assert!(!serialized.contains("PRIVATE-BODY"));
    assert!(!serialized.contains("PRIVATE-SUMMARY"));
}

#[test]
#[cfg(feature = "admin")]
fn complete_event_dispatch_and_adjudication_references_are_reported_and_protected() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("tachi-memory.db");
    let mut store = MemoryStore::open(&path.to_string_lossy()).unwrap();
    for id in [
        "event-payload",
        "event-provenance",
        "dispatch-eval",
        "dispatch-evidence",
        "adjudication",
        "signature",
        "state-ref",
    ] {
        store.upsert(&terminal(id)).unwrap();
    }
    let fixture = rusqlite::Connection::open(&path).unwrap();
    fixture.execute(
        "INSERT INTO tachi_events(id,event_type,payload_json,provenance_json,created_at) VALUES ('event','fixture',?1,?2,?3)",
        rusqlite::params![
            json!({"padding": "x".repeat(1200), "z": {"id": "event-payload"}}).to_string(),
            json!({"nested": [{"source": "/kanban/cards/event-provenance"}]}).to_string(),
            AS_OF,
        ],
    ).unwrap();
    fixture.execute(
        "INSERT INTO dispatch_outcomes(outcome_id,eval_memory_id,execution_outcome,evidence_refs,idempotency_key) VALUES ('outcome','dispatch-eval','completed','[\"memory:dispatch-evidence\"]','fixture')", [],
    ).unwrap();
    fixture.execute(
        "INSERT INTO dispatch_adjudications(adjudication_id,outcome_id,event_key,verdict,actor,evidence_ref,insertion_seq) VALUES ('judgment','outcome','judgment-key','accepted','reviewer','memory:adjudication',1)", [],
    ).unwrap();
    fixture.execute(
        "INSERT INTO dispatch_adjudication_signatures(adjudication_id,signature_id,evidence_ref) VALUES ('judgment','sig','memory:signature')", [],
    ).unwrap();
    store
        .set_state("fixture", "ref", r#"{"id":"state-ref"}"#)
        .unwrap();
    let report = store
        .inspect_maintenance_candidates("/kanban/", AS_OF, 30)
        .unwrap();
    assert!(report.reference_scan_complete);
    for (id, table, field) in [
        ("event-payload", "tachi_events", "payload_json"),
        ("event-provenance", "tachi_events", "provenance_json"),
        ("dispatch-eval", "dispatch_outcomes", "eval_memory_id"),
        ("dispatch-evidence", "dispatch_outcomes", "evidence_refs"),
        ("adjudication", "dispatch_adjudications", "evidence_ref"),
        (
            "signature",
            "dispatch_adjudication_signatures",
            "evidence_ref",
        ),
        ("state-ref", "hard_state", "value_json"),
    ] {
        let candidate = report.candidates.iter().find(|row| row.id == id).unwrap();
        assert_eq!(candidate.disposition, MaintenanceDisposition::Protected);
        assert!(
            candidate
                .protecting_references
                .iter()
                .any(|r| r.table == table && r.field == field),
            "missing protecting reference for {id}"
        );
    }
    assert_eq!(store.gc_expired_kanban_cards(AS_OF, 30).unwrap(), 0);
}

#[test]
fn malformed_reference_evidence_prevents_automatic_deletion_but_remains_inspectable() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("tachi-memory.db");
    let mut store = MemoryStore::open(&path.to_string_lossy()).unwrap();
    store.upsert(&terminal("target")).unwrap();
    let fixture = rusqlite::Connection::open(&path).unwrap();
    fixture.execute(
        "INSERT INTO tachi_events(id,event_type,provenance_json,created_at) VALUES ('malformed','fixture','{broken',?1)", [AS_OF],
    ).unwrap();
    let report = store
        .inspect_maintenance_candidates("/kanban/", AS_OF, 30)
        .unwrap();
    assert!(!report.reference_scan_complete);
    assert_eq!(report.reference_scan_issues[0].source.row_id, "malformed");
    assert_eq!(
        report.candidates[0].disposition,
        MaintenanceDisposition::Protected
    );
    assert!(store
        .gc_expired_kanban_cards(AS_OF, 30)
        .unwrap_err()
        .to_string()
        .contains("reference scan incomplete"));
    assert!(store.get("target").unwrap().is_some());
}
