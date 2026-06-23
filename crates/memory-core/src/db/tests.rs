use super::{
    add_edge, archive_memory, delete, fetch_by_ids, gc_tables, get_all, get_edges,
    get_sandbox_policy, graph_expand, init_schema, list_by_path, list_sandbox_policies,
    list_wiki_duplicate_candidates, normalize_for_write, now_utc_iso, record_access,
    record_access_with_updates, register_sqlite_vec, release_event_claim, search_fts,
    search_symbolic_candidates, search_vec, serialize_f32, set_sandbox_policy, stats,
    supersede_memory, try_claim_event, try_load_sqlite_vec, update_agent_known_state,
    update_enrichment_fields, update_with_revision, upsert, vault_touch_entry, vault_upsert_entry,
    AccessUpdate,
};
use chrono::Utc;
use rusqlite::{params, Connection};
use serde_json::json;

use crate::types::{GcConfig, MemoryEdge, MemoryEntry};

fn make_conn() -> Connection {
    libsimple::enable_auto_extension().unwrap();
    register_sqlite_vec();
    let conn = Connection::open_in_memory().unwrap();
    init_schema(&conn).unwrap();
    try_load_sqlite_vec(&conn);
    conn
}

fn make_entry(id: &str, text: &str) -> MemoryEntry {
    MemoryEntry {
        id: id.into(),
        path: "/test".into(),
        summary: text[..text.len().min(30)].into(),
        text: text.into(),
        importance: 0.7,
        timestamp: Utc::now().to_rfc3339(),
        valid_from: String::new(),
        valid_until: None,
        category: "fact".into(),
        topic: "".into(),
        keywords: vec!["test".into()],
        persons: vec![],
        entities: vec![],
        location: "".into(),
        source: "".into(),
        scope: "general".into(),
        archived: false,
        access_count: 0,
        last_access: None,
        revision: 1,
        metadata: json!({ "keywords": ["test"], "entities": [] }),
        vector: None,
        retention_policy: None,
        domain: None,
        recall_count: 0,
        query_diversity: 0,
        tier: "raw".to_string(),
    }
}

#[test]
fn upsert_folds_persons_into_entities_without_persisting_persons_column() {
    let mut conn = make_conn();
    let mut e = make_entry("pers-1", "Kyle prefers concise handoffs");
    e.persons = vec!["Kyle".to_string()];
    e.entities = vec!["Sigil".to_string()];
    upsert(&mut conn, &e, false).unwrap();

    let has_persons_column: bool = conn
        .query_row(
            "SELECT 1 FROM pragma_table_info('memories') WHERE name='persons' LIMIT 1",
            [],
            |_| Ok(true),
        )
        .unwrap_or(false);
    assert!(!has_persons_column);

    let entities: String = conn
        .query_row("SELECT entities FROM memories WHERE id='pers-1'", [], |r| {
            r.get(0)
        })
        .unwrap();
    let ents: Vec<String> = serde_json::from_str(&entities).unwrap();
    assert!(ents.iter().any(|e| e == "Kyle"));
    assert!(ents.iter().any(|e| e == "user"));
    assert!(ents.iter().any(|e| e == "Sigil"));
}

#[test]
fn upsert_and_fts() {
    let mut conn = make_conn();
    let e = make_entry("abc", "Rust is a systems programming language");
    upsert(&mut conn, &e, false).unwrap();

    let results = search_fts(&conn, "systems programming", 5, false, false, None, None).unwrap();
    assert!(results.contains_key("abc"), "expected 'abc' in FTS results");
}

#[test]
fn search_fts_returns_row_decode_errors() {
    let conn = make_conn();
    let blob_id = [1u8, 2, 3, 4];
    conn.execute(
        "INSERT INTO memories(id, timestamp) VALUES (?1, ?2)",
        params![&blob_id[..], Utc::now().to_rfc3339()],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO memories_fts(id, path, summary, text, keywords, entities)
         VALUES (?1, '/test', 'needle', 'needle', 'needle', 'needle')",
        params![&blob_id[..]],
    )
    .unwrap();

    let err = search_fts(&conn, "needle", 5, false, false, None, None)
        .expect_err("row decode errors must propagate instead of being dropped");
    assert!(
        err.to_string().contains("Invalid column type")
            || err.to_string().contains("InvalidColumnType"),
        "unexpected error: {err}"
    );
}

#[test]
fn upsert_jaccard_dedup_returns_fts_row_decode_errors() {
    let mut conn = make_conn();
    let blob_id = [5u8, 6, 7, 8];
    conn.execute(
        "INSERT INTO memories(id, timestamp) VALUES (?1, ?2)",
        params![&blob_id[..], Utc::now().to_rfc3339()],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO memories_fts(id, path, summary, text, keywords, entities)
         VALUES (?1, '/test', 'needle overlap', 'needle overlap', 'needle', 'needle')",
        params![&blob_id[..]],
    )
    .unwrap();

    let entry = make_entry("dedup-bad-row", "needle overlap");
    let err = upsert(&mut conn, &entry, false)
        .expect_err("dedup FTS row decode errors must abort the write");
    assert!(
        err.to_string().contains("Invalid column type")
            || err.to_string().contains("InvalidColumnType"),
        "unexpected error: {err}"
    );

    let count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM memories WHERE id = 'dedup-bad-row'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(count, 0, "failed dedup query should roll back the upsert");
}

