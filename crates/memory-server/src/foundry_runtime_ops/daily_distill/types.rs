use serde::Serialize;

use memory_core::MemoryEntry;

/// Per-batch outcome surfaced to the scheduler/log.
#[derive(Debug, Default, Serialize)]
pub struct DistillBatchReport {
    pub projects_scanned: usize,
    pub batches_dispatched: usize,
    pub groups_distilled: usize,
    pub groups_skipped: usize,
    pub fallback_used: usize,
    pub errors: Vec<String>,
}

#[derive(Debug, Clone)]
pub(crate) struct CandidateGroup {
    pub(crate) group_id: String,
    pub(crate) path_prefix: String,
    pub(crate) coherence_key: String,
    pub(crate) entries: Vec<MemoryEntry>,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct SourceManifestEntry {
    pub(crate) group_id: String,
    pub(crate) path_prefix: String,
    pub(crate) coherence_key: String,
    pub(crate) source_memory_ids: Vec<String>,
    pub(crate) written_memory_id: Option<String>,
    pub(crate) backend: &'static str,
    pub(crate) fallback_used: bool,
    pub(crate) skip_reason: Option<String>,
}

#[derive(Debug, Clone)]
pub(crate) struct GroupPayload {
    pub(crate) summary: String,
    pub(crate) text: String,
    pub(crate) keywords: Vec<String>,
    pub(crate) skip_reason: Option<String>,
}
