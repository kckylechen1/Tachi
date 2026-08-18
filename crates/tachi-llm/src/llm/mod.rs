// llm.rs — LLM, embedding, and provider-health client for Tachi
//
// Uses raw reqwest for OpenAI-compatible chat completions.
// SiliconFlow/Qwen still gets `enable_thinking: false` to avoid empty content.

use std::path::PathBuf;
use std::sync::atomic::AtomicUsize;
use std::sync::{Arc, Mutex, MutexGuard, RwLock};

mod auth_probe;
/// tachi#1681 D3/D7 PR-B: env-chain → catalog import. Public because the
/// status projection (tachi-server) and the #1685 consumer cutover both
/// consume the projection; nothing in this crate reads the catalog back.
pub mod catalog_import;
mod chat_lanes;
mod circuit_breaker;
mod embedding;
/// tachi#1681 D3: the guarded escape hatch for the embedding model —
/// an override must declare its output dimension, and a declaration that
/// disagrees with the stored index is refused at resolution.
pub mod embedding_config;
mod helpers;
pub mod ingress_gate;
mod provider_health;
mod rerank;

pub use auth_probe::{
    auth_probe_descriptor_for_host, auth_probe_descriptor_for_provider_kind,
    ProviderProbeDescriptor, AUTH_PROBE_DESCRIPTORS, DEEPSEEK_AUTH_PROBE, SILICONFLOW_AUTH_PROBE,
    ZAI_AUTH_PROBE, ZAI_BIGMODEL_AUTH_PROBE,
};
pub use chat_lanes::ReasoningOutcome;
pub(crate) use circuit_breaker::{CircuitBreakerRegistry, LaneOutageTracker};
pub use embedding::voyage_embeddings_endpoint;
pub use provider_health::ProviderSecret;
pub use provider_health::{
    ChatLaneConfig, CompletionStatusV1, DeploymentHealthRecordCounts, Generated,
    LaneFallbackConfig, ModelEngineKindV1, ModelInvocationLaneV1,
    PersistedModelInvocationReceiptV1, ProviderAuthProbeClass, ProviderAuthProbeFamily,
    ProviderAuthProbeResult, ProviderInvocationFailure, ProviderInvocationFailureClass,
    ProviderInvocationOutcome, ProviderInvocationReceipt, ProviderRuntimeConfig,
    LLM_OUTPUT_TRUNCATED, MODEL_INVOCATION_SCHEMA_V1,
};
use provider_health::{
    ClaudeCliFailure, DeploymentHealthCounters, ProviderHealthPersistState,
    ProviderHealthReloadState, ProviderState,
};
pub use rerank::{
    RerankConfig, RerankProviderKind, RERANK_LOCAL_ENDPOINT_ENV, RERANK_PROVIDER_ENV,
    RERANK_VOYAGE_ENDPOINT_ENV,
};

/// Stable phase cause for a provider-health write whose own SQLite busy
/// budget expired. Doctor maps this to a terminal timeout receipt only after
/// joining the blocking writer.
pub const PROVIDER_HEALTH_PERSIST_SQLITE_DEADLINE_CAUSE: &str =
    "provider_health_persist_sqlite_deadline";
/// Stable phase cause when Tokio cancels a persistence task before its
/// blocking closure begins. A cancelled task is never permission to advance
/// the next write-capable owner.
pub const PROVIDER_HEALTH_PERSIST_CANCELLED_CAUSE: &str = "provider_health_persist_cancelled";

/// LLM and embedding client using Voyage API for embeddings
/// and lane-specific OpenAI-compatible chat providers.
#[derive(Clone)]
pub struct LlmClient {
    /// Shared pooled HTTP client. Behind an `RwLock` so the recall path can
    /// rebuild it after a run of consecutive timeouts (#926): a poisoned
    /// keep-alive connection to a blackholed provider IP + a stale DNS answer
    /// would otherwise survive indefinitely in the connection pool.
    http: Arc<RwLock<reqwest::Client>>,
    /// Consecutive recall-path (embed/rerank) provider timeouts. Reset on any
    /// success; once it reaches `POOL_TIMEOUT_REBUILD_THRESHOLD` the pooled
    /// client is rebuilt and this resets to zero. Shared across clones.
    http_timeout_streak: Arc<AtomicUsize>,
    extract: ChatLaneConfig,
    distill: ChatLaneConfig,
    reasoning: ChatLaneConfig,
    summary: ChatLaneConfig,
    /// Cross-provider fallback config per lane (#1197). `None` = primary-only,
    /// identical to pre-#1197 behavior.
    extract_fallback: Option<ChatLaneConfig>,
    distill_fallback: Option<ChatLaneConfig>,
    reasoning_fallback: Option<ChatLaneConfig>,
    summary_fallback: Option<ChatLaneConfig>,
    /// Rerank provider config resolved at construction (eager fail-closed).
    rerank_config: RerankConfig,
    vault_db_path: Option<PathBuf>,
    vault_db_migration: memcore::MigrationAuthority,
    provider_state: Arc<RwLock<ProviderState>>,
    /// Serializes a complete provider materialization transaction across
    /// clones. It is deliberately separate from `provider_state` so refreshes
    /// never hold its read/write lock while consulting env or Vault inputs.
    provider_materialization_lock: Arc<Mutex<()>>,
    provider_health_reload: Arc<RwLock<ProviderHealthReloadState>>,
    provider_health_persist: Arc<RwLock<ProviderHealthPersistState>>,
    /// Counts for the deployment-health seam (#1681 D4, PR-C). Not an
    /// `RwLock`: nothing reads these to decide anything, so atomics are the
    /// whole state — and a health counter must never be able to contend with
    /// the invocation path it hangs off.
    deployment_health: Arc<DeploymentHealthCounters>,
    claude_cli_failure: Arc<RwLock<Option<ClaudeCliFailure>>>,
    pub(crate) circuit_breakers: CircuitBreakerRegistry,
    /// Full-chain (all tiers) outage streak per lane (#1197) — feeds
    /// `provider_health_status().lane_outages`.
    pub(crate) lane_outage: LaneOutageTracker,
    /// Counts model references that reached the provider call without having
    /// been resolved (#1681 PR-D debt (a)). Report-only: it changes no
    /// routing, and it is the evidence #1685's cutover will be measured
    /// against. Deliberately **not** folded into `ProviderHealthStatus` yet —
    /// that serialized contract would ship a field #1685 immediately reshapes.
    pub(crate) ingress_gate: ingress_gate::IngressReferenceGate,
    /// Test-only: last provider arm entered by `rerank()` (dispatch seam probe).
    #[cfg(test)]
    last_rerank_dispatch: Arc<std::sync::Mutex<Option<RerankProviderKind>>>,
}

