// main.rs — Memory MCP Server
//
// Rust MCP server using rmcp SDK to expose memory-core functionality.
// Stateless design: each tool opens its own DB connection per-request.

#![allow(
    clippy::cast_abs_to_unsigned,
    clippy::cloned_ref_to_slice_refs,
    clippy::cmp_owned,
    clippy::collapsible_if,
    clippy::collapsible_str_replace,
    clippy::derivable_impls,
    clippy::doc_overindented_list_items,
    clippy::enum_variant_names,
    clippy::field_reassign_with_default,
    clippy::if_same_then_else,
    clippy::io_other_error,
    clippy::let_and_return,
    clippy::manual_async_fn,
    clippy::manual_clamp,
    clippy::manual_pattern_char_comparison,
    clippy::manual_strip,
    clippy::needless_range_loop,
    clippy::needless_update,
    clippy::ptr_arg,
    clippy::redundant_closure,
    clippy::too_many_arguments,
    clippy::unnecessary_cast,
    clippy::unnecessary_sort_by,
    clippy::useless_conversion,
    clippy::useless_format
)]

mod bootstrap;
mod builtins;
mod capability_ops;
mod capture_gate;
mod clawdoctor;
mod cli;
mod cli_client;
mod complete_ops;
mod copilot_ops;
mod daemon_lock;
mod daily_pipeline;
mod dispatch_ops;
mod dlq_ops;
mod doctor;
mod doctor_ops;
mod enrichment;
mod facade_memory_ops;
mod facade_save_ops;
mod facade_search_ops;
mod foundry_ops;
mod foundry_runtime_ops;
mod foundry_scheduler;
mod gh_ops;
mod graph_state_ops;
mod handoff_ops;
mod hub_helpers;
mod hub_ops;
mod kanban;
mod llm;
mod manifest;
mod mcp_connection;
mod mcp_pool;
mod mcp_proxy;
mod memory_ops;
mod memory_search_ops;
mod notes_ops;
mod pack_ops;
mod pipeline_ops;
mod profiles;
mod project_db_ops;
mod prompts;
mod provenance;
mod repair;
mod rescue;
mod sandbox_ops;
mod server_handler;
mod server_methods;
mod shared_defs;
mod shell_ops;
mod skill_chain_ops;
mod status_ops;
mod tool_params;
mod tools;
mod utils;
mod vault_crypto;
mod vault_ops;
mod web_search_ops;
mod wiki_ops;

use crate::builtins::seed_builtin_capabilities;
use crate::foundry_runtime_ops::{
    enqueue_foundry_capture_maintenance, run_foundry_maintenance_worker, FoundryMaintenanceItem,
    FoundryWorkerStats,
};
use crate::hub_helpers::{
    build_skill_tool_from_cap, capability_callable, capability_visibility_for_cap,
    make_text_tool_result, review_status_allows_call, should_expose_mcp_tools,
    should_expose_skill_tool, CapabilityVisibility,
};
use crate::kanban::{gc_expired_kanban_cards, DEFAULT_KANBAN_GC_MAX_AGE_DAYS};
use crate::mcp_proxy::{
    append_warning, clear_mcp_discovery_metadata, filter_mcp_tools_by_permissions,
    resolve_mcp_tool_exposure, set_mcp_discovery_failure, set_mcp_discovery_success,
    McpToolExposureMode,
};
use crate::memory_search_ops::{handle_save_memory, search_memory_rows};
use crate::profiles::ToolProfile;
use crate::shared_defs::{
    categorize_error, slim_entry, slim_entry_with_enrichment, slim_l0_rule, slim_search_result,
    DeadLetter, DLQ_MAX_ENTRIES, DLQ_TTL_SECS,
};
use crate::tool_params::*;
use crate::utils::{
    find_git_root, find_project_git_root, is_active_global_rule, is_trusted_command,
    lock_or_recover, parse_env_bool, parse_env_u64, read_or_recover, sanitize_safe_path_name,
    stable_hash, value_to_template_text, write_or_recover,
};
use crate::vault_ops::load_unlocked_api_key_secrets;

