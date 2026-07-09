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

pub(crate) use circuit_breaker::CircuitBreakerRegistry;
pub use provider_health::ProviderSecret;
use provider_health::{
    ChatLaneConfig, ClaudeCliFailure, ProviderHealthPersistState, ProviderHealthReloadState,
    ProviderState,
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
    vault_db_path: Option<PathBuf>,
    provider_state: Arc<RwLock<ProviderState>>,
    provider_health_reload: Arc<RwLock<ProviderHealthReloadState>>,
    provider_health_persist: Arc<RwLock<ProviderHealthPersistState>>,
    claude_cli_failure: Arc<RwLock<Option<ClaudeCliFailure>>>,
    pub(crate) circuit_breakers: CircuitBreakerRegistry,
}

#[cfg(test)]
mod tests;
