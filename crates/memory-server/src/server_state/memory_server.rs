use super::cache::ToolDiscovery;
use super::runtime::{AgentRuntime, EnrichmentRuntime, FoundryRuntime};
use super::{DbRuntime, RateLimiter, VaultState};
use crate::mcp_pool::McpClientPool;
use rmcp::handler::server::tool::ToolRouter;
use std::sync::{Arc, Mutex as StdMutex, RwLock as StdRwLock};

#[derive(Clone)]
pub(crate) struct MemoryServer {
    pub(crate) db: DbRuntime,
    pub(crate) llm: Arc<tachi_llm::LlmClient>,
    /// Bounded Claude CLI pool used by the daily batch distill (Phase 1).
    /// Falls back to LlmClient on call errors — see
    /// `foundry_runtime_ops::maintenance::run_daily_batch_distill`.
    pub(crate) claude_pool: Arc<tachi_llm::claude_pool::ClaudePool>,
    pub(crate) pipeline_enabled: bool,
    /// Cached proxy tools from registered MCP servers: server_id → Vec<Tool>
    pub(crate) tool_discovery: Arc<ToolDiscovery>,
    pub(crate) pool: Arc<McpClientPool>,
    pub(crate) tool_router: ToolRouter<Self>,
    // ─── Phantom Tools (result caching — lock-free counters) ──────────────
    pub(crate) cache_hits: Arc<std::sync::atomic::AtomicU64>,
    pub(crate) cache_misses: Arc<std::sync::atomic::AtomicU64>,
    // ─── Idle reaper clock ────────────────────────────────────────────────────
    /// Unix-millis timestamp of the last MCP tool call. Bumped centrally in
    /// `ServerHandler::call_tool` (so forwarded calls from stdio children count
    /// too). Drives the daemon idle-timeout: a detached daemon with no recent
    /// activity exits itself instead of lingering forever.
    pub(crate) last_activity_ms: Arc<std::sync::atomic::AtomicI64>,
    // ─── Enrichment Batcher ──────────────────────────────────────────────────
    pub(crate) enrichment: EnrichmentRuntime,
    // ─── Foundry Maintenance Worker ──────────────────────────────────────────
    pub(crate) foundry: FoundryRuntime,
    // ─── Vault (Encrypted Secret Storage) ────────────────────────────────────
    pub(crate) vault: Arc<StdRwLock<VaultState>>,
    // ─── Rate Limiter ────────────────────────────────────────────────────────
    pub(crate) rate_limiter: Arc<StdMutex<RateLimiter>>,
    // ─── Agent Runtime ───────────────────────────────────────────────────────
    /// Agent profile, tool profile, and handoff memos grouped together.
    pub(crate) agent_runtime: Arc<StdRwLock<AgentRuntime>>,
}
