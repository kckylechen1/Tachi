use crate::enrichment::EnrichmentItem;
use crate::foundry_runtime_ops::{FoundryMaintenanceItem, FoundryWorkerStats};
use memory_server_runtime::AgentProfile;
use std::sync::Arc;
use tachi_hub::ToolProfile;
use tokio::sync::mpsc;

#[derive(Debug, Clone)]
pub(crate) struct AgentRuntime {
    pub(crate) agent_profile: Option<AgentProfile>,
    pub(crate) tool_profile: Option<ToolProfile>,
    pub(crate) session_client: Option<String>,
    pub(crate) session_project: Option<String>,
    pub(crate) work_claim_connection: Option<WorkClaimConnection>,
    /// #1251: the raw dispatch recursion-depth marker for THIS session, as it
    /// arrived over the wire (`HEADER_DISPATCH_DEPTH` in the daemon path, or
    /// the process's own `ENV_DISPATCH_DEPTH` in the CLI in-process path).
    /// Stored raw (not pre-parsed) so the single resolve/saturate decision
    /// lives at the gate in `session_identity::resolve_dispatch_depth`; `None`
    /// means "no marker seen" ≡ depth 0 (a leader session).
    pub(crate) session_dispatch_depth: Option<String>,
    /// #1255: opaque rate-limit identity for this MCP session. Shared
    /// `RateLimiter` state keys burst/RPM windows by this id; stamped fresh on
    /// each `clone_for_mcp_session` so sessions do not inherit each other's
    /// burst counters. Not a client-facing session token.
    pub(crate) rate_limit_session_id: String,
    // #1099: `handoff_memos` (in-memory duplicate of the persisted
    // `handoff:<id>` store rows, LRU-capped, populated only by the retired
    // `handoff_leave`/`handoff_check` handlers) removed — it was the
    // "second memo lifecycle" the #1099 acceptance criteria call out;
    // nothing wrote or read it once those handlers went away.
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct WorkClaimConnection {
    pub(crate) agent_identity_id: Option<String>,
    pub(crate) connection_id: String,
    pub(crate) admission: String,
}

/// Bounded channel capacity for enrichment batcher
pub(super) const ENRICH_CHANNEL_CAPACITY: usize = 512;
/// Bounded channel capacity for foundry maintenance worker
pub(super) const FOUNDRY_CHANNEL_CAPACITY: usize = 256;

#[derive(Clone)]
pub(crate) struct EnrichmentRuntime {
    pub(crate) enrich_tx: mpsc::Sender<EnrichmentItem>,
    #[cfg(test)]
    pub(crate) retained_enrich_rx: Arc<std::sync::Mutex<Option<mpsc::Receiver<EnrichmentItem>>>>,
}

#[derive(Clone)]
pub(crate) struct FoundryRuntime {
    pub(crate) foundry_tx: mpsc::Sender<FoundryMaintenanceItem>,
    pub(crate) foundry_stats: Arc<FoundryWorkerStats>,
}
