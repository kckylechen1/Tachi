use crate::enrichment::EnrichmentItem;
use crate::foundry_runtime_ops::{FoundryMaintenanceItem, FoundryWorkerStats};
use memory_server_runtime::{AgentProfile, HandoffMemo};
use std::sync::Arc;
use tachi_hub::ToolProfile;
use tokio::sync::mpsc;

#[derive(Debug, Clone)]
pub(crate) struct AgentRuntime {
    pub(crate) agent_profile: Option<AgentProfile>,
    pub(crate) tool_profile: Option<ToolProfile>,
    pub(crate) session_client: Option<String>,
    pub(crate) session_project: Option<String>,
    pub(crate) handoff_memos: Vec<HandoffMemo>,
}

/// Bounded channel capacity for enrichment batcher
pub(super) const ENRICH_CHANNEL_CAPACITY: usize = 512;
/// Bounded channel capacity for foundry maintenance worker
pub(super) const FOUNDRY_CHANNEL_CAPACITY: usize = 256;

#[derive(Clone)]
pub(crate) struct EnrichmentRuntime {
    pub(crate) enrich_tx: mpsc::Sender<EnrichmentItem>,
}

#[derive(Clone)]
pub(crate) struct FoundryRuntime {
    pub(crate) foundry_tx: mpsc::Sender<FoundryMaintenanceItem>,
    pub(crate) foundry_stats: Arc<FoundryWorkerStats>,
}
