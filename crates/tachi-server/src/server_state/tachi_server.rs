use super::cache::ToolDiscovery;
use super::runtime::{AgentRuntime, EnrichmentRuntime, FoundryRuntime};
use super::{DbRuntime, RateLimiter, VaultState};
use crate::mcp_pool::McpClientPool;
use crate::memory_search_ops::routing_config::RoutingConfigProvider;
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
    // ─── Vault ACL Runtime ───────────────────────────────────────────────────
    /// Server-bound vault identity read once from `TACHI_AGENT_ID` at startup.
    pub(crate) bound_agent_id: Arc<StdRwLock<Option<String>>>,
    // ─── Home Identity ───────────────────────────────────────────────────────
    /// Server-bound Tachi home directory, resolved once at startup via
    /// `path_utils::tachi_home()` (env-precedence chain
    /// `TACHI_HOME` → `SIGIL_HOME` → `TACHI_APP_HOME` → workspace fallback →
    /// `~/.tachi`). Immutable for the process lifetime — no lock needed.
    /// Handler/ops code that runs after `MemoryServer::new` should read this
    /// field via `tachi_home_dir()` instead of re-reading env at call time;
    /// outer CLI/bootstrap entry points that run *before* a server exists
    /// still read env directly (see #1096 leaf-2a).
    ///
    /// **Invariant (#1096 leaf-2a round-2, codex checkpoint-1 / B2):** this
    /// frozen value and a live re-read of `path_utils::tachi_home()` are
    /// equal for the entire lifetime of any real process, because no
    /// production path ever mutates `TACHI_HOME`/`SIGIL_HOME`/`TACHI_APP_HOME`
    /// after constructing a server — every poke/CLI/daemon entry point sets
    /// env BEFORE calling `MemoryServer::new`. That invariant is what makes
    /// the mixed frozen-vs-live split safe across this crate: dispatch's
    /// write side (`dedupe.rs`/`start.rs`) and the enumerate side of `search`
    /// still re-derive the home from env at call time, while the predicate
    /// read side and `search`'s open side read this frozen field — under the
    /// invariant, both give the same answer, so there is no correctness gap
    /// to close by moving the live-read call sites onto this field (that
    /// migration was scoped out of this leaf: 6 functions across 5 files).
    /// The only known way to violate the invariant is test code that
    /// mutates env *after* constructing a server — such tests must set env
    /// first, then construct. See
    /// `memory_server_tachi_home_dir_matches_canonical_resolution` in
    /// `server_state/init.rs`'s test module for the equality check.
    pub(crate) home_dir: Arc<std::path::PathBuf>,
    /// Routing configuration belongs to the same immutable home identity as
    /// this server. Clones share its success-only cache.
    pub(crate) routing_config: Arc<RoutingConfigProvider>,
}
