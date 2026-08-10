mod agent_state;
pub mod anchor;
mod audit;
mod common;
mod daily_pipeline;
#[cfg(feature = "admin")]
pub mod dispatch_adjudications;
#[cfg(feature = "admin")]
pub mod dispatch_outcomes;
mod doctor_probe;
mod event_ledger;
#[cfg(feature = "admin")]
pub mod exec_env;
#[cfg(feature = "admin")]
pub mod exec_env_resources;
mod filename;
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
#[cfg(feature = "admin")]
pub mod mirror_eval;
mod open;
pub mod open_context;
// tachi#1643: NOT admin-gated. #1630's premise is a host-owned sync loop with
// no Tachi daemon, so the outbox is portable surface — the same reason the v29
// migration takes no `StoreProfile`.
pub mod outbox;
mod recall_cache;
#[cfg(feature = "admin")]
mod sandbox;
mod schema;
mod search_generation;
#[cfg(feature = "admin")]
pub mod session_claims;
mod sqlite_extensions;
mod sqlite_vec;
mod state;
mod stats_gc;
pub mod store_identity;
pub mod store_profile;
#[cfg(feature = "admin")]
pub mod vault_accounts;
#[cfg(feature = "admin")]
mod vault_db;
#[cfg(feature = "admin")]
mod virtual_capability;

