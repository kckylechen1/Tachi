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

/// Tools whose results can be cached (read-only, no side effects)
pub(crate) const CACHEABLE_TOOLS: &[&str] = &[
    "section_build",
    "recommend_capability",
    "recommend_skill",
    "recommend_toolchain",
    "prepare_capability_bundle",
    "tachi_task_brief",
    "tachi_wiki_search",
    "search_memory",
    "find_similar_memory",
    "get_memory",
    "memory_graph",
    "list_memories",
    "memory_stats",
    "tachi_doctor_scan",
    "get_state",
    "hub_discover",
    "hub_get",
    "hub_stats",
    "list_agent_evolution_proposals",
    "vc_list",
    "vc_resolve",
    "get_pipeline_status",
    "list_domains",
    "get_domain",
    "wiki_search",
    // Facade tools (read-only)
    "tachi_search",
    "tachi_web_search",
    "tachi_browse",
];

/// Tools that invalidate the cache (write operations)
pub(crate) const CACHE_INVALIDATING_TOOLS: &[&str] = &[
    "save_memory",
    "remember",
    "extract_facts",
    "ingest",
    "ingest_event",
    "ingest_source",
    "set_state",
    "hub_register",
    "hub_quick_add",
    "hub_review",
    "hub_set_active_version",
    "hub_export_skills",
    "skill_evolve",
    "capture_session",
    "archive_memory",
    "compact_rollup",
    "compact_session_memory",
    "sync_memories",
    "synthesize_agent_evolution",
    "queue_agent_evolution",
    "review_agent_evolution_proposal",
    "project_agent_profile",
    "vc_register",
    "vc_bind",
    "hub_feedback",
    "sandbox_set_rule",
    "sandbox_set_policy",
    "tachi_init_project_db",
    "handoff_leave",
    "handoff_check",
    "post_card",
    "update_card",
    "pack_register",
    "pack_remove",
    "pack_project",
    "register_domain",
    "delete_domain",
    "distill_trajectory",
    "tachi_unstick",
    "wiki_lint",
    "tachi_wiki_write",
    "tachi_wiki_ingest",
    // Facade tools (write / mixed)
    "tachi_save",
    "tachi_memory",
    "tachi_domain_adapter",
    "tachi_handoff",
    "tachi_complete",
    "tachi_orchestrator",
    "tachi_task",
    "tachi_wiki",
    "tachi_skill",
    "tachi_arena",
    "tachi_verify",
    "tachi_shell",
];

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
