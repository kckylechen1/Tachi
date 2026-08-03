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

// ── Bundled-SQLite security floor, compile-time half (#833, #1453) ───────────
//
// `SQLITE_VERSION_NUMBER` is the version of the SQLite headers this crate is
// being compiled against, encoded as major*1_000_000 + minor*1_000 + patch;
// 3.50.3 is 3_050_003. Failing this assertion fails the *build*, which is the
// half of the floor that reaches consumers: a downstream crate taking memcore
// as a git dependency never runs memcore's test suite, so the runtime check in
// `db::tests::sqlite_security_floor` protects only people who run our tests.
//
// The floor is 3.50.3 because of CVE-2025-7709 (fixed 3.50.3): a corrupt FTS5
// index yields unauthorized memory access. memcore's core search *is* FTS5 and
// its content is user-writable (memories, wiki, events, URL ingest), so this is
// the exact attack surface, not a theoretical one. The workspace `rusqlite`
// requirement is a range (`>=0.37, <0.39`) whose lower bound resolves to
// libsqlite3-sys 0.35.0 / SQLite 3.50.2 — below the floor. That lower bound is
// permitted *only* because this assertion refuses such a build outright.
//
// If this fails: bump `rusqlite`/`libsqlite3-sys` until the bundled SQLite is
// >= 3.50.3. Do not lower the constant, and do not replace it and the runtime
// test with one shared constant — the duplicated literal is deliberate, so that
// lowering the floor in one place still trips the other.
const _: () = assert!(
    rusqlite::ffi::SQLITE_VERSION_NUMBER >= 3_050_003,
    "SQLite headers are below the 3.50.3 security floor (CVE-2025-7709: corrupt \
     FTS5 index -> unauthorized memory access). Bump rusqlite/libsqlite3-sys; do \
     not lower this constant. See the workspace Cargo.toml #833 comment."
);