#[test]
fn jaccard_dedup_refreshes_candidate_fts() {
    let mut conn = make_conn();
    let text = "Rust memory systems need atomic full text search updates";
    let mut canonical = make_entry("canonical", text);
    canonical.keywords = vec!["oldtag".to_string()];
    upsert(&mut conn, &canonical, false).unwrap();

    let mut duplicate = make_entry("duplicate", text);
    duplicate.keywords = vec!["mergedtag".to_string()];
    upsert(&mut conn, &duplicate, false).unwrap();

    let superseded_by: Option<String> = conn
        .query_row(
            "SELECT superseded_by FROM memories WHERE id='duplicate'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(superseded_by.as_deref(), Some("canonical"));

    let results = search_fts(&conn, "mergedtag", 5, false, false, None, None).unwrap();
    assert!(
        results.contains_key("canonical"),
        "merged keyword should be searchable through the canonical row"
    );
}

#[test]
fn upsert_idempotent() {
    let mut conn = make_conn();
    let mut e = make_entry("dup", "first text");
    upsert(&mut conn, &e, false).unwrap();
    e.text = "updated text".into();
    upsert(&mut conn, &e, false).unwrap();

    let results = search_fts(&conn, "updated", 5, false, false, None, None).unwrap();
    assert!(results.contains_key("dup"));
}

#[test]
fn update_enrichment_fields_clears_stale_failure_metadata() {
    let mut conn = make_conn();
    let mut entry = make_entry(
        "enrich-reset",
        "entry with stale enrichment failure metadata",
    );
    entry.metadata = json!({
        "enrichment": {
            "status": "failed",
            "failed_stage": "embedding",
            "last_error": "Voyage 429",
            "last_failure_at": "2026-06-01T00:00:00Z"
        }
    });
    upsert(&mut conn, &entry, false).unwrap();

    let vec_blob = serialize_f32(&vec![0.1_f32; 1024]);
    update_enrichment_fields(
        &mut conn,
        "enrich-reset",
        None,
        Some(&vec_blob),
        None,
        None,
        1,
    )
    .unwrap();

    let metadata: String = conn
        .query_row(
            "SELECT metadata FROM memories WHERE id='enrich-reset'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    let metadata: serde_json::Value = serde_json::from_str(&metadata).unwrap();
    assert_eq!(metadata["enrichment"]["status"], "embedded");
    assert_eq!(
        metadata["enrichment"]["last_error"],
        serde_json::Value::Null
    );
    assert!(metadata["enrichment"].get("failed_stage").is_none());
    assert!(metadata["enrichment"].get("last_failure_at").is_none());
}

#[test]
fn upsert_defaults_valid_from_to_timestamp() {
    let mut conn = make_conn();
    let mut entry = make_entry("temporal-default", "temporal default memory");
    entry.timestamp = "2026-01-01T00:00:00Z".to_string();
    entry.valid_from = String::new();

    upsert(&mut conn, &entry, false).unwrap();

    let stored = fetch_by_ids(&conn, &["temporal-default".to_string()], false)
        .unwrap()
        .remove("temporal-default")
        .unwrap();
    assert_eq!(stored.valid_from, "2026-01-01T00:00:00.000Z");
    assert_eq!(stored.valid_until, None);
}

#[test]
fn init_schema_backfills_valid_from_for_legacy_rows() {
    libsimple::enable_auto_extension().unwrap();
    let conn = Connection::open_in_memory().unwrap();
    conn.execute_batch(
        r#"
        CREATE TABLE memories (
            id TEXT PRIMARY KEY,
            path TEXT NOT NULL DEFAULT '/',
            summary TEXT NOT NULL DEFAULT '',
            text TEXT NOT NULL DEFAULT '',
            importance REAL NOT NULL DEFAULT 0.7,
            timestamp TEXT NOT NULL,
            category TEXT NOT NULL DEFAULT 'fact',
            topic TEXT NOT NULL DEFAULT '',
            keywords TEXT NOT NULL DEFAULT '[]',
            entities TEXT NOT NULL DEFAULT '[]',
            location TEXT NOT NULL DEFAULT '',
            source TEXT NOT NULL DEFAULT 'manual',
            scope TEXT NOT NULL DEFAULT 'general',
            archived INTEGER NOT NULL DEFAULT 0,
            created_at TEXT NOT NULL DEFAULT '',
            updated_at TEXT NOT NULL DEFAULT '',
            access_count INTEGER NOT NULL DEFAULT 0,
            last_access TEXT,
            revision INTEGER NOT NULL DEFAULT 1,
            metadata TEXT NOT NULL DEFAULT '{}',
            retention_policy TEXT,
            domain TEXT,
            superseded_by TEXT
        );
        INSERT INTO memories
            (id, text, timestamp, keywords, entities, metadata)
        VALUES
            ('legacy-valid-from', 'legacy temporal row', '2026-02-03T04:05:06Z', '[]', '[]', '{}');
        "#,
    )
    .unwrap();

    init_schema(&conn).unwrap();

    let valid_from: String = conn
        .query_row(
            "SELECT valid_from FROM memories WHERE id = 'legacy-valid-from'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(valid_from, "2026-02-03T04:05:06.000Z");
}

#[test]
fn search_fts_respects_as_of_validity_window() {
    let mut conn = make_conn();

    let mut expired = make_entry("temporal-old", "TemporalNeedle old memory");
    expired.valid_from = "2026-01-01T00:00:00Z".to_string();
    expired.valid_until = Some("2026-02-01T00:00:00Z".to_string());
    upsert(&mut conn, &expired, false).unwrap();

    let mut active = make_entry("temporal-new", "TemporalNeedle new memory");
    active.valid_from = "2026-02-01T00:00:00Z".to_string();
    upsert(&mut conn, &active, false).unwrap();

    let january = search_fts(
        &conn,
        "TemporalNeedle",
        5,
        false,
        false,
        None,
        Some("2026-01-15T00:00:00.000Z"),
    )
    .unwrap();
    assert!(january.contains_key("temporal-old"));
    assert!(!january.contains_key("temporal-new"));

    let march = search_fts(
        &conn,
        "TemporalNeedle",
        5,
        false,
        false,
        None,
        Some("2026-03-01T00:00:00.000Z"),
    )
    .unwrap();
    assert!(!march.contains_key("temporal-old"));
    assert!(march.contains_key("temporal-new"));
}

#[test]
fn search_vec_respects_as_of_validity_window() {
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

    let mut old = make_entry("temporal-vec-old", "Temporal vector old memory");
    old.valid_from = "2026-01-01T00:00:00Z".to_string();
    old.valid_until = Some("2026-02-01T00:00:00Z".to_string());
    old.vector = Some(vec![0.1_f32; 1024]);
    upsert(&mut conn, &old, true).unwrap();

    let mut new = make_entry("temporal-vec-new", "Temporal vector new memory");
    new.valid_from = "2026-02-01T00:00:00Z".to_string();
    new.vector = Some(vec![0.1_f32; 1024]);
    upsert(&mut conn, &new, true).unwrap();

    let query = vec![0.1_f32; 1024];
    let january = search_vec(
        &conn,
        &query,
        5,
        false,
        false,
        None,
        Some("2026-01-15T00:00:00.000Z"),
    )
    .unwrap();
    assert!(january.contains_key("temporal-vec-old"));
    assert!(!january.contains_key("temporal-vec-new"));

    let march = search_vec(
        &conn,
        &query,
        5,
        false,
        false,
        None,
        Some("2026-03-01T00:00:00.000Z"),
    )
    .unwrap();
    assert!(!march.contains_key("temporal-vec-old"));
    assert!(march.contains_key("temporal-vec-new"));
}

#[test]
fn update_with_revision_detects_conflict() {
    let mut conn = make_conn();
    let e = make_entry("rev-1", "original");
    upsert(&mut conn, &e, false).unwrap();

    let metadata = serde_json::to_string(&json!({"source":"test"})).unwrap();
    let ok = update_with_revision(
        &mut conn,
        "rev-1",
        "merged",
        "merged",
        "consolidation",
        &metadata,
        None,
        1,
    )
    .unwrap();
    assert!(ok);

    let stale = update_with_revision(
        &mut conn,
        "rev-1",
        "stale",
        "stale",
        "consolidation",
        &metadata,
        None,
        1,
    )
    .unwrap();
    assert!(!stale);
}

#[test]
fn search_vec_knn_with_k_constraint() {
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

    let mut e = make_entry("vec-1", "vector memory entry");
    e.vector = Some(vec![0.1_f32; 1024]);
    upsert(&mut conn, &e, true).unwrap();

    let query = vec![0.1_f32; 1024];
    let results = search_vec(&conn, &query, 3, false, false, None, None).unwrap();
    assert!(results.contains_key("vec-1"));
}

#[test]
fn delete_existing() {
    let mut conn = make_conn();
    let e = make_entry("del-1", "to be deleted");
    upsert(&mut conn, &e, false).unwrap();

    let deleted = delete(&mut conn, "del-1", false).unwrap();
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
    let fts_results = search_fts(&conn, "deleted", 5, false, false, None, None).unwrap();
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

    let err = delete(&mut conn, "del-vec-error", true)
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
fn search_fts_respects_path_prefix() {
    let mut conn = make_conn();
    let mut project_entry = make_entry("proj-1", "systems programming with Rust");
    project_entry.path = "/project/rust".into();
    upsert(&mut conn, &project_entry, false).unwrap();

    let mut docs_entry = make_entry("docs-1", "systems programming with Rust");
    docs_entry.path = "/docs/rust".into();
    upsert(&mut conn, &docs_entry, false).unwrap();

    let results = search_fts(
        &conn,
        "systems programming",
        5,
        false,
        false,
        Some("/project"),
        None,
    )
    .unwrap();
    assert!(results.contains_key("proj-1"));
    assert!(!results.contains_key("docs-1"));
}

#[test]
fn search_symbolic_candidates_treats_like_wildcards_as_literals() {
    let mut conn = make_conn();
    let plain = make_entry("plain-wildcard", "ordinary symbolic candidate text");
    upsert(&mut conn, &plain, false).unwrap();

    let literal = make_entry("literal-wildcard", "literal ___ marker text");
    upsert(&mut conn, &literal, false).unwrap();

    let results = search_symbolic_candidates(&conn, "___", 10, false, false, None, None).unwrap();
    let ids = results
        .into_iter()
        .map(|entry| entry.id)
        .collect::<Vec<_>>();

    assert_eq!(
        ids,
        vec!["literal-wildcard"],
        "underscore wildcards must not broaden symbolic LIKE matches"
    );
}

#[test]
fn raw_search_channels_exclude_superseded_by_default() {
    let mut conn = make_conn();
    let old = make_entry("old", "TrendLock old rule");
    let new = make_entry("new", "TrendLock new rule");
    upsert(&mut conn, &old, false).unwrap();
    upsert(&mut conn, &new, false).unwrap();
    supersede_memory(&conn, "old", "new").unwrap();

    let results = search_fts(&conn, "TrendLock", 5, false, false, None, None).unwrap();
    assert!(results.contains_key("new"));
    assert!(!results.contains_key("old"));

    let with_superseded = search_fts(&conn, "TrendLock", 5, false, true, None, None).unwrap();
    assert!(with_superseded.contains_key("old"));
}

#[test]
fn delete_nonexistent() {
    let mut conn = make_conn();
    let deleted = delete(&mut conn, "nonexistent-id", false).unwrap();
    assert!(!deleted, "should return false for non-existent entry");
}

#[test]
fn stats_aggregation() {
    let mut conn = make_conn();

    let mut e1 = make_entry("s1", "fact entry");
    e1.scope = "general".into();
    e1.category = "fact".into();
    e1.path = "/project/alpha".into();
    upsert(&mut conn, &e1, false).unwrap();

    let mut e2 = make_entry("s2", "decision entry");
    e2.scope = "project".into();
    e2.category = "decision".into();
    e2.path = "/project/beta".into();
    upsert(&mut conn, &e2, false).unwrap();

    let mut e3 = make_entry("s3", "user preference");
    e3.scope = "user".into();
    e3.category = "preference".into();
    e3.path = "/user/settings".into();
    upsert(&mut conn, &e3, false).unwrap();

    let s = stats(&conn, false).unwrap();
    assert_eq!(s.total, 3);
    assert_eq!(s.by_scope.get("general"), Some(&1_u64));
    assert_eq!(s.by_scope.get("project"), Some(&1_u64));
    assert_eq!(s.by_scope.get("user"), Some(&1_u64));
    assert_eq!(s.by_category.get("fact"), Some(&1_u64));
    assert_eq!(s.by_category.get("decision"), Some(&1_u64));
    assert_eq!(s.by_root_path.get("/project"), Some(&2_u64));
    assert_eq!(s.by_root_path.get("/user"), Some(&1_u64));
}

#[test]
fn graph_add_and_get_edges() {
    let mut conn = make_conn();
    let e1 = make_entry("g1", "cause event");
    let e2 = make_entry("g2", "effect event");
    upsert(&mut conn, &e1, false).unwrap();
    upsert(&mut conn, &e2, false).unwrap();

    let edge = MemoryEdge {
        source_id: "g1".into(),
        target_id: "g2".into(),
        relation: "causes".into(),
        weight: 0.9,
        metadata: serde_json::json!({}),
        created_at: String::new(),
        valid_from: String::new(),
        valid_to: None,
    };
    add_edge(&conn, &edge).unwrap();

    let out = get_edges(&conn, "g1", "outgoing", None).unwrap();
    assert_eq!(out.len(), 1);
    assert_eq!(out[0].target_id, "g2");
    assert_eq!(out[0].relation, "causes");

    let inc = get_edges(&conn, "g2", "incoming", None).unwrap();
    assert_eq!(inc.len(), 1);
    assert_eq!(inc[0].source_id, "g1");
}

#[test]
fn graph_get_edges_returns_row_decode_errors() {
    let mut conn = make_conn();
    let e1 = make_entry("bad-edge-source", "source");
    let e2 = make_entry("bad-edge-target", "target");
    upsert(&mut conn, &e1, false).unwrap();
    upsert(&mut conn, &e2, false).unwrap();

    conn.execute(
        "INSERT INTO memory_edges
         (source_id, target_id, relation, weight, metadata, created_at, valid_from, valid_to)
         VALUES (?1, ?2, ?3, ?4, '{}', ?5, ?5, NULL)",
        rusqlite::params![
            "bad-edge-source",
            "bad-edge-target",
            "causes",
            "not-a-number",
            "2026-06-14T00:00:00Z"
        ],
    )
    .unwrap();

    let err = get_edges(&conn, "bad-edge-source", "outgoing", None).unwrap_err();
    let msg = err.to_string();
    assert!(
        msg.contains("Invalid column type") || msg.contains("not-a-number"),
        "expected row decode error, got: {msg}"
    );
}

#[test]
fn graph_expand_bfs() {
    let mut conn = make_conn();
    // Create chain: a -> b -> c
    for id in &["a", "b", "c", "d"] {
        upsert(&mut conn, &make_entry(id, &format!("node {}", id)), false).unwrap();
    }
    add_edge(
        &conn,
        &MemoryEdge {
            source_id: "a".into(),
            target_id: "b".into(),
            relation: "follows".into(),
            weight: 1.0,
            metadata: serde_json::json!({}),
            created_at: String::new(),
            valid_from: String::new(),
            valid_to: None,
        },
    )
    .unwrap();
    add_edge(
        &conn,
        &MemoryEdge {
            source_id: "b".into(),
            target_id: "c".into(),
            relation: "follows".into(),
            weight: 1.0,
            metadata: serde_json::json!({}),
            created_at: String::new(),
            valid_from: String::new(),
            valid_to: None,
        },
    )
    .unwrap();
    // d is disconnected

    // Expand 1 hop from "a"
    let r1 = graph_expand(&conn, &["a".into()], 1, None).unwrap();
    assert_eq!(r1.entries.len(), 1); // should find b
    assert!(r1.distances.contains_key("b"));
    assert!(!r1.distances.contains_key("c")); // c is 2 hops

    // Expand 2 hops from "a"
    let r2 = graph_expand(&conn, &["a".into()], 2, None).unwrap();
    assert_eq!(r2.entries.len(), 2); // b and c
    assert!(r2.distances.contains_key("c"));
    assert!(!r2.distances.contains_key("d")); // d is disconnected
}

#[test]
fn delete_cascades_edges() {
    let mut conn = make_conn();
    let e1 = make_entry("del-e1", "source");
    let e2 = make_entry("del-e2", "target");
    upsert(&mut conn, &e1, false).unwrap();
    upsert(&mut conn, &e2, false).unwrap();
    add_edge(
        &conn,
        &MemoryEdge {
            source_id: "del-e1".into(),
            target_id: "del-e2".into(),
            relation: "causes".into(),
            weight: 1.0,
            metadata: serde_json::json!({}),
            created_at: String::new(),
            valid_from: String::new(),
            valid_to: None,
        },
    )
    .unwrap();

    delete(&mut conn, "del-e1", false).unwrap();
    let edges = get_edges(&conn, "del-e2", "both", None).unwrap();
    assert!(edges.is_empty(), "edges should be cleaned up on delete");
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

    delete(&mut conn, "del-cascade", false).unwrap();

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
fn gc_tables_prunes_retention_and_orphans() {
    let mut conn = make_conn();
    let e = make_entry("gc-keep", "gc target");
    upsert(&mut conn, &e, false).unwrap();

    for _ in 0..300 {
        conn.execute(
            "INSERT INTO access_history (memory_id, accessed_at, query_hash) VALUES (?1, ?2, ?3)",
            params!["gc-keep", now_utc_iso(), ""],
        )
        .unwrap();
    }
    conn.execute(
        "INSERT INTO access_history (memory_id, accessed_at, query_hash) VALUES (?1, ?2, ?3)",
        params!["gc-orphan", now_utc_iso(), ""],
    )
    .unwrap();

    conn.execute(
            "INSERT INTO processed_events (event_hash, event_id, worker, created_at) VALUES (?1, ?2, ?3, ?4)",
            params!["ev-old", "id-old", "ingest", "2000-01-01T00:00:00.000Z"],
        )
        .unwrap();
    conn.execute(
            "INSERT INTO processed_events (event_hash, event_id, worker, created_at) VALUES (?1, ?2, ?3, ?4)",
            params!["ev-new", "id-new", "ingest", "2999-01-01T00:00:00.000Z"],
        )
        .unwrap();

    conn.execute(
            "INSERT INTO audit_log (timestamp, server_id, tool_name, args_hash, success, duration_ms, error_kind, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            params![
                "2000-01-01T00:00:00.000Z",
                "mcp:test",
                "tool_old",
                "",
                1,
                1,
                Option::<String>::None,
                "2000-01-01T00:00:00.000Z"
            ],
        )
        .unwrap();
    conn.execute(
            "INSERT INTO audit_log (timestamp, server_id, tool_name, args_hash, success, duration_ms, error_kind, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            params![
                "2999-01-01T00:00:00.000Z",
                "mcp:test",
                "tool_new",
                "",
                1,
                1,
                Option::<String>::None,
                "2999-01-01T00:00:00.000Z"
            ],
        )
        .unwrap();

    conn.execute(
            "INSERT INTO agent_known_state (agent_id, memory_id, revision, synced_at) VALUES (?1, ?2, ?3, ?4)",
            params!["agent-old", "gc-keep", 1, "2000-01-01T00:00:00.000Z"],
        )
        .unwrap();
    conn.execute(
            "INSERT INTO agent_known_state (agent_id, memory_id, revision, synced_at) VALUES (?1, ?2, ?3, ?4)",
            params!["agent-new", "gc-keep", 2, "2999-01-01T00:00:00.000Z"],
        )
        .unwrap();
    conn.execute(
            "INSERT INTO agent_known_state (agent_id, memory_id, revision, synced_at) VALUES (?1, ?2, ?3, ?4)",
            params!["agent-orphan", "gc-orphan", 1, "2999-01-01T00:00:00.000Z"],
        )
        .unwrap();

    let summary = gc_tables(&mut conn, &GcConfig::default()).unwrap();

    let kept_access: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM access_history WHERE memory_id = ?1",
            params!["gc-keep"],
            |row| row.get(0),
        )
        .unwrap();
    let orphan_access: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM access_history WHERE memory_id = ?1",
            params!["gc-orphan"],
            |row| row.get(0),
        )
        .unwrap();
    let processed_count: i64 = conn
        .query_row("SELECT COUNT(*) FROM processed_events", [], |row| {
            row.get(0)
        })
        .unwrap();
    let audit_count: i64 = conn
        .query_row("SELECT COUNT(*) FROM audit_log", [], |row| row.get(0))
        .unwrap();
    let known_state_count: i64 = conn
        .query_row("SELECT COUNT(*) FROM agent_known_state", [], |row| {
            row.get(0)
        })
        .unwrap();
    let orphan_known_state: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM agent_known_state WHERE memory_id = ?1",
            params!["gc-orphan"],
            |row| row.get(0),
        )
        .unwrap();

    assert_eq!(
        kept_access, 256,
        "access_history should retain latest 256 per memory"
    );
    assert_eq!(orphan_access, 0, "orphaned access rows should be removed");
    assert_eq!(
        processed_count, 1,
        "old processed_events row should be pruned"
    );
    assert_eq!(audit_count, 1, "old audit_log row should be pruned");
    assert_eq!(
        known_state_count, 1,
        "old + orphan known-state rows should be pruned"
    );
    assert_eq!(
        orphan_known_state, 0,
        "orphaned known-state rows should be removed"
    );

    assert!(summary["access_history_pruned"].as_u64().unwrap_or(0) > 0);
    assert!(summary["orphaned_agent_known_state"].as_u64().unwrap_or(0) > 0);
}

