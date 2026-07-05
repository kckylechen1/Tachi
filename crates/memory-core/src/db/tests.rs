use super::{
    add_edge, archive_memory, checkpoint_wal_truncate, collect_daily_health_snapshot,
    count_chunks_rows, count_distinct_access_days, count_memories_missing_domain,
    count_memories_rows, count_memories_vec_rows, delete, fetch_by_ids, foundry_job_status_counts,
    gc_tables, get_all, get_edges, get_sandbox_policy, graph_expand, init_schema,
    insert_tachi_event, list_by_path, list_eval_evidence,
    list_memories_by_category_and_path_prefix, list_memories_by_path_prefix, list_sandbox_policies,
    list_tachi_events, list_wiki_duplicate_candidates, normalize_for_write, now_utc_iso,
    open_for_wal_checkpoint, open_immutable_readonly, open_raw, promote_memory_to_durable,
    record_access, record_access_with_updates, register_sqlite_vec, release_event_claim,
    schema_version, search_fts, search_symbolic_candidates, search_vec, serialize_f32,
    set_sandbox_policy, stats, supersede_memory, table_exists, truth_maintenance_prune_stale,
    try_claim_event, try_load_sqlite_vec, update_agent_known_state, update_enrichment_fields,
    update_with_revision, upsert, vault_touch_entry, vault_upsert_entry, AccessUpdate,
    FoundryJobStatusCounts,
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
