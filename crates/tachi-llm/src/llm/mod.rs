// llm.rs — LLM, embedding, and provider-health client for Tachi
//
// Uses raw reqwest for OpenAI-compatible chat completions.
// SiliconFlow/Qwen still gets `enable_thinking: false` to avoid empty content.

use std::path::PathBuf;
use std::sync::atomic::AtomicUsize;
use std::sync::{Arc, RwLock};

mod chat_lanes;
mod circuit_breaker;
mod embedding;
mod helpers;
mod provider_health;
mod rerank;

pub(crate) use circuit_breaker::CircuitBreakerRegistry;
pub use provider_health::ProviderSecret;
pub use provider_health::{ChatLaneConfig, ProviderRuntimeConfig};
use provider_health::{
    ClaudeCliFailure, ProviderHealthPersistState, ProviderHealthReloadState, ProviderState,
};
pub use rerank::{
    RerankConfig, RerankProviderKind, RERANK_LOCAL_ENDPOINT_ENV, RERANK_PROVIDER_ENV,
    RERANK_VOYAGE_ENDPOINT_ENV,
};

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
    /// Rerank provider config resolved at construction (eager fail-closed).
    rerank_config: RerankConfig,
    vault_db_path: Option<PathBuf>,
    provider_state: Arc<RwLock<ProviderState>>,
    provider_health_reload: Arc<RwLock<ProviderHealthReloadState>>,
    provider_health_persist: Arc<RwLock<ProviderHealthPersistState>>,
    claude_cli_failure: Arc<RwLock<Option<ClaudeCliFailure>>>,
    pub(crate) circuit_breakers: CircuitBreakerRegistry,
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
}

#[cfg(test)]
mod tests;
