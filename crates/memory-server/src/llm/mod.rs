// llm.rs — LLM & Embedding client for memory server
//
// Uses raw reqwest for OpenAI-compatible chat completions.
// SiliconFlow/Qwen still gets `enable_thinking: false` to avoid empty content.

use std::path::PathBuf;
use std::sync::{Arc, RwLock};

mod chat_lanes;
mod embedding;
mod helpers;
mod provider_health;

pub(crate) use provider_health::ProviderSecret;
use provider_health::{
    ChatLaneConfig, ClaudeCliFailure, ProviderHealthPersistState, ProviderHealthReloadState,
    ProviderState,
};

/// LLM and embedding client using Voyage API for embeddings
/// and lane-specific OpenAI-compatible chat providers.
#[derive(Clone)]
pub struct LlmClient {
    http: reqwest::Client,
    extract: ChatLaneConfig,
    distill: ChatLaneConfig,
    reasoning: ChatLaneConfig,
    summary: ChatLaneConfig,
    vault_db_path: Option<PathBuf>,
    provider_state: Arc<RwLock<ProviderState>>,
    provider_health_reload: Arc<RwLock<ProviderHealthReloadState>>,
    provider_health_persist: Arc<RwLock<ProviderHealthPersistState>>,
    claude_cli_failure: Arc<RwLock<Option<ClaudeCliFailure>>>,
}

#[cfg(test)]
mod tests;
