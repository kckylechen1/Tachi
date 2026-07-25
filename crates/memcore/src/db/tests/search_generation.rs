use crate::{MemoryEdge, MemoryEntry, MemoryStore};

fn entry(id: &str, text: &str) -> MemoryEntry {
    MemoryEntry {
        id: id.to_string(),
        path: format!("/scratch/generation/{id}"),
        summary: text.to_string(),
        text: text.to_string(),
        importance: 0.7,
        timestamp: "2026-07-25T00:00:00Z".to_string(),
        valid_from: String::new(),
        valid_until: None,
        category: "fact".to_string(),
        topic: String::new(),
        keywords: Vec::new(),
        persons: Vec::new(),
        entities: Vec::new(),
        location: String::new(),
        source: "manual".to_string(),
        scope: "general".to_string(),
        archived: false,
        access_count: 0,
        last_access: None,
        revision: 1,
        vector: None,
        retention_policy: None,
        domain: None,
        metadata: serde_json::json!({}),
        recall_count: 0,
        query_diversity: 0,
        tier: "raw".to_string(),
    }
}

fn edge(source_id: &str, target_id: &str, weight: f64) -> MemoryEdge {
    MemoryEdge {
        source_id: source_id.to_string(),
        target_id: target_id.to_string(),
        relation: "references".to_string(),
        weight,
        metadata: serde_json::json!({}),
        created_at: "2026-07-25T00:00:00Z".to_string(),
        valid_from: String::new(),
        valid_to: None,
    }
}

#[test]
fn rollback_does_not_publish_search_generation() {
    let mut store = MemoryStore::open_in_memory().expect("open store");
    let initial = store.search_generation().expect("initial generation");
    let tx = store.connection_mut().transaction().expect("transaction");
    tx.execute(
        "INSERT INTO memories (id, path, text, timestamp) VALUES ('rolled-back', '/scratch/generation/rollback', 'rolled back', '2026-07-25T00:00:00Z')",
        [],
    )
    .expect("memory insert");
    crate::db::bump_search_generation(&tx).expect("projection bump");
    tx.rollback().expect("rollback");

    assert_eq!(store.search_generation().expect("after rollback"), initial);
}

#[test]
fn missing_trigger_refuses_search_generation() {
    let store = MemoryStore::open_in_memory().expect("open store");
    store
        .connection()
        .execute_batch("DROP TRIGGER memory_edge_search_generation_after_update")
        .expect("drop trigger");

    let error = store
        .search_generation()
        .expect_err("missing edge trigger must fail closed");
    assert!(
        error
            .to_string()
            .contains("memory_edge_search_generation_after_update"),
        "unexpected error: {error}"
    );
}

#[test]
fn overflow_aborts_without_wrapping() {
    let store = MemoryStore::open_in_memory().expect("open store");
    store
        .connection()
        .execute(
            "UPDATE memory_search_generation SET generation = ?1 WHERE id = 1",
            [i64::MAX],
        )
        .expect("set max generation");

    let error = crate::db::bump_search_generation(store.connection())
        .expect_err("projection bump must not wrap");
    assert!(
        error.to_string().contains("generation exhausted"),
        "{error}"
    );
    assert_eq!(
        store.search_generation().expect("generation readable"),
        i64::MAX
    );
}

#[test]
fn edge_insert_update_delete_advance_generation_and_edge_rollback_does_not() {
    let mut store = MemoryStore::open_in_memory().expect("open store");
    store
        .upsert(&entry("source", "source needle"))
        .expect("source");
    store
        .upsert(&entry("target", "target needle"))
        .expect("target");
    let after_memories = store.search_generation().expect("memory generation");

    store
        .add_edge(&edge("source", "target", 0.4))
        .expect("insert edge");
    assert_eq!(
        store.search_generation().expect("after insert"),
        after_memories + 1
    );

    store
        .add_edge(&edge("source", "target", 0.8))
        .expect("update edge");
    assert_eq!(
        store.search_generation().expect("after update"),
        after_memories + 2
    );

    let tx = store
        .connection_mut()
        .transaction()
        .expect("edge rollback tx");
    tx.execute(
        "DELETE FROM memory_edges WHERE source_id = 'source' AND target_id = 'target'",
        [],
    )
    .expect("delete inside rollback");
    tx.rollback().expect("rollback edge delete");
    assert_eq!(
        store.search_generation().expect("after rollback"),
        after_memories + 2
    );

    store
        .remove_edge("source", "target", "references")
        .expect("delete edge");
    assert_eq!(
        store.search_generation().expect("after delete"),
        after_memories + 3
    );
}

#[test]
fn read_only_store_can_validate_search_generation_without_mutation() {
    let temp = tempfile::tempdir().expect("temporary database directory");
    let path = temp.path().join("generation.sqlite");
    let mut writable = MemoryStore::open(path.to_str().expect("utf8 path")).expect("open writer");
    writable
        .upsert(&entry("readonly", "read-only generation"))
        .expect("seed generation");
    let expected = writable.search_generation().expect("writer generation");
    drop(writable);

    let readonly = MemoryStore::open_read_only(path.to_str().expect("utf8 path"))
        .expect("open read-only store");
    assert_eq!(
        readonly
            .search_generation()
            .expect("validate generation read-only"),
        expected
    );
}