use chrono::Utc;
use clap::Parser;
use memory_core::{
    HubCapability, HybridWeights, MemoryEntry, MemoryStore, SearchOptions, VirtualCapabilityBinding,
};
use rmcp::{
    handler::server::{tool::ToolRouter, wrapper::Parameters},
    model::{ServerCapabilities, ServerInfo},
    schemars,
    schemars::JsonSchema,
    transport::StreamableHttpClientTransport,
    ServerHandler,
};
use serde::Deserialize;
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet, VecDeque};
use std::future::Future;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::Mutex as StdMutex;
use std::sync::RwLock as StdRwLock;
use std::time::{Duration, Instant};
use tokio::io::{stdin, stdout};
use tokio::sync::mpsc;

use crate::cli::{Cli, Commands, HubAction, ManifestAction, RescueAction};
use crate::enrichment::EnrichmentItem;
use crate::mcp_pool::McpClientPool;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DbScope {
    Global,
    Project,
}

impl DbScope {
    fn as_str(&self) -> &'static str {
        match self {
            DbScope::Global => "global",
            DbScope::Project => "project",
        }
    }
}

// ─── Server State ─────────────────────────────────────────────────────────────

/// TTL for cached tool results (Phantom Tools)
const TOOL_CACHE_TTL: Duration = Duration::from_secs(30);
/// Maximum entries in the tool cache before LRU eviction kicks in
const TOOL_CACHE_MAX_ENTRIES: usize = 256;
const DEFAULT_MCP_DISCOVERY_TIMEOUT_MS: u64 = 10_000;

// ─── Rate Limiter Constants ──────────────────────────────────────────────────
/// Default requests-per-minute limit per session (0 = unlimited)
const DEFAULT_RATE_LIMIT_RPM: u64 = 0;
/// Default max identical (tool+args) calls within the burst window (0 = unlimited)
const DEFAULT_RATE_LIMIT_BURST: u64 = 8;
/// Burst detection window
const RATE_LIMIT_BURST_WINDOW: Duration = Duration::from_secs(60);
/// Maximum tracked sessions in rate limiter before stale eviction
const RATE_LIMIT_MAX_SESSIONS: usize = 1024;
/// Maximum tracked burst keys in rate limiter before stale eviction
const RATE_LIMIT_MAX_BURST_KEYS: usize = 4096;
/// Soft warning threshold: when a tool+args is repeated this many times within
/// `RATE_LIMIT_BURST_WINDOW`, the call still succeeds but an extra TextContent
/// block is appended to the result advising the agent to call
/// `tachi_progress_check` / `tachi_wiki_search` before continuing. Hard block
/// still kicks in at `effective_burst` (default `DEFAULT_RATE_LIMIT_BURST`).
const STUCK_SOFT_WARN_THRESHOLD: u64 = 3;

// ─── Channel Backpressure ────────────────────────────────────────────────────
/// Bounded channel capacity for enrichment batcher
const ENRICH_CHANNEL_CAPACITY: usize = 512;
/// Bounded channel capacity for foundry maintenance worker
const FOUNDRY_CHANNEL_CAPACITY: usize = 256;

/// Tools whose results can be cached (read-only, no side effects)
const CACHEABLE_TOOLS: &[&str] = &[
    "section_build",
    "recommend_capability",
    "recommend_skill",
    "recommend_toolchain",
    "prepare_capability_bundle",
    "tachi_task_brief",
    "tachi_progress_check",
    "tachi_wiki_search",
    "search_memory",
    "cyberbrain_search",
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
    "wiki_browse",
    // Facade tools (read-only)
    "tachi_search",
    "tachi_memory",
    "tachi_web_search",
    "tachi_plan",
    "tachi_unstick",
    "tachi_browse",
];

