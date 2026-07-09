use super::{
    add_edge, archive_memory, checkpoint_wal_truncate, collect_daily_health_snapshot,
    count_chunks_rows, count_distinct_access_days, count_memories_missing_domain,
    count_memories_rows, count_memories_vec_rows, delete, fetch_by_ids, foundry_job_status_counts,
    gc_tables, get_all, get_edges, get_sandbox_policy, graph_expand, init_schema,
    insert_tachi_event, list_by_path, list_eval_evidence,
    list_memories_by_category_and_path_prefix, list_memories_by_path_prefix, list_sandbox_policies,
    list_tachi_events, list_wiki_duplicate_candidates, normalize_for_write, now_utc_iso,
    open_for_wal_checkpoint, open_immutable_readonly, open_raw, promote_memory_to_durable,
    record_access, record_access_with_updates, record_enrichment_failure, register_sqlite_vec,
    release_event_claim, schema_version, search_fts, search_symbolic_candidates, search_vec,
    serialize_f32, set_sandbox_policy, stats, supersede_memory, table_exists, try_claim_event,
    try_load_sqlite_vec, update_agent_known_state, update_enrichment_fields, update_with_revision,
    upsert, AccessUpdate, FoundryJobStatusCounts,
};
#[cfg(feature = "admin")]
use super::{vault_touch_entry, vault_upsert_entry};
use chrono::Utc;
use rusqlite::{params, Connection};
use serde_json::json;

use crate::types::{
    AuthorityLevel, EffectScope, GcConfig, MemoryEdge, MemoryEntry, ProjectionKind,
    TachiEventQuery, TachiEventRecord,
};

mod access;
mod daily_pipeline_ops;
mod delete_ops;
mod doctor_probe_ops;
mod events;
mod gc;
mod gc_candidates_ops;
mod graph;
mod read_ops;
mod sandbox_ops;
mod search_ops;
mod stats_ops;
mod tier;
mod write_ops;

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

// ── Bundled-SQLite security floor (#833) ─────────────────────────────────────
//
// Asserts bundled SQLite >= 3.50.3, the security floor. Below this, bundled
// SQLite carries known exploitable CVEs that directly hit this codebase:
//   - CVE-2025-7709 (fixed 3.50.3): corrupt FTS5 index -> unauthorized memory
//     access. memory-core's core search is FTS5, and DB content is user-writable
//     (memories, wiki, events, URL ingest) — this is the exact attack surface.
//   - CVE-2025-6965 (fixed 3.50.2): aggregate term count overflow.
//   - CVE-2025-29087 / -3277 (fixed 3.49.1): concat_ws() integer overflow.
//   - CVE-2025-29088 (fixed 3.49.1): lookaside allocator DoS (separate from concat).
// If this test fails, bundled SQLite dropped below the floor — bump
// rusqlite/libsqlite3-sys to restore >= 3.50.3. See Cargo.toml decision comment.

#[cfg(test)]
mod sqlite_security_floor {
    use rusqlite::Connection;

    /// Security floor: bundled SQLite must be >= 3.50.3.
    /// CVE-2025-7709 (FTS5 memory access, fixed 3.50.3) directly threatens
    /// this codebase's user-writable FTS5 search index.
    #[test]
    fn bundled_sqlite_meets_security_floor_3_50_3() {
        let conn = Connection::open_in_memory().expect("open in-memory db");
        let version: String = conn
            .query_row("SELECT sqlite_version()", [], |r| r.get(0))
            .expect("query sqlite_version");
        let parts: Vec<u32> = version.split('.').map(|p| p.parse().unwrap_or(0)).collect();
        let (major, minor, patch) = (
            *parts.first().unwrap_or(&0),
            *parts.get(1).unwrap_or(&0),
            *parts.get(2).unwrap_or(&0),
        );
        assert!(
            (major, minor, patch) >= (3, 50, 3),
            "bundled SQLite must be >= 3.50.3 (security floor for CVE-2025-7709); got {version}. \
             Bump rusqlite/libsqlite3-sys — see Cargo.toml #833 comment."
        );
    }

    /// FTS5 must be compiled in (schema depends on it).
    #[test]
    fn bundled_sqlite_has_fts5() {
        let conn = Connection::open_in_memory().expect("open in-memory db");
        conn.execute_batch("CREATE VIRTUAL TABLE fts5_check USING fts5(content);")
            .expect("FTS5 must be compiled in — schema depends on it");
        conn.execute("INSERT INTO fts5_check VALUES ('hello world')", [])
            .expect("insert into fts5");
        let matched: i64 = conn
            .query_row(
                "SELECT count(*) FROM fts5_check WHERE fts5_check MATCH 'hello'",
                [],
                |r| r.get(0),
            )
            .expect("fts5 match query");
        assert_eq!(matched, 1, "FTS5 match should find the inserted row");
    }

    /// JSON1 functions (json_extract etc.) are used throughout the DB layer.
    /// Built into SQLite >= 3.38.
    #[test]
    fn bundled_sqlite_has_json1() {
        let conn = Connection::open_in_memory().expect("open in-memory db");
        let val: i64 = conn
            .query_row(r#"SELECT json_extract('{"k": 42}', '$.k')"#, [], |r| {
                r.get(0)
            })
            .expect("json_extract must work — DB layer depends on JSON1");
        assert_eq!(val, 42);
    }
}
