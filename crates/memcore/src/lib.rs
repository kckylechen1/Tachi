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
pub mod canonical_digest;
/// Model-broker catalog row types (tachi#1681). `admin`-gated because all
/// six catalog tables are `SchemaScope::Product`.
///
/// # Canonical endpoint credential guard
///
/// [`catalog::endpoint`] is this stack's single source for "this endpoint URL
/// is smuggling a credential" — the credential-shaped query keys and the
/// userinfo scan over the authority. Catalog import calls this guard before a
/// URL can enter a durable operator-visible row, and the broker CLI calls it
/// before rendering one. Keep the rule here rather than duplicating its deny
/// list at each caller.
#[cfg(feature = "admin")]
pub mod catalog;
pub mod db;
pub mod embed_config;
pub mod error;
#[cfg(feature = "admin")]
pub mod foundry;
#[cfg(feature = "admin")]
pub mod hub;
pub mod kernel_policy;
pub mod model_broker_seam;
pub mod namespace;
pub mod near_dup;
pub mod noise;
pub mod path_router;
pub mod private_partition;
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
pub use canonical_digest::{canonical_json, canonical_json_digest_hex, canonical_json_eq};
/// tachi#1681 model-broker catalog: the row types the six catalog tables
/// carry, re-exported at the root so the env-import and status projection
/// reach them without importing the internal `catalog::` module layout.
#[cfg(feature = "admin")]
pub use catalog::{
    partition_authoritative_at, AttachmentBounds, AuthoritativeDeployment, AuthoritativePartition,
    CatalogFreshness, CatalogSource, DeploymentCapabilities, DeploymentEventKind,
    EmbeddingsCapability, ModelAlias, ModelAliasBinding, ModelDeployment, ModelDeploymentEvent,
    ModelDeploymentHealth, NewModelDeployment, NewModelDeploymentEvent, NotAuthoritative,
    PricingSnapshot, ProtocolKind, ALIAS_STATUS_ACTIVE, ALIAS_STATUS_RETIRED,
    DEPLOYMENT_STATUS_ACTIVE, DEPLOYMENT_STATUS_RETIRED, PRICING_SNAPSHOT_SCHEME,
};
#[cfg(feature = "admin")]
pub use db::a2a::{
    consume_a2a_for_recipient, expire_a2a_for_recipient, insert_a2a_envelope, list_a2a_status,
    resolve_a2a_recipient_eligibility, A2aDeliveryReceipt, A2aEnvelope, A2aInsertOutcome,
    A2aRecipientEligibility, A2aStatusRow, A2aTransitionActor, NewA2aEnvelope,
    A2A_SAME_HOST_TRUST_DOMAIN, A2A_TURN_RESPONSE_KIND, MAX_A2A_STORAGE_BATCH,
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
/// tachi#1675 PR2 (design D1): the ONE spine-tagged evidence row every
/// routing/quality reader joins through. Read-only surface.
#[cfg(feature = "admin")]
pub use db::eval_projection::{
    list_dispatch_eval_observations, list_eval_observations, list_mirror_eval_observations,
    policy_revision_census, EvalAdjudicationFacts, EvalObservation, EvalRouteFacts, EvalSpine,
    PolicyRevisionCensus, ProfileAttributionBasis, OCCURRED_AT_BASIS_LEGACY_CREATED_AT,
};
/// tachi#1675 PR3 (design D6): the replay half of the same read surface —
/// the ledger folded forward from its append-only judgment log rather than
/// read as current state. Required to agree canonically with
/// [`list_eval_observations`] over the same window.
#[cfg(feature = "admin")]
pub use db::eval_replay::{
    canonical_eval_observations, eval_observations_digest, replay_eval_observations, EvalReplay,
    REPLAY_ORDERING_BASIS,
};
#[cfg(feature = "admin")]
pub use db::exec_env::{
    find_active_exec_env_by_path, find_live_exec_env_by_path, get_exec_env, insert_exec_env,
    list_exec_envs, reclaim_exec_env, EnvClass, ExecEnvLease, ExecEnvSelector, ExecEnvState,
    NewExecEnvLease, ReclaimOutcome,
};
#[cfg(feature = "admin")]
pub use db::exec_env_resources::{
    active_binding_count, bind_resource, exec_env_resource_removal_refusal, find_resource_by_path,
    get_resource, insert_resource, list_bound_resource_paths, list_resources, quarantine_resource,
    quarantine_resources_atomically, reclaim_resource, record_resource_measurement,
    release_binding, release_quarantine, BindOutcome, ExecEnvResource, NewExecEnvResource,
    QuarantineOutcome, RegisterOutcome, ReleaseBindingOutcome, ReleaseQuarantineOutcome,
    ResourceKind, ResourceReclaimOutcome, ResourceState,
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
pub use db::harness_session_attachments::{
    attach_harness_session, authorize_harness_session_attachment, get_harness_session_attachment,
    HarnessSessionAttachment, HarnessSessionAttachmentAdmission,
    HarnessSessionAttachmentAuthorization, HarnessSessionAttachmentCapabilities,
    HarnessSessionAttachmentReceipt, HarnessSessionAttachmentSelector,
    HarnessSessionAttachmentState, HarnessSessionHostAdmission, NewHarnessSessionAttachment,
    ACP_CAPABILITY_CLASSES, ACP_SESSION_CAPABILITIES, ACP_TOOL_PROFILES,
    TRUSTED_LOCAL_HOST_DECLARED_BASIS,
};
#[cfg(feature = "admin")]
pub use db::mirror_eval::{
    append_mirror_eval_adjudication, get_mirror_eval_run_view, get_observation, get_run_by_id,
    get_run_by_native_child_id, list_adjudications_for_run, record_mirror_eval_observation,
    register_mirror_eval_run, run_is_adjudicated, MirrorEvalAdjudication, MirrorEvalObservation,
    MirrorEvalRun, MirrorEvalRunView, NewMirrorEvalAdjudication, NewMirrorEvalObservation,
    NewMirrorEvalRun,
};
/// tachi#1643 durable outbox (#1630 A1). Ungated: the outbox is portable
/// surface, so a `StoreProfile::PortableKernel` database carries it and a
/// portable build can drive it.
pub use db::outbox::{
    LocalStoreStatus, OutboxEventRow, OutboxHealth, OutboxState, RemoteSyncStatus,
    DEFAULT_OUTBOX_HEALTH_STALE_AFTER, MAX_OUTBOX_CLASS_BYTES,
};
#[cfg(feature = "admin")]
pub use db::route_eval::{
    get_eval_rubric_score, get_route_decision_by_dispatch_id, get_route_recommendation,
    insert_eval_rubric_score, insert_route_decision_idempotent, insert_route_recommendation,
    list_eval_rubric_scores, list_route_decisions, EvalRubricScoreRow, NewEvalRubricScore,
    NewRouteDecision, NewRouteRecommendation, RouteDecisionRow, RouteRecommendationRow,
    ASSIGNMENT_MODES, RUBRIC_CONFIDENCE_VALUES, RUBRIC_DIMENSION_VALUES,
    RUBRIC_INDEPENDENCE_BASIS_VALUES, RUBRIC_SUBJECT_KINDS,
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
    is_memory_db_filename, migrate_legacy_filename_if_present, resolve_memory_db_read_path,
    LEGACY_MEMORY_DB_FILENAME, MEMORY_DB_FILENAME,
};
pub use db::{normalize_utc_iso, normalize_utc_iso_or_now, now_utc_iso};
pub use db::{
    CategoryPathPrefixMemoryRow, ConfirmedContradictionOutcome, FoundryJobStatusCounts,
    InsertMemoryResult, PathPrefixMemoryRow,
};
pub use db::{CategorySourceGroup, DailyHealthDbSnapshot, DuplicateSummaryRow, EvalEvidenceRow};
pub use db::{DbOpenContext, MigrationAuthority, OpenIntent, StoreProfile};
pub use db::{
    DeleteMaintenanceOutcome, GcMaintenanceOutcome, MaintenanceClassFact,
    OperatorMaintenanceCommittedReceiptBinding, OperatorMaintenanceOperation,
    OperatorMaintenancePlanBinding, OPERATOR_DELETE_CLASSES, OPERATOR_GC_CLASSES,
};
pub use embed_config::embed_raw_tier_enabled;
pub use error::{
    MemoryError, OutboxOutcomeRefusal, ProviderPlanRefusal, WorkClaimTransitionReason,
};
#[cfg(feature = "admin")]
pub use foundry::{
    AgentEvolutionProposal, AgentEvolutionSynthesis, AgentProfileDocument,
    AgentProfileDocumentKind, FoundryEvidence, FoundryEvidenceKind, FoundryJobKind, FoundryJobSpec,
    FoundryJobStatus, FoundryModelLane,
};
#[cfg(feature = "admin")]
pub use hub::{HubCapability, VirtualCapabilityBinding};
pub use kernel_policy::{EmbedPolicy, KernelPolicy};
/// Test-only model-broker fixture resolver. Its re-export carries the same
/// `broker-fixtures` gate as the type itself, so downstream tests must opt in
/// explicitly from `[dev-dependencies]`.
#[cfg(any(test, feature = "broker-fixtures"))]
pub use model_broker_seam::StaticFixtureResolver;
/// Reachable public closure of the model-broker control-plane seam. A type is
/// listed only when a consumer must name it to build an input or read an output.
///
/// - `ModelRef` → `SeamError` (its constructor's error).
/// - `ResolvedDeployment` → `ResolvedDeploymentParts` (its constructor's input
///   and wire shape), `WireDialect`, `DeploymentCapabilities`,
///   `DeploymentBounds`.
/// - `ResolutionOutcome` → `CandidateEvaluation` → `ExclusionReason`;
///   `Selection` → `AbstainReason`; `ResolutionRevisions`; `BudgetEstimate`;
///   `FALLBACK_ORDER_CAP` (the bound its constructor enforces, which callers
///   must respect before calling).
/// - `OperationalResolver` → `ResolverInput` → `CatalogSnapshot`,
///   `HealthSnapshot` → `DeploymentCooldown`, `AccountSnapshot` →
///   `AccountAvailability`, `BudgetContext`, `PinContext`, `RetryContext`.
/// - `HealthObservation` → `InvocationErrorClass`, `RetryAfter`,
///   `ObservationEvidence`.
///
/// Nothing else in the module is public, so the list is closed by construction;
/// the module's internal deserialization shadows are private and deliberately
/// unreachable.
pub use model_broker_seam::{
    AbstainReason, AccountAvailability, AccountSnapshot, BudgetContext, BudgetEstimate,
    CandidateEvaluation, CatalogSnapshot, DeploymentBounds, DeploymentCooldown, ExclusionReason,
    HealthObservation, HealthSnapshot, InvocationErrorClass, ModelRef, ObservationEvidence,
    OperationalResolver, PinContext, ResolutionOutcome, ResolutionRevisions, ResolvedDeployment,
    ResolvedDeploymentParts, ResolverInput, RetryAfter, RetryContext, SeamError, Selection,
    WireDialect, FALLBACK_ORDER_CAP,
};
pub use private_partition::{
    AdmittedPartition, CapabilityReceipt, PartitionCapability, PartitionKeyProvider,
    PrivatePartition, PrivatePartitionOpenContext, StaticKeyProvider, SubjectId, TrustDomainId,
    STORE_PRIVATE_PARTITION_KEY,
};
pub use store::immutable_supersession::{
    SupersessionClaimOutcome, SupersessionCommitResult, SupersessionError, SupersessionErrorKind,
    SupersessionExpectedState, SupersessionReceipt, SUPERSESSION_RECEIPT_EVENT_TYPE,
    SUPERSESSION_RECEIPT_NAMESPACE, SUPERSESSION_ROUTE_IMMUTABLE_CLAIM,
};
// `DeploymentCapabilities` (the seam's flat bool projection) is deliberately
// NOT re-exported at the crate root: PR-B's catalog row type of the same name
// (model_catalog) owns the root path. The seam type stays reachable as
// `memcore::model_broker_seam::DeploymentCapabilities` — the merge ruling the
// #1757 vendoring note deferred to this merge.
pub use namespace::{
    is_anchor_entry, is_continuity_projection_entry, is_continuity_projection_path, is_eval_entry,
    is_handoff_entry, is_internal_only_row, is_kanban_entry, is_namespace_search_noise,
    is_non_default_retrievable_wiki_row, is_recall_cache_entry, is_reserved_wiki_rem_id,
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
    RecallCoverageExpectedIdLane, RecallCoverageFactEvidence, RecallCoverageFilterReason,
    RecallCoverageMetrics, RecallCoverageOptions, RecallCoverageOutcome,
    RecallCoveragePartitionCounts, RecallCoveragePriorScoredCountSplit, RecallCoverageQuerySource,
    RecallCoverageReport, RecallCoverageTarget, DEFAULT_RECALL_COVERAGE_CANDIDATES_PER_CHANNEL,
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
/// tachi#1643 single-transaction commit boundary. Re-exported at the root for
/// the same reason as the snapshot-import contract below.
pub use store::outbox::{outbox_payload_digest, OutboxCommitReceipt, OutboxEventMeta};
/// tachi#1718 portable destination-side outbox apply/readback boundary.
pub use store::outbox_destination_apply::{
    OutboxDestinationApplyApplication, OutboxDestinationApplyEnvelope,
    OutboxDestinationApplyReceipt, OutboxDestinationApplyResult, OutboxDestinationConflictReason,
    OutboxDestinationConflictReceipt, OutboxDestinationIdentity,
};
/// tachi#1644 outbox reconciliation protocol (#1630 A2). Ungated for the same
/// reason A1 is: a host-owned sync loop drives this from a portable build,
/// with no Tachi daemon in the picture.
pub use store::outbox_protocol::{
    outbox_local_wins_successor_id, ClaimedOutboxEvent, OutboxClaimKind, OutboxClaimRequest,
    OutboxConflictResolution, OutboxConflictResolutionReceipt, OutboxOutcome,
    OutboxOutcomeApplication, OutboxOutcomeEvidence, OutboxOutcomeReceipt,
    OUTBOX_LOCAL_WINS_RESOLVED_CLASS, OUTBOX_LOCAL_WINS_SUCCESSOR_SUFFIX,
};
/// tachi#1607 portable snapshot-import contract. Re-exported at the root so
/// an external portable consumer reaches it exactly like [`MemoryEntry`],
/// without importing the internal `store::` module layout.
pub use store::snapshot_import::{
    DanglingSupersession, PortableImportEntry, PortableImportReceipt,
};
/// tachi#1680 D4 apply report. Re-exported beside the plan types it describes.
#[cfg(feature = "admin")]
pub use store::vault_accounts::{AccountApplyReport, AccountRevision, AliasRef};
pub use types::{
    AuthorityLevel, ContinuityCandidate, ContinuityCandidateBatch, ContinuityMetrics,
    ContinuityOutcomeLabel, EffectScope, ExpectedMemoryState, GcConfig, GraphExpandResult,
    GraphInjectionProvenance, HybridScore, MemoryEdge, MemoryEntry, MetricCount,
    OutcomeEvidenceBasis, ProjectionKind, RetentionPolicy, SearchResult, SessionOutcomeKind,
    SessionOutcomeMetrics, StatsResult, TachiEventQuery, TachiEventRecord,
};
#[cfg(feature = "admin")]
pub use vault::accounts::{
    mint_account_id, mint_auth_ref, names_rotation_pool_member, AccountClass, AccountCustody,
    AuthMode, CustodyKind, CustodyResolution, NewProviderAccount, NewProviderAccountEvent,
    ProviderAccount, ProviderAccountAlias, ProviderAccountEvent,
};
/// tachi#1680 D4 bound reconcile plan: what `apply` consumes and the digest
/// that binds it. Re-exported at the root so the reconcile pipeline reaches it
/// without importing the internal `vault::` module layout.
#[cfg(feature = "admin")]
pub use vault::apply::{
    plan_digest, AccountAction, AccountBinding, AliasSighting, BoundAccountPlan, CustodyBinding,
    MergeConfirmation, NoPlanSources, PlanBindings, PlanSourceDigests, PlannedAccount,
    SourceBinding, VaultEntryBinding, VaultPoolBinding, PLAN_DIGEST_SCHEME,
};
#[cfg(feature = "admin")]
pub use vault::fingerprint::{account_fingerprint_class, AccountFingerprintClass, FingerprintKey};
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
    ///
    /// tachi#1579: **stamp-derived**. This is the role resolved from the
    /// write-once `store_identity` rows inside the database file, not the
    /// `db_label` argument a caller passed to `open_with_label` — that argument
    /// is now a *claim* that the open verifies against the stamp and refuses on
    /// conflict. Identity therefore travels with the bytes and cannot be forged
    /// by moving the file into a differently-named directory.
    pub(crate) db_label: String,
    /// This store's effective schema profile (#1585): whether it carries the
    /// Tachi product tables or only the portable memory kernel. Read from the
    /// store's own profile stamp at open — never from the caller's
    /// `DbOpenContext::required_profile`, which is an admission check only.
    pub(crate) profile: db::StoreProfile,
    /// Whether path validation is enforced for this store. Disabled when
    /// db_label is unknown to avoid breaking unlabeled callers.
    pub(crate) path_validation: bool,
    /// Physical identity captured immediately after this connection opened.
    /// Long-lived runtimes use it to fail closed if the path is later replaced
    /// while SQLite still holds the original file descriptor.
    pub(crate) opened_physical_db_identity: Option<String>,
    /// Host-injected recall/decay/embed configuration (tachi#1585 D5). Every
    /// constructor sets this to [`KernelPolicy::default()`] (pure, no env) —
    /// see [`Self::with_kernel_policy`] for how a caller attaches a
    /// non-default policy after opening.
    pub(crate) policy: KernelPolicy,
    /// tachi#1668: set only when this handle was opened through
    /// [`PrivatePartition`]. Generic opens leave it `None`.
    pub(crate) admitted_partition: Option<crate::private_partition::AdmittedPartition>,
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