/// Tools that invalidate the cache (write operations)
const CACHE_INVALIDATING_TOOLS: &[&str] = &[
    "save_memory",
    "cyberbrain_write",
    "remember",
    "extract_facts",
    "ingest",
    "ingest_event",
    "ingest_source",
    "set_state",
    "hub_register",
    "hub_quick_add",
    "hub_review",
    "section9_review",
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
    "shell_set_policy",
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
    "wiki_lint",
    "tachi_wiki_write",
    "tachi_wiki_ingest",
    // Facade tools (write / mixed)
    "tachi_save",
    "tachi_memory",
    "tachi_handoff",
    "tachi_complete",
    "tachi_task",
    "tachi_wiki",
    "tachi_skill",
    "tachi_shell",
];

struct CachedResult {
    result: rmcp::model::CallToolResult,
    created_at: Instant,
}

impl Clone for CachedResult {
    fn clone(&self) -> Self {
        Self {
            result: self.result.clone(),
            created_at: self.created_at,
        }
    }
}

#[derive(Clone)]
#[allow(dead_code)]
struct ProjectDbState {
    store: Arc<StdMutex<MemoryStore>>,
    rw_gate: Arc<StdRwLock<()>>,
    db_path: Arc<PathBuf>,
    vec_available: bool,
}

/// Agent profile registered via `agent_register`. Stored per-session (in-memory).
#[derive(Debug, Clone, serde::Serialize)]
pub(crate) struct AgentProfile {
    pub(crate) agent_id: String,
    pub(crate) display_name: String,
    pub(crate) capabilities: Vec<String>,
    pub(crate) tool_filter: Option<Vec<String>>,
    pub(crate) rate_limit_rpm: Option<u64>,
    pub(crate) rate_limit_burst: Option<u64>,
    pub(crate) registered_at: String,
}

/// Cross-agent handoff memo — left by one agent for the next.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
struct HandoffMemo {
    id: String,
    from_agent: String,
    target_agent: Option<String>,
    summary: String,
    next_steps: Vec<String>,
    context: Option<serde_json::Value>,
    created_at: String,
    acknowledged: bool,
}

#[derive(Clone)]
#[allow(dead_code)]
struct MemoryServer {
    global_store: Arc<StdMutex<MemoryStore>>,
    project_store: Option<Arc<StdMutex<MemoryStore>>>,
    /// Read/write gate for global DB access. Read operations share the lock,
    /// write operations take exclusive lock.
    global_rw_gate: Arc<StdRwLock<()>>,
    /// Read/write gate for project DB access.
    project_rw_gate: Option<Arc<StdRwLock<()>>>,
    global_db_path: Arc<PathBuf>,
    project_db_path: Option<Arc<PathBuf>>,
    global_vec_available: bool,
    project_vec_available: bool,
    /// Hot-swappable project DB state — allows `tachi_init_project_db` to activate
    /// a project database on a running daemon without restart.
    hot_project_db: Arc<StdRwLock<Option<ProjectDbState>>>,
    llm: Arc<llm::LlmClient>,
    pipeline_enabled: bool,
    /// Cached proxy tools from registered MCP servers: server_id → Vec<Tool>
    proxy_tools: Arc<StdMutex<HashMap<String, Vec<rmcp::model::Tool>>>>,
    skill_tools: Arc<StdMutex<HashMap<String, String>>>,
    skill_tool_defs: Arc<StdMutex<HashMap<String, rmcp::model::Tool>>>,
    pool: Arc<McpClientPool>,
    tool_router: ToolRouter<Self>,
    // ─── Phantom Tools (result caching) ──────────────────────────────────────
    tool_cache: Arc<StdMutex<HashMap<String, CachedResult>>>,
    cache_hits: Arc<std::sync::atomic::AtomicU64>,
    cache_misses: Arc<std::sync::atomic::AtomicU64>,
    // ─── Dead Letter Queue (failed tool call auto-retry) ─────────────────
    dead_letters: Arc<StdMutex<VecDeque<DeadLetter>>>,
    mcp_discovery_timeout: Duration,
    mcp_tool_exposure_mode: McpToolExposureMode,
    // ─── Enrichment Batcher ──────────────────────────────────────────────────
    enrich_tx: mpsc::Sender<EnrichmentItem>,
    // ─── Foundry Maintenance Worker ──────────────────────────────────────────
    foundry_tx: mpsc::Sender<FoundryMaintenanceItem>,
    foundry_stats: Arc<FoundryWorkerStats>,
    // ─── Vault (Encrypted Secret Storage) ────────────────────────────────────
    vault_key: Arc<StdRwLock<Option<[u8; 32]>>>,
    vault_unlock_time: Arc<StdRwLock<Option<Instant>>>,
    vault_failed_attempts: Arc<StdMutex<(u32, Option<Instant>)>>,
    vault_auto_lock_after_secs: u64,
    // ─── Rate Limiter ────────────────────────────────────────────────────────
    /// Sliding window: tool call timestamps per session. Key = session_id (or "default").
    rate_limit_windows: Arc<StdMutex<HashMap<String, VecDeque<Instant>>>>,
    /// Burst detection: (tool_name + args_hash) → timestamps
    rate_limit_bursts: Arc<StdMutex<HashMap<String, VecDeque<Instant>>>>,
    /// Configured RPM limit (0 = unlimited)
    rate_limit_rpm: u64,
    /// Configured burst limit (0 = unlimited)
    rate_limit_burst: u64,
    // ─── Agent Profile ───────────────────────────────────────────────────────
    /// Per-session agent profile (set via agent_register tool).
    agent_profile: Arc<StdRwLock<Option<AgentProfile>>>,
    /// Default host-facing tool surface bundle selection for this server instance.
    tool_profile: Arc<StdRwLock<Option<ToolProfile>>>,
    // ─── Cross-Agent Handoff ─────────────────────────────────────────────────
    /// Pending handoff memos from previous agent sessions.
    handoff_memos: Arc<StdMutex<Vec<HandoffMemo>>>,
}

