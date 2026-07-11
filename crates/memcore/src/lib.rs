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
pub mod error;
#[cfg(feature = "admin")]
pub mod foundry;
#[cfg(feature = "admin")]
pub mod hub;
pub mod namespace;
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

#[cfg(feature = "admin")]
pub use agent_profile::{
    AgentProfileIdentity, AgentProfilePack, AgentProfileRule, AgentProfileSource,
    RenderedAgentProfile, AGENT_PROFILE_PACK_SCHEMA_VERSION,
};
#[cfg(feature = "admin")]
pub use db::exec_env::{
    find_active_exec_env_by_path, get_exec_env, insert_exec_env, list_exec_envs, reclaim_exec_env,
    ExecEnvLease, ExecEnvSelector, ExecEnvState, NewExecEnvLease, ReclaimOutcome,
};
#[cfg(feature = "admin")]
pub use db::foundry_config::{get_foundry_config, set_foundry_config, PerDbConfig};
#[cfg(feature = "admin")]
pub use db::foundry_jobs::{
    claim_foundry_job_for_run, find_foundry_jobs_for_memory, gc_foundry_jobs, insert_foundry_job,
    job_status_histogram, load_pending_foundry_jobs, requeue_retryable_foundry_jobs,
    update_foundry_job_status_with_reason, FoundryJobLease, FoundryJobSummary, FoundryRetryPolicy,
    JobStatusHistogram, PersistedFoundryJob, RequeueOutcome,
};
pub use db::row_to_entry;
pub use db::{CategoryPathPrefixMemoryRow, FoundryJobStatusCounts, PathPrefixMemoryRow};
pub use db::{CategorySourceGroup, DailyHealthDbSnapshot, DuplicateSummaryRow, EvalEvidenceRow};
pub use error::MemoryError;
#[cfg(feature = "admin")]
pub use foundry::{
    AgentEvolutionProposal, AgentEvolutionSynthesis, AgentProfileDocument,
    AgentProfileDocumentKind, FoundryEvidence, FoundryEvidenceKind, FoundryJobKind, FoundryJobSpec,
    FoundryJobStatus, FoundryModelLane,
};
#[cfg(feature = "admin")]
pub use hub::{HubCapability, VirtualCapabilityBinding};
pub use namespace::{
    is_eval_entry, is_handoff_entry, is_kanban_entry, is_namespace_search_noise,
    is_recall_cache_entry, is_wiki_entry, path_contains_recall_cache, path_in_namespace,
    path_prefix_opts_into_recall_cache, FOUNDRY_RECALL_CACHE_SOURCE, RECALL_CACHE_SQL_WHERE,
    RECALL_CACHE_SQL_WHERE_M,
};
pub use noise::{is_noise_text, should_skip_query};
pub use recall_config::RecallConfig;
pub use scorer::{
    generic_precision_multiplier, surprise_score, DecayPolicy, DecayPolicyContext,
    DefaultDecayPolicy, HybridWeights, PrecisionMatcher, DEFAULT_DECAY_POLICY,
};
pub use search::{
    apply_blend_relevance, hybrid_search, merge_rerank_order_with_hybrid_floor, SearchOptions,
    HYBRID_HEAD_FRACTION,
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
