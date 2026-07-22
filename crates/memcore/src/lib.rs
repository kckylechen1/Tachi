// lib.rs — Public API for memcore
//
// Re-exports all primary types and provides a MemoryStore handle that
// bundles a rusqlite::Connection with convenience methods.
//
// ## Portable vs admin split
//
// The crate has two layers:
//
// 1. **Portable kernel** (always on): `MemoryStore`, schema/migrations,
//    CRUD, hybrid search/scorer, graph, events, sandbox, path routing,
//    recall config/noise. This is what HyperTachi / HyperMemory should
//    sync. Build with `default-features = false` (see `portable-kernel`).
//
// 2. **Admin / operator** (`feature = "admin"`, on by default for Tachi):
//    vault secrets, Hub capability catalog, Foundry job queue types,
//    agent_profile product surfaces. Downstream memory forks do
//    not need these to open a DB or run save/search/readiness.

#[cfg(feature = "admin")]
pub mod agent_profile;
pub mod db;
pub mod embed_config;
pub mod error;
#[cfg(feature = "admin")]
pub mod foundry;
#[cfg(feature = "admin")]
pub mod hub;
pub mod namespace;
pub mod near_dup;
pub mod noise;
pub mod path_router;
pub mod recall_config;
pub mod relation_ontology;
pub mod scorer;
pub mod search;
pub mod store;
pub mod types;
#[cfg(feature = "admin")]
pub mod vault;
pub mod vector_backfill;