// MCP client pool types are in mcp_pool.rs

impl MemoryServer {
    fn new(
        global_db_path: PathBuf,
        project_db_path: Option<PathBuf>,
    ) -> Result<Self, Box<dyn std::error::Error>> {
        // Open stores once at startup (init_schema runs here, not per-request)
        let global_db_str = global_db_path.to_str().ok_or_else(|| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                format!(
                    "Global DB path contains invalid UTF-8: {}",
                    global_db_path.display()
                ),
            )
        })?;
        let global_store = MemoryStore::open_with_label(global_db_str, "global")?;
        let global_vec_available = global_store.vec_available;

        let (project_store, project_rw_gate, project_db_path, project_vec_available) =
            if let Some(ref p) = project_db_path {
                let project_db_str = p.to_str().ok_or_else(|| {
                    std::io::Error::new(
                        std::io::ErrorKind::InvalidInput,
                        format!("Project DB path contains invalid UTF-8: {}", p.display()),
                    )
                })?;
                // Derive project label from parent directory name
                // (e.g. ~/.tachi/projects/{name}/memory.db → {name}).
                let project_label = p
                    .parent()
                    .and_then(|parent| parent.file_name())
                    .and_then(|os| os.to_str())
                    .unwrap_or("project")
                    .to_string();
                let store = MemoryStore::open_with_label(project_db_str, &project_label)?;
                let v = store.vec_available;
                (
                    Some(Arc::new(StdMutex::new(store))),
                    Some(Arc::new(StdRwLock::new(()))),
                    Some(Arc::new(p.clone())),
                    v,
                )
            } else {
                (None, None, None, false)
            };

        let llm = Arc::new(llm::LlmClient::new()?);
        let pipeline_enabled = std::env::var("ENABLE_PIPELINE")
            .map(|v| v == "true" || v == "1")
            .unwrap_or(false);
        let mcp_discovery_timeout_ms = match parse_env_u64("MCP_DISCOVERY_TIMEOUT_MS") {
            Some(0) => {
                eprintln!("MCP_DISCOVERY_TIMEOUT_MS must be >= 1; using 1ms");
                1
            }
            Some(value) => value,
            None => DEFAULT_MCP_DISCOVERY_TIMEOUT_MS,
        };
        let mcp_tool_exposure_mode = std::env::var("MCP_TOOL_EXPOSURE_MODE")
            .ok()
            .and_then(|raw| match McpToolExposureMode::from_str(&raw) {
                Some(mode) => Some(mode),
                None => {
                    eprintln!(
                        "Ignoring invalid MCP_TOOL_EXPOSURE_MODE value '{}' (expected flatten|gateway)",
                        raw
                    );
                    None
                }
            })
            .unwrap_or(McpToolExposureMode::Flatten);

        let (enrich_tx, enrich_rx) = mpsc::channel::<EnrichmentItem>(ENRICH_CHANNEL_CAPACITY);
        let (foundry_tx, foundry_rx) =
            mpsc::channel::<FoundryMaintenanceItem>(FOUNDRY_CHANNEL_CAPACITY);
        let foundry_stats = Arc::new(FoundryWorkerStats::default());

        // Build hot-swap state before moving project_store into the struct
        let hot_project_db = Arc::new(StdRwLock::new(
            match (
                project_store.as_ref(),
                project_rw_gate.clone(),
                project_db_path.clone(),
            ) {
                (Some(store), Some(rw_gate), Some(db_path)) => Some(ProjectDbState {
                    store: Arc::clone(store),
                    rw_gate,
                    db_path,
                    vec_available: project_vec_available,
                }),
                _ => None,
            },
        ));

        let server = Self {
            global_store: Arc::new(StdMutex::new(global_store)),
            project_store,
            global_rw_gate: Arc::new(StdRwLock::new(())),
            project_rw_gate: project_rw_gate.clone(),
            global_db_path: Arc::new(global_db_path),
            project_db_path: project_db_path.clone(),
            global_vec_available,
            project_vec_available,
            hot_project_db,
            llm: llm.clone(),
            pipeline_enabled,
            proxy_tools: Arc::new(StdMutex::new(HashMap::new())),
            skill_tools: Arc::new(StdMutex::new(HashMap::new())),
            skill_tool_defs: Arc::new(StdMutex::new(HashMap::new())),
            pool: Arc::new(McpClientPool::new()),
            tool_router: Self::tool_router(),
            tool_cache: Arc::new(StdMutex::new(HashMap::new())),
            cache_hits: Arc::new(std::sync::atomic::AtomicU64::new(0)),
            cache_misses: Arc::new(std::sync::atomic::AtomicU64::new(0)),
            dead_letters: Arc::new(StdMutex::new(VecDeque::new())),
            mcp_discovery_timeout: Duration::from_millis(mcp_discovery_timeout_ms),
            mcp_tool_exposure_mode,
            enrich_tx,
            foundry_tx,
            foundry_stats,
            vault_key: Arc::new(StdRwLock::new(None)),
            vault_unlock_time: Arc::new(StdRwLock::new(None)),
            vault_failed_attempts: Arc::new(StdMutex::new((0, None))),
            vault_auto_lock_after_secs: 1800,
            rate_limit_windows: Arc::new(StdMutex::new(HashMap::new())),
            rate_limit_bursts: Arc::new(StdMutex::new(HashMap::new())),
            rate_limit_rpm: parse_env_u64("RATE_LIMIT_RPM").unwrap_or(DEFAULT_RATE_LIMIT_RPM),
            rate_limit_burst: parse_env_u64("RATE_LIMIT_BURST").unwrap_or(DEFAULT_RATE_LIMIT_BURST),
            agent_profile: Arc::new(StdRwLock::new(None)),
            tool_profile: Arc::new(StdRwLock::new(
                Some(crate::profiles::default_tool_profile()),
            )),
            handoff_memos: Arc::new(StdMutex::new(Vec::new())),
        };

        // Spawn the enrichment batcher worker
        {
            let batcher_server = server.clone();
            tokio::spawn(Self::run_enrichment_batcher(batcher_server, enrich_rx));
        }
        {
            let foundry_server = server.clone();
            tokio::spawn(run_foundry_maintenance_worker(foundry_server, foundry_rx));
        }

        // Replay pending foundry jobs from DB (survive process restart)
        {
            let replay_server = server.clone();
            tokio::spawn(async move {
                // Short delay to let the foundry worker start receiving
                tokio::time::sleep(std::time::Duration::from_millis(200)).await;

                let mut replayed = 0usize;

                // Helper: replay jobs from a store
                let replay_from = |jobs: Vec<memory_core::PersistedFoundryJob>| -> usize {
                    let mut count = 0;
                    for job in jobs {
                        let target_db = match job.target_db.as_str() {
                            "project" => DbScope::Project,
                            _ => DbScope::Global,
                        };
                        let item = FoundryMaintenanceItem {
                            job: job.spec,
                            target_db,
                            named_project: job.named_project,
                            path_prefix: job.path_prefix,
                            memory_ids: job.memory_ids,
                        };
                        if replay_server.foundry_tx.try_send(item).is_ok() {
                            count += 1;
                        }
                    }
                    count
                };

                // Replay from global DB
                if let Ok(jobs) = replay_server.with_global_store(|store| {
                    memory_core::load_pending_foundry_jobs(store.connection())
                        .map_err(|e| format!("load pending foundry jobs (global): {e}"))
                }) {
                    replayed += replay_from(jobs);
                }

                // Replay from project DB
                if let Ok(jobs) = replay_server.with_project_store(|store| {
                    memory_core::load_pending_foundry_jobs(store.connection())
                        .map_err(|e| format!("load pending foundry jobs (project): {e}"))
                }) {
                    replayed += replay_from(jobs);
                }

                if replayed > 0 {
                    eprintln!("[foundry] replayed {replayed} pending jobs from DB");
                }
            });
        }

        seed_builtin_capabilities(&server)
            .map_err(|e| std::io::Error::other(format!("seed builtin capabilities: {e}")))?;

        Ok(server)
    }

    fn refresh_llm_provider_secrets_from_vault(&self) -> Result<usize, String> {
        self.llm.clear_provider_secrets();
        let secrets = load_unlocked_api_key_secrets(self)?;
        Ok(self.llm.set_provider_secrets(secrets))
    }

    /// Clone the foundry maintenance sender so external supervisors
    /// (e.g. the multi-DB FoundryScheduler) can re-inject jobs into the
    /// same in-process worker that handles enrichment-driven enqueues.
    pub(crate) fn foundry_tx_clone(&self) -> mpsc::Sender<FoundryMaintenanceItem> {
        self.foundry_tx.clone()
    }

    /// Path to this server's global memory DB (canonicalized at boot).
    pub(crate) fn global_db_path_buf(&self) -> PathBuf {
        (*self.global_db_path).clone()
    }

    /// Path to this server's project memory DB, when one is bound.
    pub(crate) fn project_db_path_buf(&self) -> Option<PathBuf> {
        self.project_db_path.as_ref().map(|p| (**p).clone())
    }
}

// Enrichment batcher methods are in enrichment.rs

// ─── Tool Parameter Types ───────────────────────────────────────────────────────
//
// Note: dead_code warnings are expected here because the #[tool] macro
// generates code that uses these types through macro expansion.

// Parameter and tool schema definitions moved to `tool_params.rs`.

// MCP pool proxy methods are in mcp_pool.rs

// ─── Main ────────────────────────────────────────────────────────────────────────

fn main() {
    let cli = Cli::parse();
    if let Err(e) = bootstrap::run(cli) {
        if let Some(exit) = e.downcast_ref::<repair::RepairExit>() {
            std::process::exit(exit.code());
        }
        eprintln!("Fatal: {e}");
        std::process::exit(1);
    }
}

#[cfg(test)]
mod tests;
