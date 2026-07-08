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
    upsert, vault_touch_entry, vault_upsert_entry, AccessUpdate, FoundryJobStatusCounts,
};
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

// ── Bundled-SQLite feature assertion (#833) ──────────────────────────────────
//
// Locks the decision to keep `rusqlite` with `bundled`. If someone removes the
// `bundled` feature or the system SQLite lacks FTS5/JSON1/at least version
// 3.50.0, this test fails fast instead of producing silent migration drift or
// runtime panics. See the decision comment in the root `Cargo.toml`.

#[cfg(test)]
mod sqlite_features_asserted {
    use rusqlite::Connection;

    /// Assert the linked SQLite reports version >= 3.46.0 (the version
    /// `libsqlite3-sys` 0.30.1 bundles as of the locked `Cargo.lock`). With the
    /// `bundled` feature this is deterministic; without it the system version
    /// floats (Ubuntu 24.04 ships 3.45) and migrations can regress.
    #[test]
    fn bundled_sqlite_version_is_at_least_3_46() {
        let conn = Connection::open_in_memory().expect("open in-memory db");
        let version: String = conn
            .query_row("SELECT sqlite_version()", [], |r| r.get(0))
            .expect("query sqlite_version");
        let major: u32 = version
            .split('.')
            .next()
            .and_then(|s| s.parse().ok())
            .unwrap_or(0);
        assert!(
            major >= 3,
            "expected SQLite major version >= 3, got {version}"
        );
        let minor: u32 = version
            .split('.')
            .nth(1)
            .and_then(|s| s.parse().ok())
            .unwrap_or(0);
        assert!(
            (major, minor) >= (3, 46),
            "bundled SQLite must be >= 3.46.0 (libsqlite3-sys 0.30.1 bundles 3.46.0); got {version}. \
             This usually means the `bundled` feature was removed — see Cargo.toml #833 comment."
        );
    }

    /// FTS5 is required by the `memories_fts` virtual table in the schema DDL.
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

    /// JSON1 functions (json_extract etc.) are used throughout the DB layer
    /// (metadata columns, event ledger). Built into SQLite >= 3.38.
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
