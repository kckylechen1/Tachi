use super::*;

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
        last_access: None,
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
fn gc_purges_stale_non_terminal_dispatch_cards_only() {
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
    assert_eq!(deleted, 1, "only the stale dispatch card should be reaped");

    assert!(
        store.get("stale-working").expect("get stale").is_none(),
        "stale 31d-old WORKING dispatch card must be deleted"
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

/// INPUT_REQUIRED is normally reapable when an abandoned plan review has
/// aged out, but a durable partial-closure marker makes the same displayed
/// state terminal history that GC must retain.
#[test]
fn gc_keeps_old_partial_closed_input_required_but_reaps_open_plan_input() {
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
        deleted, 1,
        "only the open plan INPUT_REQUIRED card is reapable"
    );
    assert!(
        store.get("old-partial").expect("get partial").is_some(),
        "partial-closed INPUT_REQUIRED card must be retained as terminal history"
    );
    assert!(
        store.get("old-plan-input").expect("get plan").is_none(),
        "ordinary stale plan INPUT_REQUIRED card must remain GC-reapable"
    );
}
