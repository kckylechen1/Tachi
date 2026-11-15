use super::*;
use memcore::MemoryEntry;
use serde_json::json;

fn test_store() -> MemoryStore {
    MemoryStore::open_in_memory().expect("test memory store")
}

/// Build a dispatch ("board") card mirroring how `init_kanban_task` writes
/// them: `category=fact`, path under `/kanban/tasks/`, `retention_policy=Pinned`,
/// lifecycle in `metadata.a2a_state`.
fn dispatch_card_entry(id: &str, a2a_state: &str, timestamp: String) -> MemoryEntry {
    MemoryEntry {
        id: id.to_string(),
        path: format!("{KANBAN_DISPATCH_PATH_PREFIX}{id}"),
        summary: format!("Kanban dispatch {id}"),
        text: "Dispatch Task".to_string(),
        importance: 0.7,
        timestamp,
        valid_from: String::new(),
        valid_until: None,
        category: "fact".to_string(),
        topic: "kanban".to_string(),
        keywords: vec!["kanban".to_string(), "dispatch".to_string()],
        persons: vec![],
        entities: vec![],
        location: String::new(),
        source: "test".to_string(),
        scope: "project".to_string(),
        archived: false,
        access_count: 0,
        scored_count: 0,
        last_access: None,
        last_use_at: None,
        revision: 1,
        vector: None,
        metadata: json!({
            "type": "a2a_task",
            "dispatch_id": id,
            "a2a_state": a2a_state,
        }),
        retention_policy: Some(memcore::RetentionPolicy::Pinned.as_str().to_string()),
        domain: Some("system".to_string()),
        recall_count: 0,
        query_diversity: 0,
        tier: "raw".to_string(),
    }
}

#[test]
fn gc_preserves_stale_and_fresh_non_terminal_dispatch_cards() {
    let mut store = test_store();

    let stale = dispatch_card_entry(
        "stale-working",
        "TASK_STATE_WORKING",
        (chrono::Utc::now() - chrono::Duration::days(31)).to_rfc3339(),
    );
    store.upsert(&stale).expect("upsert stale dispatch card");

    let fresh = dispatch_card_entry(
        "fresh-working",
        "TASK_STATE_WORKING",
        chrono::Utc::now().to_rfc3339(),
    );
    store.upsert(&fresh).expect("upsert fresh dispatch card");

    let deleted = gc_expired_kanban_cards(&mut store, DEFAULT_KANBAN_GC_MAX_AGE_DAYS).expect("gc");
    assert_eq!(
        deleted, 0,
        "age never authorizes deleting active dispatch work"
    );

    assert!(
        store.get("stale-working").expect("get stale").is_some(),
        "stale 31d-old WORKING dispatch card must be retained"
    );
    assert!(
        store.get("fresh-working").expect("get fresh").is_some(),
        "fresh WORKING dispatch card must be retained"
    );
}

#[test]
fn gc_keeps_old_terminal_dispatch_cards() {
    let mut store = test_store();

    let old_completed = dispatch_card_entry(
        "old-completed",
        "TASK_STATE_COMPLETED",
        (chrono::Utc::now() - chrono::Duration::days(31)).to_rfc3339(),
    );
    store
        .upsert(&old_completed)
        .expect("upsert old completed dispatch card");

    let deleted = gc_expired_kanban_cards(&mut store, DEFAULT_KANBAN_GC_MAX_AGE_DAYS).expect("gc");
    assert_eq!(deleted, 0, "terminal dispatch cards must not be reaped");
    assert!(
        store.get("old-completed").expect("get").is_some(),
        "terminal (COMPLETED) dispatch card must be retained"
    );
}

/// An unfinished plan remains active regardless of age. Partial closure is
/// terminal history, but both fixtures retain their explicit pin.
#[test]
fn gc_keeps_old_partial_closed_and_open_plan_input() {
    let mut store = test_store();
    let old = (chrono::Utc::now() - chrono::Duration::days(31)).to_rfc3339();

    let mut partial = dispatch_card_entry("old-partial", "TASK_STATE_INPUT_REQUIRED", old.clone());
    partial.metadata["closure_kind"] = json!("partial");
    store
        .upsert(&partial)
        .expect("upsert partial dispatch card");

    let plan = dispatch_card_entry("old-plan-input", "TASK_STATE_INPUT_REQUIRED", old);
    store.upsert(&plan).expect("upsert plan dispatch card");

    let deleted = gc_expired_kanban_cards(&mut store, DEFAULT_KANBAN_GC_MAX_AGE_DAYS).expect("gc");
    assert_eq!(
        deleted, 0,
        "neither unfinished work nor pinned history is reapable"
    );
    assert!(
        store.get("old-partial").expect("get partial").is_some(),
        "partial-closed INPUT_REQUIRED card must be retained as terminal history"
    );
    assert!(
        store.get("old-plan-input").expect("get plan").is_some(),
        "ordinary stale plan INPUT_REQUIRED card must remain protected"
    );
}

#[test]
fn gc_reaps_only_unpinned_unreferenced_cards_aged_from_terminal_time() {
    let mut store = test_store();
    let old = (chrono::Utc::now() - chrono::Duration::days(60)).to_rfc3339();
    let recent = (chrono::Utc::now() - chrono::Duration::days(1)).to_rfc3339();
    for id in [
        "eligible",
        "recent-terminal",
        "unknown-terminal",
        "pinned",
        "referenced",
    ] {
        let mut entry = dispatch_card_entry(id, "TASK_STATE_COMPLETED", old.clone());
        entry.category = "kanban".to_string();
        entry.path = format!("/kanban/cards/{id}");
        entry.metadata["status"] = json!("resolved");
        entry.metadata["terminal_at"] = json!(if id == "recent-terminal" {
            &recent
        } else {
            &old
        });
        entry.retention_policy = Some(
            if id == "pinned" {
                "pinned"
            } else {
                "ephemeral"
            }
            .to_string(),
        );
        if id == "unknown-terminal" {
            entry
                .metadata
                .as_object_mut()
                .unwrap()
                .remove("terminal_at");
        }
        store.upsert(&entry).expect("seed terminal card");
    }
    let mut reference = dispatch_card_entry("reference-source", "TASK_STATE_WORKING", old);
    reference.path = "/notes/reference-source".to_string();
    reference.metadata = json!({
        "padding": "x".repeat(1000),
        "z_evidence": {"nested": ["memory:referenced"]}
    });
    store
        .upsert(&reference)
        .expect("seed complete metadata reference");

    assert_eq!(gc_expired_kanban_cards(&mut store, 30).expect("gc"), 1);
    assert!(store.get("eligible").unwrap().is_none());
    for id in [
        "recent-terminal",
        "unknown-terminal",
        "pinned",
        "referenced",
        "reference-source",
    ] {
        assert!(
            store.get(id).unwrap().is_some(),
            "protected card {id} was deleted"
        );
    }
}

#[test]
fn gc_reaps_aged_terminal_dispatch_with_explicit_ephemeral_retention() {
    let mut store = test_store();
    let old = (chrono::Utc::now() - chrono::Duration::days(31)).to_rfc3339();
    let mut entry = dispatch_card_entry("eligible-dispatch", "TASK_STATE_COMPLETED", old.clone());
    entry.metadata["terminal_at"] = json!(old);
    entry.retention_policy = Some("ephemeral".to_string());
    store.upsert(&entry).unwrap();
    assert_eq!(gc_expired_kanban_cards(&mut store, 30).unwrap(), 1);
    assert!(store.get("eligible-dispatch").unwrap().is_none());
}
