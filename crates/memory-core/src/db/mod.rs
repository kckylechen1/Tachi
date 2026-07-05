mod agent_state;
mod audit;
mod common;
mod daily_pipeline;
mod doctor_probe;
mod domain;
mod event_ledger;
pub mod foundry_config;
pub mod foundry_jobs;
mod gc_candidates;
mod graph;
mod hub_db;
mod memory_crud;
pub mod migrations;
mod open;
mod pack_db;
mod recall_cache;
mod sandbox;
mod schema;
mod sqlite_vec;
mod state;
mod stats_gc;
mod vault_db;
mod virtual_capability;

pub use agent_state::{get_agent_known_revisions, update_agent_known_state};
pub use audit::{audit_log_insert, audit_log_list};
pub(crate) use common::normalize_utc_iso;
pub use common::{normalize_utc_iso_or_now, row_to_entry};
pub use daily_pipeline::{
    collect_daily_health_snapshot, count_active_memories, count_consolidated_active_memories,
    count_distinct_access_days, list_eval_evidence, list_memory_ids_needing_embedding,
    list_promotion_candidate_ids, promote_memory_to_durable, truth_maintenance_prune_stale,
    truth_maintenance_self_heal_promote_raw, CategorySourceGroup, DailyHealthDbSnapshot,
    DuplicateSummaryRow, EvalEvidenceRow,
};
pub use doctor_probe::{
    checkpoint_wal_truncate, count_chunks_rows, count_memories_missing_domain, count_memories_rows,
    count_memories_vec_rows, foundry_job_status_counts, open_for_wal_checkpoint,
    open_immutable_readonly, open_raw, schema_version, table_exists, FoundryJobStatusCounts,
};
pub use domain::{delete_domain, get_domain, list_domains, register_domain};
pub use event_ledger::{continuity_metrics, insert_tachi_event, list_tachi_events};
pub use gc_candidates::{
    list_memories_by_category_and_path_prefix, list_memories_by_path_prefix,
    CategoryPathPrefixMemoryRow, PathPrefixMemoryRow,
};
pub use graph::{
    add_edge, avg_importance, count_same_topic, get_contradiction_count, get_edges,
    get_superseded_ids, graph_expand, remove_edge,
};
pub use hub_db::{
    hub_get, hub_get_active_version_route, hub_list, hub_record_call_outcome, hub_record_feedback,
    hub_search, hub_set_active_version_route, hub_set_enabled, hub_set_review, hub_upsert,
};
#[cfg(test)]
pub(crate) use memory_crud::record_access;
pub(crate) use memory_crud::record_access_with_updates;
pub(crate) use memory_crud::search_fts_raw_match;
#[cfg(test)]
pub(crate) use memory_crud::AccessUpdate;
pub(crate) use memory_crud::MEMORY_SELECT_COLUMNS;
pub use memory_crud::{
    archive_memory, delete, fetch_by_ids, find_active_wiki_entry_by_path_or_topic,
    get_access_times, get_all, list_by_path, list_wiki_duplicate_candidates, normalize_for_write,
    record_enrichment_failure, release_event_claim, search_fts, search_symbolic_candidates,
    search_vec, supersede_memory, try_claim_event, update_enrichment_fields, update_with_revision,
    upsert,
};
pub(crate) use open::{
    acquire_startup_lock, configure_connection, open_read_only, open_read_write,
    retry_memory_locked, sqlite_error_is_locked,
};
pub use pack_db::{
    pack_delete, pack_get, pack_list, pack_upsert, projection_list, projection_upsert,
};
pub use recall_cache::{
    recall_cache_get, recall_cache_purge_stale, recall_cache_put, recall_cache_record_hit,
    recall_cache_stats, RecallCacheHit, RecallCacheStats,
};
pub use sandbox::{
    check_sandbox_access, get_sandbox_policy, insert_sandbox_exec_audit, list_sandbox_exec_audit,
    list_sandbox_policies, set_sandbox_policy, set_sandbox_rule,
};
pub use schema::{init_schema, init_schema_with_label_mut};
pub use sqlite_vec::{register_sqlite_vec, serialize_f32, try_load_sqlite_vec};
pub use state::{
    get_state, insert_state_if_absent, list_derived_by_source, list_state, save_derived,
    save_derived_with_id, set_state, set_state_if_version, StateRow,
};
pub use stats_gc::{archive_stale_memories, gc_tables, stats};
pub use vault_db::{
    vault_count_entries, vault_delete_entry, vault_entry_exists, vault_get_config, vault_get_entry,
    vault_get_key_health, vault_get_rotation, vault_insert_audit, vault_list_entries,
    vault_list_entries_by_type, vault_list_key_health, vault_list_rotations, vault_set_config,
    vault_set_rotation, vault_touch_entry, vault_upsert_entry, vault_upsert_key_health,
};
pub use virtual_capability::{vc_list_bindings, vc_upsert_binding};

#[cfg(test)]
pub(crate) use common::now_utc_iso;

#[cfg(test)]
mod tests;