impl LlmClient {
    /// Model references that reached a provider call without having been
    /// resolved, per lane (#1681 PR-D debt (a)).
    ///
    /// A report, not a control: nothing branches on this. It is the evidence
    /// for what #1685's cutover has to cover, and its going to zero is the
    /// evidence the cutover is complete.
    pub fn unresolved_model_references(&self) -> Vec<ingress_gate::UnresolvedReferenceReport> {
        self.ingress_gate.snapshot()
    }

    /// Sightings that arrived after the gate's distinct-reference cap. Nonzero
    /// means [`Self::unresolved_model_references`] is a sample rather than the
    /// whole list.
    pub fn unresolved_model_reference_overflow(&self) -> u64 {
        self.ingress_gate.overflow()
    }

    /// Record which rerank arm `rerank()` actually entered (test discrimination).
    #[inline]
    fn note_rerank_dispatch(&self, kind: RerankProviderKind) {
        #[cfg(test)]
        {
            if let Ok(mut slot) = self.last_rerank_dispatch.lock() {
                *slot = Some(kind);
            }
        }
        #[cfg(not(test))]
        {
            let _ = kind;
        }
    }

    /// Last provider arm entered by `rerank()`. Test discrimination only.
    #[cfg(test)]
    pub fn last_rerank_dispatch_for_tests(&self) -> Option<RerankProviderKind> {
        self.last_rerank_dispatch.lock().ok().and_then(|slot| *slot)
    }

    /// Configured rerank provider (resolved at construction).
    pub fn rerank_config(&self) -> &RerankConfig {
        &self.rerank_config
    }

    /// The lane configuration this client is **actually running on**.
    ///
    /// Reassembled from the client's own fields rather than re-read from env,
    /// which is the whole point: a caller that calls
    /// `ProviderRuntimeConfig::from_env()` a second time gets *a* config, not
    /// *this client's* config, and the two differ exactly where it matters —
    /// a client built through [`Self::new_with_config`] (every injected-config
    /// caller, and every test) would be described by somebody else's process
    /// environment. `catalog_import`'s deployment rows are a projection of
    /// this value, so "the catalog equals the env resolution" is a statement
    /// about the running client instead of a tautology about two calls to the
    /// same env reader.
    ///
    /// Cross-provider fallbacks (#1197) are deliberately not included:
    /// `ProviderRuntimeConfig` does not model them, and a fallback lane is a
    /// separate deployment question that #1681 D2's alias governance owns.
    pub fn runtime_config(&self) -> ProviderRuntimeConfig {
        ProviderRuntimeConfig {
            extract: self.extract.clone(),
            summary: self.summary.clone(),
            reasoning: self.reasoning.clone(),
            distill: self.distill.clone(),
            rerank: self.rerank_config.clone(),
        }
    }

    pub(crate) fn provider_materialization_guard(&self) -> Result<MutexGuard<'_, ()>, String> {
        self.provider_materialization_lock
            .lock()
            .map_err(|_| {
                "Provider materialization transaction lock is poisoned; refusing provider cache mutation"
                    .to_string()
            })
    }

    #[cfg(any(test, feature = "test-support"))]
    #[doc(hidden)]
    pub fn provider_materialization_lock_is_held_for_tests(&self) -> bool {
        self.provider_materialization_lock.try_lock().is_err()
    }

    #[cfg(any(test, feature = "test-support"))]
    #[doc(hidden)]
    pub fn poison_provider_materialization_lock_for_tests(&self) {
        let lock = Arc::clone(&self.provider_materialization_lock);
        let result = std::thread::spawn(move || {
            let _guard = lock.lock().expect("materialization lock starts healthy");
            panic!("poison provider materialization lock for discrimination");
        })
        .join();
        assert!(result.is_err(), "poisoning thread must panic");
    }
}

#[cfg(test)]
mod tests;