#[cfg(feature = "admin")]
pub mod agent_profile;
// #1297 leaf 1. Portable-kernel, not admin-gated: it has no network, no `gh`,
// no `Connection`, and a downstream consumer pinning this crate wants a
// complete memcore. Schema delta: zero — assertions ride `tachi_events`.
//
// Gated per #1564 pending owner disposition (verified dead: exported but
// zero functional references from tachi-server or elsewhere in this crate,
// only its own tests reference it). Owning contract: #1297.
#[cfg(feature = "contract-leaves")]
pub mod current_truth;
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
pub mod recall_coverage;
mod recall_impressions;
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
#[cfg(feature = "contract-leaves")]
pub use current_truth::{
    build_truth_assertion_event, classify_assertion, decode_truth_assertion, derive_action_queue,
    reduce_current_truth, truth_assertion_event_id, ActionItemV1, ActionKindV1, AssertionRefV1,
    AssertionRelationV1, CandidateReasonV1, CandidateRecordV1, CurrentTruthDiagnosticsV1,
    CurrentTruthError, CurrentTruthFold, CurrentTruthProjectionV1, CurrentTruthStatsV1,
    IssuerClass, PredicateTruthV1, RejectedAssertionV1, SubjectTruthV1, TruthAssertionInputV1,
    TruthAssertionV1, TruthEventEnvelopeV1, TruthIssuerV1, TruthPredicate, TruthStateV1,
    TruthValue, TRUTH_ASSERTION_DOMAIN, TRUTH_ASSERTION_EVENT_TYPE,
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
    CategoryPathPrefixMemoryRow, ConfirmedContradictionOutcome, FoundryJobStatusCounts,
    InsertMemoryResult, PathPrefixMemoryRow,
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
    is_anchor_entry, is_continuity_projection_entry, is_continuity_projection_path, is_eval_entry,
    is_handoff_entry, is_internal_only_row, is_kanban_entry, is_namespace_search_noise,
    is_recall_cache_entry, is_reserved_wiki_rem_id,
    is_user_facing_wiki_entry_allowing_recall_cache, is_wiki_entry, path_contains_recall_cache,
    path_in_namespace, path_prefix_opts_into_continuity_projection,
    path_prefix_opts_into_recall_cache, surface_of, surface_sql_clause, user_facing_wiki_sql_where,
    Surface, DOCS_SURFACE_SQL_WHERE, DOCS_SURFACE_SQL_WHERE_M, FOUNDRY_RECALL_CACHE_SOURCE,
    RECALL_CACHE_SQL_WHERE, RECALL_CACHE_SQL_WHERE_M, WIKI_REM_OPERATION_ID_PREFIX,
};
pub use near_dup::{near_duplicate_raw_pairs, text_token_jaccard, NEAR_DUP_RAW_SCAN_CAP};
pub use noise::{is_noise_text, should_skip_query};
pub use recall_config::{RecallConfig, TypoFallbackConfig};
pub use recall_coverage::{
    format_recall_coverage_human, is_recall_coverage_path_list_only, run_recall_coverage_probe,
    run_recall_coverage_probe_with_corpus, run_recall_coverage_probe_with_equivalences,
    RecallCoverageEquivalenceCorpus, RecallCoverageEquivalenceSet, RecallCoverageEvidenceKind,
    RecallCoverageExpectedIdLane, RecallCoverageFactEvidence, RecallCoverageMetrics,
    RecallCoverageOptions, RecallCoverageOutcome, RecallCoveragePartitionCounts,
    RecallCoveragePriorScoredCountSplit, RecallCoverageQuerySource, RecallCoverageReport,
    RecallCoverageTarget, DEFAULT_RECALL_COVERAGE_CANDIDATES_PER_CHANNEL,
    DEFAULT_RECALL_COVERAGE_TOP_K, RECALL_COVERAGE_EQUIVALENCE_SCHEMA_VERSION,
};
pub use recall_impressions::{
    increment_recall_impression_replay_count, replay_recall_impression_group,
    RecallReplayCandidate, RecallReplayReport,
};
pub use relation_ontology::ComponentGovernanceRelation;
pub use scorer::{
    generic_precision_multiplier, surprise_score, surprise_score_with_config, DecayPolicy,
    DecayPolicyContext, DefaultDecayPolicy, HybridWeights, PrecisionMatcher, DEFAULT_DECAY_POLICY,
};
pub use search::{
    apply_blend_relevance, hybrid_search, hybrid_search_with_receipt,
    merge_rerank_order_with_hybrid_floor, AccessRecordingPhaseReceipt, CandidateLegEvidence,
    CandidatePhaseReceipt, ChannelPhaseReceipt, FetchPhaseReceipt, FtsExpansionGroupReceipt,
    GraphPhaseReceipt, LayerAvailability, RankPhaseReceipt, SearchOptions, SearchPhaseReceipt,
    SearchReceiptDatabaseScope, SearchReceiptOperation, TypoFallbackPhaseReceipt,
    HYBRID_HEAD_FRACTION,
};
pub use types::{
    AuthorityLevel, ContinuityCandidate, ContinuityCandidateBatch, ContinuityMetrics,
    ContinuityOutcomeLabel, EffectScope, ExpectedMemoryState, GcConfig, GraphExpandResult,
    HybridScore, MemoryEdge, MemoryEntry, MetricCount, OutcomeEvidenceBasis, ProjectionKind,
    RetentionPolicy, SearchResult, SessionOutcomeKind, SessionOutcomeMetrics, StatsResult,
    TachiEventQuery, TachiEventRecord,
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
    pub(crate) reserved_reference_write: db::ReservedReferenceWriteFlag,
    pub vec_available: bool,
    /// Manifest label for this DB ("global", "wiki", a project name, or
    /// "unknown"). Used by path validation at write time.
    pub(crate) db_label: String,
    /// Whether path validation is enforced for this store. Disabled when
    /// db_label is unknown to avoid breaking unlabeled callers.
    pub(crate) path_validation: bool,
    /// Physical identity captured immediately after this connection opened.
    /// Long-lived runtimes use it to fail closed if the path is later replaced
    /// while SQLite still holds the original file descriptor.
    pub(crate) opened_physical_db_identity: Option<String>,
}

#[cfg(test)]
mod test_fixtures;

#[cfg(test)]
#[path = "recall_coverage_tests.rs"]
mod recall_coverage_tests;

#[cfg(test)]
#[path = "lib_tests.rs"]
mod tests;

/// Compile-time marker used by docs/tests to assert the feature boundary.
#[cfg(feature = "admin")]
pub const ADMIN_SURFACE_ENABLED: bool = true;
#[cfg(not(feature = "admin"))]
pub const ADMIN_SURFACE_ENABLED: bool = false;
