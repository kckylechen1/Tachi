// lib.rs — Public API for memory-core
//
// Re-exports all primary types and provides a MemoryStore handle that
// bundles a rusqlite::Connection with convenience methods.

pub mod agent_profile;
pub mod db;
pub mod error;
pub mod foundry;
pub mod hub;
pub mod namespace;
pub mod noise;
pub mod pack;
pub mod path_router;
pub mod recall_config;
pub mod scorer;
pub mod search;
pub mod store;
pub mod types;
pub mod vault;

pub use agent_profile::{
    AgentProfileIdentity, AgentProfilePack, AgentProfileRule, AgentProfileSource,
    RenderedAgentProfile, AGENT_PROFILE_PACK_SCHEMA_VERSION,
};
pub use db::foundry_config::{get_foundry_config, set_foundry_config, PerDbConfig};
pub use db::foundry_jobs::{
    claim_foundry_job_for_run, find_foundry_jobs_for_memory, gc_foundry_jobs, insert_foundry_job,
    job_status_histogram, load_pending_foundry_jobs, requeue_retryable_foundry_jobs,
    update_foundry_job_status_with_reason, FoundryJobLease, FoundryJobSummary, FoundryRetryPolicy,
    JobStatusHistogram, PersistedFoundryJob, RequeueOutcome,
};
pub use db::row_to_entry;
pub use error::MemoryError;
pub use foundry::{
    AgentEvolutionProposal, AgentEvolutionSynthesis, AgentProfileDocument,
    AgentProfileDocumentKind, FoundryEvidence, FoundryEvidenceKind, FoundryJobKind, FoundryJobSpec,
    FoundryJobStatus, FoundryModelLane,
};
pub use hub::{HubCapability, VirtualCapabilityBinding};
pub use namespace::{
    is_eval_entry, is_handoff_entry, is_kanban_entry, is_namespace_search_noise,
    is_recall_cache_entry, is_wiki_entry, path_contains_recall_cache, path_in_namespace,
    path_prefix_opts_into_recall_cache, FOUNDRY_RECALL_CACHE_SOURCE,
};
pub use noise::{is_noise_text, should_skip_query};
pub use pack::{AgentKind, AgentProjection, Pack, PackAssetRef, PackManifest, PackOverlay};
pub use recall_config::RecallConfig;
pub use scorer::{generic_precision_multiplier, surprise_score, HybridWeights, PrecisionMatcher};
pub use search::{hybrid_search, SearchOptions};
pub use types::{
    AuthorityLevel, ContinuityCandidate, ContinuityCandidateBatch, ContinuityMetrics,
    ContinuityOutcomeLabel, DomainConfig, EffectScope, GcConfig, GraphExpandResult, HybridScore,
    MemoryEdge, MemoryEntry, MetricCount, OutcomeEvidenceBasis, ProjectionKind, RetentionPolicy,
    SearchResult, SessionOutcomeKind, SessionOutcomeMetrics, StatsResult, TachiEventQuery,
    TachiEventRecord,
};
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