#[test]
fn gc_tables_reconciles_query_diversity_after_prune() {
    let mut conn = make_conn();
    let e = make_entry("gc-qd", "diversity target");
    upsert(&mut conn, &e, false).unwrap();

    for i in 0..5 {
        let hash = format!("hash-{i}");
        conn.execute(
            "INSERT INTO access_history (memory_id, accessed_at, query_hash) VALUES (?1, ?2, ?3)",
            params!["gc-qd", now_utc_iso(), hash],
        )
        .unwrap();
    }
    conn.execute(
        "UPDATE memories SET query_diversity = 99 WHERE id = ?1",
        params!["gc-qd"],
    )
    .unwrap();

    let cfg = GcConfig {
        access_history_keep_per_memory: 2,
        ..GcConfig::default()
    };
    gc_tables(&mut conn, &cfg).unwrap();

    let qd: i64 = conn
        .query_row(
            "SELECT query_diversity FROM memories WHERE id = ?1",
            params!["gc-qd"],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(
        qd, 2,
        "query_diversity should match distinct query hashes kept after GC"
    );
}

#[test]
fn sandbox_policy_crud_roundtrip() {
    let conn = make_conn();

    set_sandbox_policy(
        &conn,
        "mcp:alpha",
        "process",
        r#"["PATH"]"#,
        r#"["/safe/read"]"#,
        r#"["/safe/write"]"#,
        r#"["/work"]"#,
        10_000,
        30_000,
        2,
        true,
    )
    .unwrap();

    set_sandbox_policy(
        &conn,
        "mcp:beta",
        "container",
        r#"["HOME"]"#,
        r#"["/ro"]"#,
        r#"["/rw"]"#,
        r#"["/sandbox"]"#,
        20_000,
        40_000,
        1,
        false,
    )
    .unwrap();

    let alpha = get_sandbox_policy(&conn, "mcp:alpha").unwrap().unwrap();
    assert_eq!(alpha["capability_id"], "mcp:alpha");
    assert_eq!(alpha["runtime_type"], "process");
    assert_eq!(alpha["enabled"], true);
    assert_eq!(alpha["env_allowlist"], json!(["PATH"]));

    let enabled = list_sandbox_policies(&conn, true, 10).unwrap();
    assert_eq!(enabled.len(), 1);
    assert_eq!(enabled[0]["capability_id"], "mcp:alpha");

    let limited = list_sandbox_policies(&conn, false, 1).unwrap();
    assert_eq!(limited.len(), 1);
}

// ───────────────────────────────────────────────────────────────────────────
// Touch-algorithm regression tests
//
// These guard the contract that `record_access` is a read-side accounting
// bump and `vault_touch_entry` is race-truthful. See
// .tachi/runs/agent-prompts/tachi-shell-github-convoy-touchy-agent-prompt.md
// section 三 for the full reveal that motivated these.
// ───────────────────────────────────────────────────────────────────────────

#[test]
fn record_access_bumps_count_and_last_access_only() {
    let mut conn = make_conn();
    let e = make_entry("touch-1", "first text");
    upsert(&mut conn, &e, false).unwrap();

    let (rev0, upd0, ac0): (i64, String, i64) = conn
        .query_row(
            "SELECT revision, updated_at, access_count FROM memories WHERE id = ?1",
            params!["touch-1"],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .unwrap();
    assert_eq!(
        ac0, 0,
        "freshly upserted memory must start with access_count=0"
    );

    // Sleep ≥1s so any (incorrect) `updated_at` bump would be visibly newer.
    std::thread::sleep(std::time::Duration::from_millis(1100));
    record_access(&conn, &["touch-1".to_string()], &[], None).unwrap();

    let (rev1, upd1, ac1, la1): (i64, String, i64, Option<String>) = conn
        .query_row(
            "SELECT revision, updated_at, access_count, last_access \
             FROM memories WHERE id = ?1",
            params!["touch-1"],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )
        .unwrap();

    assert_eq!(ac1, 1, "record_access must increment access_count by 1");
    assert!(la1.is_some(), "record_access must set last_access");
    assert_eq!(
        rev1, rev0,
        "record_access must NOT bump revision (optimistic-concurrency invariant)"
    );
    assert_eq!(
        upd1, upd0,
        "record_access must NOT bump updated_at (downstream cache invariant)"
    );

    // access_history row recorded
    let ah_count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM access_history WHERE memory_id = ?1",
            params!["touch-1"],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(
        ah_count, 1,
        "record_access must insert exactly one access_history row"
    );
}

#[test]
fn record_access_with_updates_returns_post_write_access_fields() {
    let mut conn = make_conn();
    let mut e = make_entry("touch-return", "return updated access fields");
    e.access_count = 7;
    upsert(&mut conn, &e, false).unwrap();

    let updates = record_access_with_updates(&conn, &["touch-return".to_string()], &[], None)
        .expect("record access updates");
    let update = updates.get("touch-return").expect("updated row");
    let db_update: AccessUpdate = conn
        .query_row(
            "SELECT access_count, last_access FROM memories WHERE id = ?1",
            params!["touch-return"],
            |row| {
                Ok(AccessUpdate {
                    access_count: row.get(0)?,
                    last_access: row.get(1)?,
                })
            },
        )
        .unwrap();

    assert_eq!(update.access_count, 8);
    assert!(update.last_access.is_some());
    assert_eq!(update, &db_update);
}

#[test]
fn record_access_with_updates_ignores_missing_ids() {
    let mut conn = make_conn();
    let e = make_entry("touch-present", "present access row");
    upsert(&mut conn, &e, false).unwrap();

    let updates = record_access_with_updates(
        &conn,
        &["touch-present".to_string(), "touch-missing".to_string()],
        &[],
        None,
    )
    .expect("missing rows should not abort accounting");

    assert!(updates.contains_key("touch-present"));
    assert!(!updates.contains_key("touch-missing"));
    let missing_history: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM access_history WHERE memory_id = ?1",
            params!["touch-missing"],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(missing_history, 0);
}

#[test]
fn record_access_repeats_accumulate_on_access_history() {
    let mut conn = make_conn();
    let e = make_entry("touch-2", "repeat target");
    upsert(&mut conn, &e, false).unwrap();

    for _ in 0..3 {
        record_access(&conn, &["touch-2".to_string()], &[], None).unwrap();
    }

    let ac: i64 = conn
        .query_row(
            "SELECT access_count FROM memories WHERE id = ?1",
            params!["touch-2"],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(ac, 3);

    let ah: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM access_history WHERE memory_id = ?1",
            params!["touch-2"],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(
        ah, 3,
        "each record_access call should append to access_history"
    );
}

#[test]
fn vault_touch_entry_returns_post_touch_count() {
    use crate::vault::VaultEntry;

    let conn = make_conn();
    let entry = VaultEntry {
        name: "TOUCH_KEY".into(),
        encrypted_value: "AQID".into(),
        nonce: "BAUG".into(),
        secret_type: "api_key".into(),
        description: "regression".into(),
        allowed_agents: None,
        created_at: String::new(),
        updated_at: String::new(),
        accessed_at: String::new(),
        access_count: 0,
    };
    vault_upsert_entry(&conn, &entry).unwrap();

    let c1 = vault_touch_entry(&conn, "TOUCH_KEY").unwrap();
    assert_eq!(c1, 1, "first touch must report 1, not the pre-touch 0");

    let c2 = vault_touch_entry(&conn, "TOUCH_KEY").unwrap();
    assert_eq!(c2, 2, "second touch must report 2, race-truthful");

    let c3 = vault_touch_entry(&conn, "TOUCH_KEY").unwrap();
    assert_eq!(c3, 3);

    // DB row must agree with the last returned value.
    let db_count: i64 = conn
        .query_row(
            "SELECT access_count FROM vault_entries WHERE name = ?1",
            params!["TOUCH_KEY"],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(db_count, c3);
}

#[test]
fn normalize_for_write_clamps_fields() {
    let mut e = make_entry("norm-1", "normalize test");
    e.path = "project/alpha".into();
    e.source = "user".into();
    e.category = "FACT".into();
    e.scope = "PROJECT".into();
    e.importance = 1.5;
    normalize_for_write(&mut e);
    assert!(
        e.path.starts_with('/'),
        "path should be normalized to start with /"
    );
    assert_eq!(e.category, "fact", "category should be lowercase");
    assert_eq!(e.scope, "project", "scope should be lowercase");
    assert!(
        (e.importance - 1.0).abs() < f64::EPSILON,
        "importance should be clamped to 1.0"
    );
}

#[test]
fn normalize_for_write_empty_id_rejected_by_upsert() {
    let e = make_entry(" ", "empty id test");
    let err = upsert(&mut make_conn(), &e, false);
    assert!(err.is_err(), "empty id should be rejected");
}

#[test]
fn fetch_by_ids_returns_entries() {
    let mut conn = make_conn();
    upsert(&mut conn, &make_entry("fid-1", "alpha memory"), false).unwrap();
    upsert(&mut conn, &make_entry("fid-2", "beta memory"), false).unwrap();
    upsert(&mut conn, &make_entry("fid-3", "gamma memory"), false).unwrap();

    let result = fetch_by_ids(&conn, &["fid-1".into(), "fid-3".into()], false).unwrap();
    assert_eq!(result.len(), 2);
    assert!(result.contains_key("fid-1"));
    assert!(result.contains_key("fid-3"));
    assert!(!result.contains_key("fid-2"));
}

#[test]
fn fetch_by_ids_hydrates_vectors_in_main_query() {
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

    let mut entry = make_entry("fid-vec", "vector memory");
    entry.vector = Some(vec![0.25_f32; 1024]);
    upsert(&mut conn, &entry, true).unwrap();

    let result = fetch_by_ids(&conn, &["fid-vec".into()], false).unwrap();
    let fetched = result.get("fid-vec").expect("entry should be fetched");
    assert_eq!(
        fetched.vector.as_ref().map(Vec::len),
        Some(1024),
        "fetch_by_ids should hydrate the vector from the joined row"
    );
}

#[test]
fn fetch_by_ids_returns_vector_decode_errors() {
    let mut conn = make_conn();
    upsert(
        &mut conn,
        &make_entry("fid-bad-vec", "bad vector memory"),
        false,
    )
    .unwrap();
    conn.execute("DROP TABLE IF EXISTS memories_vec", [])
        .unwrap();
    conn.execute(
        "CREATE TABLE memories_vec(id TEXT PRIMARY KEY, embedding BLOB)",
        [],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO memories_vec(id, embedding) VALUES (?1, ?2)",
        params!["fid-bad-vec", vec![1_u8, 2, 3]],
    )
    .unwrap();

    let err = fetch_by_ids(&conn, &["fid-bad-vec".into()], false)
        .expect_err("invalid vector blob length should not be silently ignored");
    assert!(
        err.to_string().contains("invalid vector blob length"),
        "{err}"
    );
}

#[test]
fn fetch_by_ids_empty_input() {
    let conn = make_conn();
    let result = fetch_by_ids(&conn, &[], false).unwrap();
    assert!(result.is_empty());
}

#[test]
fn get_all_returns_ordered_by_timestamp() {
    let mut conn = make_conn();
    upsert(&mut conn, &make_entry("ga-1", "first memory"), false).unwrap();
    upsert(&mut conn, &make_entry("ga-2", "second memory"), false).unwrap();

    let all = get_all(&conn, 10, false).unwrap();
    assert_eq!(all.len(), 2);
}

#[test]
fn get_all_respects_limit() {
    let mut conn = make_conn();
    for i in 0..5 {
        upsert(
            &mut conn,
            &make_entry(&format!("lim-{i}"), &format!("entry {i}")),
            false,
        )
        .unwrap();
    }
    let limited = get_all(&conn, 2, false).unwrap();
    assert_eq!(limited.len(), 2);
}

#[test]
fn list_by_path_filters_prefix() {
    let mut conn = make_conn();
    let mut e1 = make_entry("lp-1", "under project");
    e1.path = "/project/alpha".into();
    upsert(&mut conn, &e1, false).unwrap();

    let mut e2 = make_entry("lp-2", "under docs");
    e2.path = "/docs/beta".into();
    upsert(&mut conn, &e2, false).unwrap();

    let project_entries = list_by_path(&conn, "/project", 10, false).unwrap();
    assert_eq!(project_entries.len(), 1);
    assert_eq!(project_entries[0].id, "lp-1");
}

#[test]
fn list_by_path_empty_prefix_returns_all() {
    let mut conn = make_conn();
    upsert(&mut conn, &make_entry("lbe-1", "any"), false).unwrap();
    upsert(&mut conn, &make_entry("lbe-2", "any other"), false).unwrap();
    let all = list_by_path(&conn, "/", 10, false).unwrap();
    assert_eq!(all.len(), 2);
}

#[test]
fn list_wiki_duplicate_candidates_pushes_path_topic_and_parent_filter_to_sql() {
    let mut conn = make_conn();
    let mut same_path = make_entry("wiki-same-path", "same path");
    same_path.path = "/wiki/engineering/mcp".to_string();
    same_path.topic = "mcp".to_string();
    upsert(&mut conn, &same_path, false).unwrap();

    let mut same_topic_elsewhere = make_entry("wiki-same-topic", "same topic");
    same_topic_elsewhere.path = "/wiki/ops/mcp".to_string();
    same_topic_elsewhere.topic = "mcp".to_string();
    upsert(&mut conn, &same_topic_elsewhere, false).unwrap();

    let mut parent_sibling = make_entry("wiki-parent-sibling", "sibling text candidate");
    parent_sibling.path = "/wiki/engineering/other".to_string();
    parent_sibling.topic = "other".to_string();
    upsert(&mut conn, &parent_sibling, false).unwrap();

    let mut unrelated = make_entry("wiki-unrelated", "unrelated text");
    unrelated.path = "/wiki/product/roadmap".to_string();
    unrelated.topic = "roadmap".to_string();
    upsert(&mut conn, &unrelated, false).unwrap();

    let candidates = list_wiki_duplicate_candidates(
        &conn,
        "/wiki/engineering/mcp",
        "mcp",
        "/wiki/engineering",
        10,
    )
    .unwrap();
    let ids = candidates
        .into_iter()
        .map(|entry| entry.id)
        .collect::<std::collections::HashSet<_>>();

    assert!(ids.contains("wiki-same-path"));
    assert!(ids.contains("wiki-same-topic"));
    assert!(ids.contains("wiki-parent-sibling"));
    assert!(!ids.contains("wiki-unrelated"));
}

#[test]
fn archive_memory_marks_archived() {
    let mut conn = make_conn();
    upsert(&mut conn, &make_entry("arch-1", "to be archived"), false).unwrap();

    let ok = archive_memory(&conn, "arch-1").unwrap();
    assert!(ok, "archive should return true for existing entry");

    let archived_flag: bool = conn
        .query_row(
            "SELECT archived FROM memories WHERE id = 'arch-1'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert!(archived_flag, "entry should be archived");

    let not_found = archive_memory(&conn, "nonexistent").unwrap();
    assert!(!not_found, "archive should return false for nonexistent");
}

#[test]
fn archive_excluded_from_default_fetch() {
    let mut conn = make_conn();
    upsert(
        &mut conn,
        &make_entry("arch-fetch-1", "visible before archive"),
        false,
    )
    .unwrap();
    archive_memory(&conn, "arch-fetch-1").unwrap();

    let active = get_all(&conn, 10, false).unwrap();
    assert!(
        !active.iter().any(|e| e.id == "arch-fetch-1"),
        "archived entry should not appear in default get_all"
    );

    let with_archived = get_all(&conn, 10, true).unwrap();
    assert!(
        with_archived.iter().any(|e| e.id == "arch-fetch-1"),
        "archived entry should appear when include_archived=true"
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

// ── Tier lifecycle tests ───────────────────────────────────────────────────────

#[test]
fn record_access_increments_recall_count_for_fts_hits() {
    let mut conn = make_conn();
    let e = make_entry("tier-rc-1", "tier recall count test memory entry");
    upsert(&mut conn, &e, false).unwrap();

    // First call: id is in fts_hits → recall_count should be 1
    record_access(
        &conn,
        &["tier-rc-1".to_string()],
        &["tier-rc-1".to_string()],
        Some("query-a"),
    )
    .unwrap();

    let (rc, qd): (i64, i64) = conn
        .query_row(
            "SELECT recall_count, query_diversity FROM memories WHERE id = ?1",
            rusqlite::params!["tier-rc-1"],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert_eq!(rc, 1, "recall_count should be 1 after one FTS hit");
    assert_eq!(
        qd, 1,
        "query_diversity should be 1 after one distinct query"
    );

    // Second call with different query and no FTS hit → recall_count stays 1, diversity goes to 2
    record_access(&conn, &["tier-rc-1".to_string()], &[], Some("query-b")).unwrap();

    let (rc2, qd2): (i64, i64) = conn
        .query_row(
            "SELECT recall_count, query_diversity FROM memories WHERE id = ?1",
            rusqlite::params!["tier-rc-1"],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert_eq!(
        rc2, 1,
        "recall_count should still be 1 (no FTS hit in second call)"
    );
    assert_eq!(
        qd2, 2,
        "query_diversity should be 2 after two distinct queries"
    );
}

#[test]
fn record_access_promotion_gate_raw_to_consolidated() {
    let mut conn = make_conn();
    let e = make_entry(
        "tier-promo-1",
        "promotion gate test memory entry for tier lifecycle",
    );
    upsert(&mut conn, &e, false).unwrap();

    // Need: recall_count >= 3 AND query_diversity >= 3
    // Call 3 times with distinct queries and id in fts_hits each time
    let id = "tier-promo-1".to_string();
    record_access(
        &conn,
        std::slice::from_ref(&id),
        std::slice::from_ref(&id),
        Some("q-alpha"),
    )
    .unwrap();
    record_access(
        &conn,
        std::slice::from_ref(&id),
        std::slice::from_ref(&id),
        Some("q-beta"),
    )
    .unwrap();
    record_access(
        &conn,
        std::slice::from_ref(&id),
        std::slice::from_ref(&id),
        Some("q-gamma"),
    )
    .unwrap();

    let tier: String = conn
        .query_row(
            "SELECT tier FROM memories WHERE id = ?1",
            rusqlite::params!["tier-promo-1"],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(
        tier, "consolidated",
        "tier should be promoted to consolidated after 3 FTS recalls with 3 distinct queries"
    );
}

#[test]
fn record_access_no_promotion_without_diversity() {
    let mut conn = make_conn();
    let e = make_entry(
        "tier-no-promo",
        "no promotion without diversity memory entry",
    );
    upsert(&mut conn, &e, false).unwrap();

    let id = "tier-no-promo".to_string();
    // Same query hash every time → diversity stays 1
    record_access(
        &conn,
        std::slice::from_ref(&id),
        std::slice::from_ref(&id),
        Some("same-query"),
    )
    .unwrap();
    record_access(
        &conn,
        std::slice::from_ref(&id),
        std::slice::from_ref(&id),
        Some("same-query"),
    )
    .unwrap();
    record_access(
        &conn,
        std::slice::from_ref(&id),
        std::slice::from_ref(&id),
        Some("same-query"),
    )
    .unwrap();

    let (tier, rc, qd): (String, i64, i64) = conn
        .query_row(
            "SELECT tier, recall_count, query_diversity FROM memories WHERE id = ?1",
            rusqlite::params!["tier-no-promo"],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .unwrap();
    assert_eq!(
        tier, "raw",
        "tier should NOT be promoted with low query diversity"
    );
    assert_eq!(rc, 3, "recall_count should be 3");
    assert_eq!(qd, 1, "query_diversity should be 1 (all same query hash)");
}

#[test]
fn tier_based_decay_pattern_decays_slower_than_raw() {
    use crate::scorer::decay_score;

    // Create two identical entries except for tier and timestamp (1 day ago)
    let yesterday = (chrono::Utc::now() - chrono::Duration::days(1)).to_rfc3339();
    let raw_entry = MemoryEntry {
        id: "decay-raw".into(),
        tier: "raw".into(),
        last_access: Some(yesterday.clone()),
        access_count: 0,
        importance: 0.7,
        timestamp: yesterday.clone(),
        ..make_entry("decay-raw", "decay test entry")
    };
    let pattern_entry = MemoryEntry {
        id: "decay-pattern".into(),
        tier: "pattern".into(),
        last_access: Some(yesterday.clone()),
        access_count: 0,
        importance: 0.7,
        timestamp: yesterday.clone(),
        ..make_entry("decay-pattern", "decay test entry")
    };
    let consolidated_entry = MemoryEntry {
        id: "decay-cons".into(),
        tier: "consolidated".into(),
        last_access: Some(yesterday.clone()),
        access_count: 0,
        importance: 0.7,
        timestamp: yesterday.clone(),
        ..make_entry("decay-cons", "decay test entry")
    };

    let raw_score = decay_score(&raw_entry);
    let consolidated_score = decay_score(&consolidated_entry);
    let pattern_score = decay_score(&pattern_entry);

    // Pattern tier half-life is 30000 days → barely any decay after 1 day
    // Raw tier half-life is ~30 days → more decay after 1 day
    assert!(
        pattern_score > consolidated_score,
        "pattern ({pattern_score:.4}) should decay slower than consolidated ({consolidated_score:.4})"
    );
    assert!(
        consolidated_score > raw_score,
        "consolidated ({consolidated_score:.4}) should decay slower than raw ({raw_score:.4})"
    );
}
