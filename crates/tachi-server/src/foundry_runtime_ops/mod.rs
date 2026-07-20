use crate::server_state::DbScope;
use serde::Deserialize;
use std::sync::atomic::AtomicU64;

mod capture;
mod daily_distill;
mod handlers;
pub(crate) use handlers::{CAPTURE_EPHEMERAL_TTL_DAYS, CAPTURE_RETENTION_POLICY_VERSION};
mod helpers;
mod maintenance;
mod recall;
mod recall_cache;
pub(crate) mod wiki_evolver;

pub(crate) use tachi_foundry::FOUNDRY_DISTILL_SOURCE;

#[cfg(test)]
mod tests;

const CAPTURE_DEDUP_THRESHOLD: f64 = 0.95;
const CAPTURE_MERGE_THRESHOLD: f64 = 0.85;
const FOUNDRY_RECALL_RERANK_CACHE_SOURCE: &str = "foundry_recall_rerank_cache";
const FOUNDRY_RELATED_LIMIT: usize = 4;
const FOUNDRY_DISTILL_KEEP: usize = 6;
const FOUNDRY_RECALL_RERANK_TOP_K: usize = 6;
const FOUNDRY_RECALL_RERANK_CANDIDATE_MULTIPLIER: usize = 3;

#[derive(Debug, Default)]
pub(super) struct FoundryWorkerStats {
    pub queued: AtomicU64,
    pub running: AtomicU64,
    pub completed: AtomicU64,
    pub failed: AtomicU64,
    pub skipped: AtomicU64,
}

#[derive(Debug, Clone)]
pub(crate) struct FoundryMaintenanceItem {
    pub job: memcore::FoundryJobSpec,
    pub target_db: DbScope,
    pub named_project: Option<String>,
    pub db_path: Option<std::path::PathBuf>,
    pub path_prefix: String,
    pub memory_ids: Vec<String>,
    /// True only for the primary enqueue path that incremented
    /// `foundry_stats.queued`. Safety-net replay/scheduler items are sourced
    /// from DB state and must not decrement that in-memory queue counter.
    pub counted_queue_slot: bool,
}

#[derive(Debug, Clone, Deserialize)]
struct SessionCaptureDraft {
    text: String,
    #[serde(default)]
    summary: String,
    #[serde(default)]
    topic: String,
    #[serde(default = "default_capture_category")]
    category: String,
    #[serde(default = "default_capture_scope")]
    scope: String,
    #[serde(default = "default_capture_importance")]
    importance: f64,
    #[serde(default)]
    keywords: Vec<String>,
    #[serde(default)]
    persons: Vec<String>,
    #[serde(default)]
    entities: Vec<String>,
    #[serde(default)]
    location: String,
}

#[derive(Debug, Clone, Deserialize)]
struct CompactContextDraft {
    compacted_text: String,
    #[serde(default)]
    salient_topics: Vec<String>,
    #[serde(default)]
    durable_signals: Vec<String>,
}

#[derive(Debug, Clone)]
struct RecallScope {
    search_prefixes: Vec<Option<String>>,
    allowed_prefixes: Vec<String>,
    warning: Option<String>,
}

fn default_capture_category() -> String {
    "fact".to_string()
}

fn default_capture_scope() -> String {
    "project".to_string()
}

fn default_capture_importance() -> f64 {
    0.3
}

// Re-export items so sibling modules (main.rs etc.) can use them
pub(crate) use daily_distill::run_daily_batch_distill;
pub(crate) use daily_distill::run_daily_batch_distill_with_options;
pub(crate) use daily_distill::scrub_agent_noise;
pub(crate) use handlers::{
    handle_capture_session, handle_compact_context, handle_compact_rollup,
    handle_compact_session_memory, handle_recall_context, handle_section_build,
    COMPACT_CONTEXT_PERSIST_REFUSAL,
};
pub(crate) use maintenance::{enqueue_foundry_capture_maintenance, run_foundry_maintenance_worker};
