mod agent_state;
pub mod anchor;
mod audit;
mod common;
mod daily_pipeline;
#[cfg(feature = "admin")]
pub mod dispatch_outcomes;
mod doctor_probe;
mod event_ledger;
#[cfg(feature = "admin")]
pub mod exec_env;
#[cfg(feature = "admin")]
pub mod foundry_config;
#[cfg(feature = "admin")]
pub mod foundry_jobs;
mod gc_candidates;
mod graph;
#[cfg(feature = "admin")]
mod hub_db;
mod memory_crud;
pub mod migrations;
mod open;
mod recall_cache;
mod sandbox;
mod schema;
#[cfg(feature = "admin")]
pub mod session_claims;
mod sqlite_vec;
mod state;
mod stats_gc;
#[cfg(feature = "admin")]
pub mod terminal_inbox;
#[cfg(feature = "admin")]
mod vault_db;
#[cfg(feature = "admin")]
mod virtual_capability;

pub use agent_state::{get_agent_known_revisions, update_agent_known_state};
pub use anchor::{anchor_id, anchor_path, ensure_anchor, AnchorKind};
pub use audit::{audit_log_insert, audit_log_list};
pub(crate) use common::normalize_utc_iso;
pub use common::{normalize_utc_iso_or_now, row_to_entry};
pub use daily_pipeline::{
    collect_daily_health_snapshot, count_active_memories, count_consolidated_active_memories,
    count_distinct_access_days, list_eval_evidence, list_memory_ids_needing_embedding,
    list_promotion_candidate_ids, promote_memory_to_durable, CategorySourceGroup,
    DailyHealthDbSnapshot, DuplicateSummaryRow, EvalEvidenceRow,
};
#[cfg(feature = "admin")]
pub use dispatch_outcomes::{
    derive_idempotency_key, find_outcome_by_dispatch_id, get_outcome, list_outcomes_by_issue_ref,
    list_outcomes_by_vendor_window, outcome_exists_for_dispatch, upsert_outcome,
    upsert_outcome_reconciling_terminal_placeholder, DispatchOutcomeRow, NewDispatchOutcome,
};
pub use doctor_probe::{
    checkpoint_wal_truncate, count_chunks_rows, count_memories_missing_domain, count_memories_rows,
    count_memories_vec_rows, foundry_job_status_counts, open_for_wal_checkpoint,
    open_immutable_readonly, open_raw, schema_version, table_exists, FoundryJobStatusCounts,
};
pub use event_ledger::{continuity_metrics, insert_tachi_event, list_tachi_events};
pub use gc_candidates::{
    list_memories_by_category_and_path_prefix, list_memories_by_path_prefix,
    CategoryPathPrefixMemoryRow, PathPrefixMemoryRow,
};
pub use graph::{
    add_component_governance_edge, add_component_governance_edge_with_provenance, add_edge,
    add_edge_with_provenance, avg_importance, close_related_to_fog, count_active_observations,
    count_same_topic, get_contradiction_count, get_edges, get_superseded_ids, graph_expand,
    invalidate_observation, list_observations_for_edge, remove_edge, EdgeObservation,
    EdgeProvenance,
};
#[cfg(feature = "admin")]
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
    get_access_times, get_all, list_by_path, list_by_path_recent, list_wiki_duplicate_candidates,
    normalize_for_write, record_enrichment_failure, release_event_claim, search_fts,
    search_symbolic_candidates, search_vec, set_keyword_enrichment_pending_if_unset,
    set_keyword_enrichment_status, supersede_memory, try_claim_event, update_enrichment_fields,
    update_with_revision, upsert,
};
pub(crate) use open::{
    acquire_startup_lock, configure_connection, open_read_only, open_read_write,
    retry_memory_locked, sqlite_error_is_locked,
};
pub use recall_cache::{
    recall_cache_get, recall_cache_purge_stale, recall_cache_put, recall_cache_record_hit,
    recall_cache_stats, RecallCacheHit, RecallCacheStats,
};
pub use sandbox::{
    check_sandbox_access, evaluate_sandbox_access, get_sandbox_policy, insert_sandbox_exec_audit,
    list_sandbox_exec_audit, list_sandbox_policies, list_sandbox_rules_for_role,
    path_matches_pattern, set_sandbox_policy, set_sandbox_rule,
};
pub use schema::{init_schema, init_schema_with_label_mut};
pub use sqlite_vec::{register_sqlite_vec, serialize_f32, try_load_sqlite_vec};
pub use state::{
    delete_state, get_state, insert_state_if_absent, list_derived_by_source, list_state,
    save_derived, save_derived_with_id, set_state, set_state_if_version, StateRow,
};
pub use stats_gc::{archive_stale_memories, gc_tables, stats};
#[cfg(feature = "admin")]
pub use terminal_inbox::{
    acknowledge_terminal_receipt, get_terminal_receipt, insert_terminal_receipt,
    list_terminal_receipts, NewTerminalReceipt, TerminalReceipt,
};
#[cfg(feature = "admin")]
pub use vault_db::{
    vault_count_entries, vault_delete_entry, vault_entry_exists, vault_get_config, vault_get_entry,
    vault_get_key_health, vault_get_rotation, vault_insert_audit, vault_list_entries,
    vault_list_entries_by_type, vault_list_key_health, vault_list_rotations, vault_set_config,
    vault_set_rotation, vault_touch_entry, vault_upsert_entry, vault_upsert_key_health,
};
#[cfg(feature = "admin")]
pub use virtual_capability::{vc_list_bindings, vc_upsert_binding};

#[cfg(test)]
pub(crate) use common::now_utc_iso;

#[cfg(test)]
mod tests;
