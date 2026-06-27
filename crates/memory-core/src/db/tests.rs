use super::{
    add_edge, archive_memory, delete, fetch_by_ids, gc_tables, get_all, get_edges,
    get_sandbox_policy, graph_expand, init_schema, insert_tachi_event, list_by_path,
    list_sandbox_policies, list_tachi_events, list_wiki_duplicate_candidates, normalize_for_write,
    now_utc_iso, record_access, record_access_with_updates, register_sqlite_vec,
    release_event_claim, search_fts, search_symbolic_candidates, search_vec, serialize_f32,
    set_sandbox_policy, stats, supersede_memory, try_claim_event, try_load_sqlite_vec,
    update_agent_known_state, update_enrichment_fields, update_with_revision, upsert,
    vault_touch_entry, vault_upsert_entry, AccessUpdate,
};
use chrono::Utc;
use rusqlite::{params, Connection};
use serde_json::json;

use crate::types::{
    AuthorityLevel, EffectScope, GcConfig, MemoryEdge, MemoryEntry, ProjectionKind,
    TachiEventQuery, TachiEventRecord,
};

mod access;
mod events;
mod gc;
mod graph;
mod read_ops;
mod tier;

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
