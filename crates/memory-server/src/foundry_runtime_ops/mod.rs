use super::*;
use serde::{Deserialize, Serialize};
use std::sync::atomic::AtomicU64;

mod capture;
mod daily_distill;
mod handlers;
mod helpers;
mod maintenance;
mod recall;
mod recall_cache;

pub(crate) use recall::rerank_rows;

#[cfg(test)]
mod tests;

const CAPTURE_DEDUP_THRESHOLD: f64 = 0.95;
const CAPTURE_MERGE_THRESHOLD: f64 = 0.85;
const FOUNDRY_DISTILL_SOURCE: &str = "foundry_distill";
const FOUNDRY_RECALL_RERANK_CACHE_SOURCE: &str = "foundry_recall_rerank_cache";
const FOUNDRY_RELATED_LIMIT: usize = 4;
#[allow(dead_code)] // Phase 1: only used by legacy schedule_pending_distill_jobs fallback.
const FOUNDRY_DISTILL_WINDOW: usize = 8;
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
    pub job: memory_core::FoundryJobSpec,
    pub target_db: DbScope,
    pub named_project: Option<String>,
    pub db_path: Option<std::path::PathBuf>,
    pub path_prefix: String,
    pub memory_ids: Vec<String>,
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

#[derive(Debug, Clone, Serialize)]
struct SectionArtifact {
    section_id: String,
    layer: String,
    kind: String,
    title: Option<String>,
    cache_boundary: String,
    estimated_tokens: usize,
    item_count: usize,
    source_refs: Vec<String>,
    block: String,
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
#[allow(unused_imports)]
pub(crate) use daily_distill::DistillBatchReport;
pub(crate) use handlers::{
    handle_capture_session, handle_compact_context, handle_compact_rollup,
    handle_compact_session_memory, handle_recall_context, handle_section_build,
};
pub(crate) use maintenance::{enqueue_foundry_capture_maintenance, run_foundry_maintenance_worker};
// Phase 1: legacy 30-minute per-capture distill scheduler kept as a
// manual fallback. The bootstrap loop now drives
// `run_daily_batch_distill` instead, but operators can still call this
// directly during incident recovery.
#[allow(unused_imports)]
pub(crate) use maintenance::schedule_pending_distill_jobs;