pub use agent_state::{get_agent_known_revisions, update_agent_known_state};
pub use anchor::{anchor_id, anchor_path, ensure_anchor, AnchorKind};
pub use audit::{audit_log_insert, audit_log_list};
pub use common::{normalize_utc_iso, normalize_utc_iso_or_now, now_utc_iso, row_to_entry};
pub use daily_pipeline::{
    collect_daily_health_snapshot, count_active_memories, count_consolidated_active_memories,
    count_distinct_access_days, count_distinct_promotion_days, list_eval_evidence,
    list_memory_ids_needing_embedding, list_promotion_candidate_ids, promote_memory_to_durable,
    CategorySourceGroup, DailyHealthDbSnapshot, DuplicateSummaryRow, EvalEvidenceRow,
};
#[cfg(feature = "admin")]
pub use dispatch_adjudications::{
    append_dispatch_adjudication, list_adjudications_for_outcome, outcome_is_adjudicated,
    DispatchAdjudication, DispatchAdjudicationSignature, NewDispatchAdjudication,
    NOT_REQUIRED_REASONS,
};
#[cfg(feature = "admin")]
pub use dispatch_outcomes::{
    derive_idempotency_key, find_outcome_by_dispatch_id, get_outcome,
    list_outcome_ids_for_dispatch, list_outcomes_by_issue_ref, list_outcomes_by_vendor_window,
    outcome_exists_for_dispatch, upsert_outcome, upsert_outcome_reconciling_terminal_placeholder,
    DispatchOutcomeRow, NewDispatchOutcome, OutcomeEvidenceClass,
};
// The three raw-`Connection` constructors are gated with the accessor pair on
// `MemoryStore` (#1585 review round 3): a bare connection is a raw-SQL bypass
// of the `store_identity` write-once guards, so the non-test portable surface
// does not get one. The read-only probes below stay portable.
pub use doctor_probe::{
    checkpoint_wal_truncate, count_chunks_rows, count_memories_missing_domain, count_memories_rows,
    count_memories_vec_rows, foundry_job_status_counts, probe_keyword_suspects, schema_version,
    table_exists, FoundryJobStatusCounts, KeywordSuspectProbe,
};
#[cfg(any(feature = "admin", test))]
pub use doctor_probe::{open_for_wal_checkpoint, open_immutable_readonly, open_raw};
pub use event_ledger::{
    continuity_metrics, insert_tachi_event, insert_tachi_event_if_absent, list_tachi_events,
};
pub use filename::{
    is_memory_db_filename, migrate_legacy_filename_if_present, LEGACY_MEMORY_DB_FILENAME,
    MEMORY_DB_FILENAME,
};
pub use gc_candidates::{
    list_memories_by_category_and_path_prefix, list_memories_by_path_prefix,
    CategoryPathPrefixMemoryRow, PathPrefixMemoryRow,
};
pub(crate) use graph::persist_confirmed_contradiction_within_tx;
pub use graph::{
    add_component_governance_edge, add_component_governance_edge_with_provenance, add_edge,
    add_edge_with_provenance, avg_importance, close_related_to_fog, count_active_observations,
    count_same_topic, edge_authority, get_contradiction_count, get_edges, get_edges_limited,
    get_superseded_ids, graph_expand, graph_expand_limited, invalidate_observation,
    list_observations_for_edge, remove_edge, ConfirmedContradictionOutcome, EdgeAuthority,
    EdgeObservation, EdgeProvenance,
};
#[cfg(feature = "admin")]
pub use hub_db::{
    hub_get, hub_get_active_version_route, hub_list, hub_list_limited, hub_record_call_outcome,
    hub_record_feedback, hub_search, hub_search_limited, hub_set_active_version_route,
    hub_set_enabled, hub_set_review, hub_upsert,
};
#[cfg(test)]
pub(crate) use memory_crud::query_hash;
#[cfg(test)]
pub(crate) use memory_crud::record_access;
pub(crate) use memory_crud::record_access_with_updates;
pub(crate) use memory_crud::search_fts_raw_match;
pub(crate) use memory_crud::search_symbolic_candidates_with_relevance;
pub(crate) use memory_crud::upsert_with_validated_reference_mutations_within_tx_and_metadata_removals;
pub(crate) use memory_crud::wiki_corpus_store_sql_splice;
#[cfg(test)]
pub(crate) use memory_crud::AccessUpdate;
pub(crate) use memory_crud::MEMORY_SELECT_COLUMNS;
pub use memory_crud::{
    access_event_density, archive_memory, archive_memory_if_revision, delete,
    delete_memories_symbolic_fts, fetch_by_ids, fetch_by_ids_excluding_store_internal,
    find_active_wiki_entry_by_path, find_exact_path_text_id, get_access_times, get_all,
    get_use_access_times, is_reserved_wiki_internal_path, is_user_facing_wiki_entry,
    list_active_wiki_ingest_predecessors, list_by_path, list_by_path_active_unsuperseded,
    list_by_path_recent, list_user_facing_wiki_entries, list_wiki_duplicate_candidates,
    normalize_for_write, record_enrichment_failure, record_memory_use, release_event_claim,
    restore_archived_if_revision, search_fts, search_symbolic_candidates, search_vec,
    set_keyword_enrichment_pending_if_unset, set_keyword_enrichment_status, supersede_memory,
    supersede_memory_if_revision, symbolic_trigram_select_sql, sync_memories_symbolic_fts,
    try_claim_event, update_enrichment_fields, update_with_revision, AccessEventDensity,
    AccessEventKind, IdlessUpsertResult, InsertMemoryResult, NearDuplicatePolicy,
    ValidatedReferenceMutation, MAX_REFERENCE_BYTES, MAX_REFERENCE_HASH_BYTES,
    MAX_REFERENCE_ID_BYTES, MAX_REFERENCE_KIND_BYTES, MAX_REFERENCE_SECTION_BYTES,
    MAX_REFERENCE_TIMESTAMP_BYTES, SYMBOLIC_TRIGRAM_SELECT_SQL_TEMPLATE,
};
pub(crate) use memory_crud::{
    archive_with_metadata_if_expected_state, restore_with_metadata_if_expected_state,
    supersede_with_metadata_if_expected_state, update_with_revision_if_expected_state,
};
/// tachi#1446 drift guard for hand-built `memories` test fixtures — see the
/// function's own doc for when a hand-built fixture is legitimate.
#[cfg(test)]
pub(crate) use memory_crud::{
    assert_memories_fixture_matches_select_columns, memory_select_required_columns,
};
/// tachi#1607 snapshot import: see `memory_crud::snapshot_import`.
pub(crate) use memory_crud::{
    import_snapshot_row_within_tx, memory_row_exists_within_tx,
    read_snapshot_lifecycle_row_within_tx, read_snapshot_vector_blob_within_tx,
    SnapshotLifecycleRow, SnapshotVectorRow,
};
/// Caller-transaction upsert seam for lifecycle-apply: runs the full upsert
/// body (main row + FTS + vectors + idless semantics) inside a caller-owned
/// `BEGIN IMMEDIATE` transaction. See `memory_crud::upsert_within_tx`.
pub(crate) use memory_crud::{
    insert_if_absent, insert_if_absent_within_tx, insert_rem_operation_if_absent_within_tx, upsert,
    upsert_idless, upsert_within_tx, upsert_within_tx_allowing_reserved_anchor_ids,
};
/// Public: see `open::ensure_reserved_reference_write_guard`'s doc comment.
pub use open::ensure_reserved_reference_write_guard;
/// Public: benchmarks/diagnostics outside this crate read the process-wide
/// lock-retry backoff counter without needing a tracing subscriber (see
/// `open::lock_retry_backoff_count`'s doc comment).
pub use open::lock_retry_backoff_count;
/// Public: see `open::sqlite_error_is_locked`'s doc comment.
pub use open::sqlite_error_is_locked;
pub(crate) use open::{
    acquire_startup_lock, authorize_planner_maintenance, authorize_reserved_reference_write,
    authorize_schema_migration, configure_connection, install_reserved_reference_authorizer,
    open_read_only, open_read_write, open_read_write_with_busy_timeout,
    register_reserved_reference_write_guard, retry_memory_locked, scoped_sqlite_busy_deadline,
    sqlite_busy_deadline_remaining, validate_persistent_trigger_inventory,
    ReservedReferenceWriteFlag,
};
pub use open_context::{
    DbOpenContext, MigrationAuthority, OpenIntent, SCHEMA_MIGRATION_LEGACY_ENV,
};
/// tachi#1643 durable outbox write/read seams. Crate-internal on purpose: they
/// take a `Transaction`/`Connection`, and the invariant this leaf exists to
/// hold — an event is only ever durable in the same transaction as the object
/// it announces — is enforced by `crate::store::outbox`, which owns the
/// `BEGIN IMMEDIATE` boundary. See `outbox`'s module doc.
pub(crate) use outbox::{
    claim_outbox_events_within_tx, insert_outbox_event_within_tx,
    insert_resolution_successor_event_within_tx, list_outbox_events_by_state, read_outbox_event,
    read_outbox_health, refuse_invalid_class, refuse_non_canonical_digest,
    refuse_reserved_resolved_class, transition_outbox_event_within_tx,
    transition_outbox_resolved_conflict_within_tx, ClaimedOutboxRow,
};
pub use outbox::{
    LocalStoreStatus, NewOutboxEvent, OutboxEventRow, OutboxHealth, OutboxState, RemoteSyncStatus,
    MAX_OUTBOX_CLASS_BYTES, OUTBOX_PAYLOAD_DIGEST_HEX_LEN,
};
pub use recall_cache::{
    recall_cache_get, recall_cache_invalidate_all, recall_cache_purge_stale, recall_cache_put,
    recall_cache_record_hit, recall_cache_stats, RecallCacheHit, RecallCacheStats,
};
#[cfg(feature = "admin")]
pub use sandbox::{
    check_sandbox_access, evaluate_sandbox_access, get_sandbox_policy, insert_sandbox_exec_audit,
    list_sandbox_exec_audit, list_sandbox_policies, list_sandbox_rules_for_role,
    path_matches_pattern, set_sandbox_policy, set_sandbox_rule,
};
#[cfg(test)]
pub(crate) use schema::install_reserved_reference_guard;
pub use schema::{init_schema, init_schema_with_label_mut, SchemaInitOutcome};
pub use search_generation::{bump_search_generation, search_generation};
pub use sqlite_extensions::enable_simple_auto_extension;
pub use sqlite_vec::{register_sqlite_vec, serialize_f32, try_load_sqlite_vec};
pub(crate) use state::refuse_store_identity_namespace;
pub use state::{
    backfill_missing_expires_at, delete_state, get_state, insert_state_if_absent,
    list_derived_by_source, list_state, reap_expired_state, save_derived, save_derived_with_id,
    set_state, set_state_if_version, StateRow,
};
pub(crate) use stats_gc::write_gc_archived_receipt;
pub use stats_gc::{
    archive_stale_memories, archive_stale_memories_with_config, gc_tables, stats,
    GC_MEMORY_ARCHIVED_EVENT_TYPE,
};
pub use store_identity::StoreIdentity;
pub use store_profile::{
    StoreProfile, STORE_IDENTITY_NAMESPACE, STORE_PROFILE_KEY, STORE_ROLE_KEY,
};
#[cfg(feature = "admin")]
pub use vault_accounts::{
    append_provider_account_event, find_provider_account_by_auth_ref,
    find_provider_accounts_by_fingerprint, get_account_custody, get_account_custody_by_auth_ref,
    get_provider_account, insert_account_custody, insert_provider_account,
    list_provider_account_aliases, list_provider_account_events, list_provider_accounts,
    record_account_fingerprint, record_provider_account_alias, resolve_auth_ref,
    retire_provider_account, retire_provider_account_alias, update_custody_target,
    vault_pool_members_digest, AccountRetirement, AliasObservation, FingerprintUpdate,
    POOL_MEMBERS_DIGEST_SCHEME,
};
#[cfg(feature = "admin")]
pub use vault_db::{
    vault_count_entries, vault_delete_entry, vault_entry_exists, vault_get_config, vault_get_entry,
    vault_get_key_health, vault_get_rotation, vault_insert_audit, vault_list_entries,
    vault_list_entries_by_type, vault_list_entry_timestamps, vault_list_key_health,
    vault_list_rotations, vault_set_config, vault_set_rotation, vault_touch_entry,
    vault_upsert_entry, vault_upsert_key_health,
};
#[cfg(feature = "admin")]
pub use virtual_capability::{vc_list_bindings, vc_upsert_binding};

#[cfg(test)]
mod tests;
