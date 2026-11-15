use portable_kernel::{MemoryEntry, MemoryStore};
use serde_json::json;

fn entry(id: &str, path: &str, key: &str, agent: &str) -> MemoryEntry {
    MemoryEntry {
        id: id.into(),
        path: path.into(),
        summary: id.into(),
        text: "body ".repeat(410),
        importance: 0.5,
        timestamp: "2026-07-09T00:00:00Z".into(),
        valid_from: String::new(),
        valid_until: None,
        category: "fact".into(),
        topic: String::new(),
        keywords: vec![],
        persons: vec![],
        entities: vec![],
        location: String::new(),
        source: "manual".into(),
        scope: "general".into(),
        archived: false,
        access_count: 0,
        scored_count: 0,
        last_access: None,
        last_use_at: None,
        revision: 1,
        vector: None,
        retention_policy: None,
        domain: None,
        metadata: json!({"host_key": key, "host_agent": agent}),
        recall_count: 0,
        query_diversity: 0,
        tier: "raw".into(),
    }
}

#[test]
fn exact_metadata_candidates_equal_the_complete_legacy_projection() {
    let mut store = MemoryStore::open_in_memory().unwrap();
    for index in 0..96 {
        let id = format!("unrelated-{index}");
        store
            .upsert(&entry(&id, &format!("/agents/a_one/{id}"), &id, "a"))
            .unwrap();
    }
    for (id, path, agent) in [
        ("canonical", "/agents/a_one/z/canonical", "a"),
        ("legacy", "/agents/a_one/a/legacy", "a"),
        ("foreign", "/agents/a_one/foreign", "b"),
        ("wildcard", "/agents/axone/other", "a"),
        ("case", "/agents/A_one/other", "a"),
    ] {
        store.upsert(&entry(id, path, "shared-key", agent)).unwrap();
    }
    let mut oracle = store
        .list_memories_by_path_prefix("/agents/a_one/%")
        .unwrap();
    oracle.retain(|row| {
        let metadata: serde_json::Value = serde_json::from_str(&row.metadata).unwrap();
        row.path.starts_with("/agents/a_one/")
            && metadata["host_key"] == "shared-key"
            && metadata["host_agent"] == "a"
    });
    oracle.sort_by(|a, b| {
        a.path
            .cmp(&b.path)
            .then_with(|| b.timestamp.cmp(&a.timestamp))
    });
    let actual = store
        .find_metadata_key_candidates(
            "/agents/a_one/",
            &[("host_key", "shared-key"), ("host_agent", "a")],
            8,
        )
        .unwrap();
    assert_eq!(actual, oracle);
    assert_eq!(
        actual.iter().map(|row| row.id.as_str()).collect::<Vec<_>>(),
        ["legacy", "canonical"]
    );
    assert!(store
        .find_metadata_key_candidates("/agents/a_one/", &[("host_key", "absent")], 1)
        .unwrap()
        .is_empty());
}

#[test]
fn archived_duplicates_remain_visible_but_over_budget_never_truncates() {
    let mut store = MemoryStore::open_in_memory().unwrap();
    let mut archived = entry("old", "/agents/a/legacy", "same", "a");
    archived.archived = true;
    store.upsert(&archived).unwrap();
    store
        .upsert(&entry("new", "/agents/a/current", "same", "a"))
        .unwrap();
    let predicates = [("host_key", "same"), ("host_agent", "a")];
    let rows = store
        .find_metadata_key_candidates("/agents/a/", &predicates, 2)
        .unwrap();
    assert_eq!(rows.len(), 2);
    assert!(store
        .find_metadata_key_candidates("/agents/a/", &predicates, 1)
        .is_err());
    assert!(
        store
            .get_with_options("old", true)
            .unwrap()
            .unwrap()
            .archived
    );
    assert!(store
        .find_metadata_key_candidates("/agents/a/", &[], 8)
        .is_err());
}
