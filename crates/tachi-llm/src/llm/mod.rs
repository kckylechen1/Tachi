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
mod helpers;
mod provider_health;
mod rerank;

pub use auth_probe::{
    auth_probe_descriptor_for_host, auth_probe_descriptor_for_provider_kind,
    ProviderProbeDescriptor, AUTH_PROBE_DESCRIPTORS, DEEPSEEK_AUTH_PROBE, SILICONFLOW_AUTH_PROBE,
    ZAI_AUTH_PROBE, ZAI_BIGMODEL_AUTH_PROBE,
};
pub use chat_lanes::ReasoningOutcome;
pub(crate) use circuit_breaker::{CircuitBreakerRegistry, LaneOutageTracker};
pub use provider_health::ProviderSecret;
pub use provider_health::{
    ChatLaneConfig, CompletionStatusV1, Generated, LaneFallbackConfig, ModelEngineKindV1,
    ModelInvocationLaneV1, PersistedModelInvocationReceiptV1, ProviderAuthProbeClass,
    ProviderAuthProbeFamily, ProviderAuthProbeResult, ProviderInvocationFailure,
    ProviderInvocationFailureClass, ProviderInvocationOutcome, ProviderInvocationReceipt,
    ProviderRuntimeConfig, LLM_OUTPUT_TRUNCATED, MODEL_INVOCATION_SCHEMA_V1,
};
use provider_health::{
    ClaudeCliFailure, ProviderHealthPersistState, ProviderHealthReloadState, ProviderState,
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
    claude_cli_failure: Arc<RwLock<Option<ClaudeCliFailure>>>,
    pub(crate) circuit_breakers: CircuitBreakerRegistry,
    /// Full-chain (all tiers) outage streak per lane (#1197) — feeds
    /// `provider_health_status().lane_outages`.
    pub(crate) lane_outage: LaneOutageTracker,
    /// Test-only: last provider arm entered by `rerank()` (dispatch seam probe).
    #[cfg(test)]
    last_rerank_dispatch: Arc<std::sync::Mutex<Option<RerankProviderKind>>>,
}

impl LlmClient {
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
