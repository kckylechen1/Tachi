use super::cache::ToolDiscovery;
use super::read_pool::ReadStorePool;
use super::runtime::{
    AgentRuntime, EnrichmentRuntime, FoundryRuntime, ProjectDbState, RateLimiter, VaultState,
};
use crate::claude_pool;
use crate::llm;
use crate::mcp_pool::McpClientPool;
use memory_core::MemoryStore;
use rmcp::handler::server::tool::ToolRouter;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex as StdMutex, RwLock as StdRwLock};

#[derive(Clone)]
pub(crate) struct MemoryServer {
    pub(crate) global_store: Arc<StdMutex<MemoryStore>>,
    pub(crate) global_read_pool: ReadStorePool,
    pub(crate) project_store: Option<Arc<StdMutex<MemoryStore>>>,
    pub(crate) project_read_pool: Option<ReadStorePool>,
    /// Read/write gate for global DB access. Read operations share the lock,
    /// write operations take exclusive lock.
    pub(crate) global_rw_gate: Arc<StdRwLock<()>>,
    /// Read/write gate for project DB access.
    pub(crate) project_rw_gate: Option<Arc<StdRwLock<()>>>,
    pub(crate) global_db_path: Arc<PathBuf>,
    pub(crate) project_db_path: Option<Arc<PathBuf>>,
    pub(crate) global_vec_available: bool,
    pub(crate) project_vec_available: bool,
    /// Hot-swappable project DB state — allows `tachi_init_project_db` to activate
    /// a project database on a running daemon without restart.
    pub(crate) hot_project_db: Arc<StdRwLock<Option<ProjectDbState>>>,
    pub(crate) llm: Arc<llm::LlmClient>,
    /// Bounded Claude CLI pool used by the daily batch distill (Phase 1).
    /// Falls back to LlmClient on call errors — see
    /// `foundry_runtime_ops::maintenance::run_daily_batch_distill`.
    pub(crate) claude_pool: Arc<claude_pool::ClaudePool>,
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
    // ─── Vault ACL Runtime ───────────────────────────────────────────────────
    /// Server-bound vault identity read once from `TACHI_AGENT_ID` at startup.
    pub(crate) bound_agent_id: Arc<StdRwLock<Option<String>>>,
    pub(crate) named_project_cache: Arc<StdMutex<HashMap<String, Arc<StdMutex<MemoryStore>>>>>,
}