#[cfg(feature = "admin")]
pub use agent_profile::{
    AgentProfileIdentity, AgentProfilePack, AgentProfileRule, AgentProfileSource,
    RenderedAgentProfile, AGENT_PROFILE_PACK_SCHEMA_VERSION,
};
#[cfg(feature = "admin")]
pub use db::dispatch_adjudications::{
    append_dispatch_adjudication, list_adjudications_for_outcome, outcome_is_adjudicated,
    DispatchAdjudication, DispatchAdjudicationSignature, NewDispatchAdjudication,
    NOT_REQUIRED_REASONS,
};
#[cfg(feature = "admin")]
pub use db::dispatch_outcomes::{
    derive_idempotency_key, find_outcome_by_dispatch_id, get_outcome,
    list_outcome_ids_for_dispatch, list_outcomes_by_issue_ref, list_outcomes_by_vendor_window,
    outcome_exists_for_dispatch, upsert_outcome, upsert_outcome_reconciling_terminal_placeholder,
    DispatchOutcomeRow, NewDispatchOutcome, OutcomeEvidenceClass,
};
#[cfg(feature = "admin")]
pub use db::exec_env::{
    find_active_exec_env_by_path, get_exec_env, insert_exec_env, list_exec_envs, reclaim_exec_env,
    EnvClass, ExecEnvLease, ExecEnvSelector, ExecEnvState, NewExecEnvLease, ReclaimOutcome,
};
#[cfg(feature = "admin")]
pub use db::exec_env_resources::{
    active_binding_count, bind_resource, find_resource_by_path, get_resource, insert_resource,
    list_bound_resource_paths, list_resources, quarantine_resource, reclaim_resource,
    record_resource_measurement, release_binding, release_quarantine, BindOutcome, ExecEnvResource,
    NewExecEnvResource, QuarantineOutcome, RegisterOutcome, ReleaseBindingOutcome,
    ReleaseQuarantineOutcome, ResourceKind, ResourceReclaimOutcome, ResourceState,
};
#[cfg(feature = "admin")]
pub use db::foundry_config::{get_foundry_config, set_foundry_config, PerDbConfig};
#[cfg(feature = "admin")]
pub use db::foundry_jobs::{
    claim_foundry_job_for_run, find_foundry_jobs_for_memory, gc_foundry_jobs, insert_foundry_job,
    job_status_histogram, load_pending_foundry_jobs, requeue_retryable_foundry_jobs,
    update_foundry_job_status_with_reason, FoundryJobLease, FoundryJobSummary, FoundryRetryPolicy,
    InsertFoundryJobResult, JobStatusHistogram, PersistedFoundryJob, RequeueOutcome,
};
#[cfg(feature = "admin")]
pub use db::mirror_eval::{
    append_mirror_eval_adjudication, get_mirror_eval_run_view, get_observation, get_run_by_id,
    get_run_by_native_child_id, list_adjudications_for_run, record_mirror_eval_observation,
    register_mirror_eval_run, run_is_adjudicated, MirrorEvalAdjudication, MirrorEvalObservation,
    MirrorEvalRun, MirrorEvalRunView, NewMirrorEvalAdjudication, NewMirrorEvalObservation,
    NewMirrorEvalRun,
};
pub use db::row_to_entry;
#[cfg(feature = "admin")]
pub use db::session_claims::{
    bind_work_claim_exec_env, gc_session_claims, get_claim, handoff_work_claim, heartbeat_claim,
    heartbeat_work_claim, holder_evidence, insert_agent_identity, insert_claim, insert_work_claim,
    is_claim_stale, list_active_claims, list_claims, record_rejected_admission,
    record_unverified_admission, release_claim, release_work_claim, upsert_or_heartbeat_claim,
    AdmissionState, AgentIdentity, ClaimSelector, ClaimState, HolderEvidence, NewSessionClaim,
    NewWorkClaim, ReleaseOutcome, SessionClaim, SessionClaimsGc, UnverifiedAdmissionState,
    WorkClaim, WorkClaimHandoff, WorkClaimHandoffRequest, WorkClaimHeartbeat, WorkClaimMode,
};
pub use db::{anchor_id, anchor_path, AnchorKind};
pub use db::{
    is_memory_db_filename, migrate_legacy_filename_if_present, LEGACY_MEMORY_DB_FILENAME,
    MEMORY_DB_FILENAME,
};
pub use db::{
    CategoryPathPrefixMemoryRow, FoundryJobStatusCounts, InsertMemoryResult, PathPrefixMemoryRow,
};
pub use db::{CategorySourceGroup, DailyHealthDbSnapshot, DuplicateSummaryRow, EvalEvidenceRow};
pub use db::{DbOpenContext, MigrationAuthority, OpenIntent};
pub use embed_config::embed_raw_tier_enabled;
pub use error::{MemoryError, WorkClaimTransitionReason};
#[cfg(feature = "admin")]
pub use foundry::{
    AgentEvolutionProposal, AgentEvolutionSynthesis, AgentProfileDocument,
    AgentProfileDocumentKind, FoundryEvidence, FoundryEvidenceKind, FoundryJobKind, FoundryJobSpec,
    FoundryJobStatus, FoundryModelLane,
};
#[cfg(feature = "admin")]
pub use hub::{HubCapability, VirtualCapabilityBinding};
pub use namespace::{
    is_anchor_entry, is_eval_entry, is_handoff_entry, is_kanban_entry, is_namespace_search_noise,
    is_recall_cache_entry, is_wiki_entry, path_contains_recall_cache, path_in_namespace,
    path_prefix_opts_into_recall_cache, surface_of, surface_sql_clause, Surface,
    DOCS_SURFACE_SQL_WHERE, DOCS_SURFACE_SQL_WHERE_M, FOUNDRY_RECALL_CACHE_SOURCE,
    RECALL_CACHE_SQL_WHERE, RECALL_CACHE_SQL_WHERE_M,
};
pub use near_dup::{near_duplicate_raw_pairs, text_token_jaccard, NEAR_DUP_RAW_SCAN_CAP};
pub use noise::{is_noise_text, should_skip_query};
pub use recall_config::RecallConfig;
pub use relation_ontology::ComponentGovernanceRelation;
pub use scorer::{
    generic_precision_multiplier, surprise_score, DecayPolicy, DecayPolicyContext,
    DefaultDecayPolicy, HybridWeights, PrecisionMatcher, DEFAULT_DECAY_POLICY,
};
pub use search::{
    apply_blend_relevance, hybrid_search, hybrid_search_with_receipt,
    merge_rerank_order_with_hybrid_floor, AccessRecordingPhaseReceipt, CandidatePhaseReceipt,
    ChannelPhaseReceipt, FetchPhaseReceipt, FtsExpansionGroupReceipt, GraphPhaseReceipt,
    LayerAvailability, RankPhaseReceipt, SearchOptions, SearchPhaseReceipt,
    SearchReceiptDatabaseScope, SearchReceiptOperation, HYBRID_HEAD_FRACTION,
};
pub use types::{
    AuthorityLevel, ContinuityCandidate, ContinuityCandidateBatch, ContinuityMetrics,
    ContinuityOutcomeLabel, EffectScope, GcConfig, GraphExpandResult, HybridScore, MemoryEdge,
    MemoryEntry, MetricCount, OutcomeEvidenceBasis, ProjectionKind, RetentionPolicy, SearchResult,
    SessionOutcomeKind, SessionOutcomeMetrics, StatsResult, TachiEventQuery, TachiEventRecord,
};
#[cfg(feature = "admin")]
pub use vault::{
    api_key_pool_member_index, normalize_secret_type, VaultCipher, VaultConfig, VaultEntry,
    VaultKeyRotation, SECRET_TYPES, SECRET_TYPE_API_KEY, SECRET_TYPE_COOKIE, SECRET_TYPE_JSON_BLOB,
    SECRET_TYPE_OAUTH_TOKEN, SECRET_TYPE_OTHER,
};
pub use vector_backfill::{VectorBackfillCounts, VectorBackfillEntry, VectorBackfillScope};

use rusqlite::Connection;

/// High-level handle that owns a database connection.
/// Language bindings (NAPI, PyO3) will wrap this struct.
///
/// Method definitions are split across `crate::store::*` extension modules
/// so this file stays focused on exports and the handle shape. Fields are
/// `pub(crate)` so those sibling modules can construct `MemoryStore` and access
/// the connection directly; they remain private to the crate.
///
/// Admin-only methods (vault/hub) live in `store::{vault,hub}` and
/// are compiled only when the `admin` feature is enabled.
pub struct MemoryStore {
    pub(crate) conn: Connection,
    pub vec_available: bool,
    /// Manifest label for this DB ("global", "wiki", a project name, or
    /// "unknown"). Used by path validation at write time.
    pub(crate) db_label: String,
    /// Whether path validation is enforced for this store. Disabled when
    /// db_label is unknown to avoid breaking unlabeled callers.
    pub(crate) path_validation: bool,
}

#[cfg(test)]
#[path = "lib_tests.rs"]
mod tests;

/// Compile-time marker used by docs/tests to assert the feature boundary.
#[cfg(feature = "admin")]
pub const ADMIN_SURFACE_ENABLED: bool = true;
#[cfg(not(feature = "admin"))]
pub const ADMIN_SURFACE_ENABLED: bool = false;
