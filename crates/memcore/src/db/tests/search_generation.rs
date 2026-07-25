use std::collections::BTreeSet;

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

#[test]
fn every_memory_entry_column_and_access_history_mutation_is_generation_covered() {
    let mut store = MemoryStore::open_in_memory().expect("open store");
    store
        .upsert(&entry("audit", "dynamic trigger audit"))
        .expect("seed memory");

    let memory_fields = serde_json::to_value(entry("shape", "shape"))
        .expect("serialize MemoryEntry")
        .as_object()
        .expect("MemoryEntry object")
        .keys()
        .cloned()
        .collect::<BTreeSet<_>>();
    let mut stmt = store
        .connection()
        .prepare("PRAGMA table_info(memories)")
        .expect("prepare memories columns");
    let memory_columns = stmt
        .query_map([], |row| row.get::<_, String>(1))
        .expect("query memories columns")
        .collect::<Result<BTreeSet<_>, _>>()
        .expect("collect memories columns");
    let derived_fields = ["location", "persons", "vector"]
        .into_iter()
        .map(str::to_string)
        .collect::<BTreeSet<_>>();
    let unmapped = memory_fields
        .difference(&memory_columns)
        .filter(|field| !derived_fields.contains(*field))
        .cloned()
        .collect::<Vec<_>>();
    assert!(
        unmapped.is_empty(),
        "new MemoryEntry fields need an explicit storage/projection audit: {unmapped:?}"
    );

    let update_trigger: String = store
        .connection()
        .query_row(
            "SELECT sql FROM sqlite_master WHERE type = 'trigger' AND name = 'memory_search_generation_after_update'",
            [],
            |row| row.get(0),
        )
        .expect("memory update trigger");
    let normalized = update_trigger
        .chars()
        .filter(|character| !character.is_whitespace())
        .flat_map(char::to_lowercase)
        .collect::<String>();
    assert!(normalized.contains("afterupdateonmemories"));
    assert!(
        !normalized.contains("updateof"),
        "an UPDATE OF allowlist lets future result-visible columns drift outside generation coverage"
    );

    let before_access_fields = store.search_generation().expect("before access fields");
    store
        .connection()
        .execute(
            "UPDATE memories
             SET access_count = access_count + 1,
                 last_access = '2026-07-26T00:00:00Z',
                 recall_count = recall_count + 1,
                 query_diversity = query_diversity + 1
             WHERE id = 'audit'",
            [],
        )
        .expect("update result-visible access fields");
    assert_eq!(
        store.search_generation().expect("after access fields"),
        before_access_fields + 1
    );

    let before_history = store.search_generation().expect("before history insert");
    store
        .connection()
        .execute(
            "INSERT INTO access_history (memory_id, accessed_at, query_hash)
             VALUES ('audit', '2026-07-26T00:00:00Z', 'query-a')",
            [],
        )
        .expect("insert access history");
    store
        .connection()
        .execute(
            "UPDATE access_history SET query_hash = 'query-b' WHERE memory_id = 'audit'",
            [],
        )
        .expect("update access history");
    store
        .connection()
        .execute("DELETE FROM access_history WHERE memory_id = 'audit'", [])
        .expect("delete access history");
    assert_eq!(
        store.search_generation().expect("after history mutations"),
        before_history + 3
    );
}

#[test]
fn known_previous_memory_update_trigger_migrates_to_all_column_coverage() {
    let store = MemoryStore::open_in_memory().expect("open store");
    store
        .connection()
        .execute_batch(
            r#"
            DROP TRIGGER memory_search_generation_after_update;
            CREATE TRIGGER memory_search_generation_after_update
            AFTER UPDATE OF path, summary, text, importance, timestamp, valid_from, valid_until, category, topic, keywords, entities, source, scope, archived, created_at, updated_at, revision, metadata, superseded_by, idless_identity, retention_policy, domain, tier ON memories
            BEGIN
                SELECT CASE
                    WHEN (SELECT COUNT(*) FROM memory_search_generation WHERE id = 1) != 1
                        THEN RAISE(ABORT, 'memory search generation row missing')
                    WHEN (SELECT generation FROM memory_search_generation WHERE id = 1) >= 9223372036854775807
                        THEN RAISE(ABORT, 'memory search generation exhausted')
                END;
                UPDATE memory_search_generation SET generation = generation + 1 WHERE id = 1;
            END;
            "#,
        )
        .expect("install previous trigger shape");

    super::super::search_generation::ensure_search_generation_schema(store.connection())
        .expect("known previous trigger must migrate");

    let sql: String = store
        .connection()
        .query_row(
            "SELECT sql FROM sqlite_master WHERE type = 'trigger' AND name = 'memory_search_generation_after_update'",
            [],
            |row| row.get(0),
        )
        .expect("migrated trigger");
    let normalized = sql
        .chars()
        .filter(|character| !character.is_whitespace())
        .flat_map(char::to_lowercase)
        .collect::<String>();
    assert!(normalized.contains("afterupdateonmemories"));
    assert!(!normalized.contains("updateof"));
}
