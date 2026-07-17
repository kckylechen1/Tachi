use crate::mcp_proxy::McpToolExposureMode;
use crate::shared_defs::DeadLetter;
use std::collections::{HashMap, VecDeque};
use std::sync::Mutex as StdMutex;
use std::time::{Duration, Instant};

/// TTL for cached tool results (Phantom Tools)
pub(crate) const TOOL_CACHE_TTL: Duration = Duration::from_secs(30);
/// Maximum entries in the tool cache before LRU eviction kicks in
pub(crate) const TOOL_CACHE_MAX_ENTRIES: usize = 256;
pub(super) const DEFAULT_MCP_DISCOVERY_TIMEOUT_MS: u64 = 10_000;

/// #1098: single typed action-effect authority. `CACHEABLE_TOOLS` /
/// `CACHE_INVALIDATING_TOOLS` used to be hand-maintained here independently
/// of the DLQ/retry classification in `shared_defs.rs`; both now read the
/// same source list from `crate::action_effect` (membership unchanged).
pub(crate) use crate::action_effect::{CACHEABLE_TOOLS, CACHE_INVALIDATING_TOOLS};

pub(crate) struct CachedResult {
    pub(crate) result: rmcp::model::CallToolResult,
    pub(crate) created_at: Instant,
}

impl Clone for CachedResult {
    fn clone(&self) -> Self {
        Self {
            result: self.result.clone(),
            created_at: self.created_at,
        }
    }
}

pub(crate) struct ToolDiscovery {
    pub(crate) proxy_tools: StdMutex<HashMap<String, Vec<rmcp::model::Tool>>>,
    pub(crate) skill_tools: StdMutex<HashMap<String, String>>,
    pub(crate) skill_tool_defs: StdMutex<HashMap<String, rmcp::model::Tool>>,
    pub(crate) tool_cache: StdMutex<HashMap<String, CachedResult>>,
    pub(crate) dead_letters: StdMutex<VecDeque<DeadLetter>>,
    pub(crate) mcp_discovery_timeout: Duration,
    pub(crate) mcp_tool_exposure_mode: McpToolExposureMode,
}
