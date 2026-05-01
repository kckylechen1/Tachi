// main.rs — Memory MCP Server
//
// Rust MCP server using rmcp SDK to expose memory-core functionality.
// Stateless design: each tool opens its own DB connection per-request.

mod bootstrap;
mod builtins;
mod capability_ops;
mod capture_gate;
mod clawdoctor;
mod cli;
mod cli_client;
mod copilot_ops;
mod daemon_lock;
mod dispatch_ops;
mod dlq_ops;
mod doctor;
mod doctor_ops;
mod enrichment;
mod foundry_ops;
mod foundry_runtime_ops;
mod foundry_scheduler;
mod ghost_ops;
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
mod skill_chain_ops;
mod status_ops;
mod tool_params;
mod utils;
mod vault_crypto;
mod vault_ops;
mod wiki_ops;

use crate::builtins::seed_builtin_capabilities;
use crate::capability_ops::{
    handle_prepare_capability_bundle, handle_recommend_capability, handle_recommend_skill,
    handle_recommend_toolchain,
};
use crate::copilot_ops::{
    handle_tachi_progress_check, handle_tachi_task_brief, handle_tachi_wiki_search,
    handle_tachi_wiki_write,
};
use crate::dlq_ops::{handle_dlq_list, handle_dlq_retry};
use crate::foundry_ops::{
    handle_list_agent_evolution_proposals, handle_project_agent_profile,
    handle_queue_agent_evolution, handle_review_agent_evolution_proposal,
    handle_synthesize_agent_evolution,
};
use crate::foundry_runtime_ops::{
    enqueue_foundry_capture_maintenance, handle_capture_session, handle_compact_context,
    handle_compact_rollup, handle_compact_session_memory, handle_recall_context,
    handle_section_build, run_foundry_maintenance_worker, FoundryMaintenanceItem,
    FoundryWorkerStats,
};
use crate::ghost_ops::{
    handle_ghost_ack, handle_ghost_promote, handle_ghost_publish, handle_ghost_reflect,
    handle_ghost_subscribe, handle_ghost_topics,
};
use crate::graph_state_ops::{
    handle_add_edge, handle_get_edges, handle_get_state, handle_memory_graph, handle_set_state,
};
use crate::handoff_ops::{handle_handoff_check, handle_handoff_leave};
use crate::hub_helpers::{
    build_skill_tool_from_cap, capability_callable, capability_visibility_for_cap,
    make_text_tool_result, review_status_allows_call, should_expose_mcp_tools,
    should_expose_skill_tool, CapabilityVisibility,
};
use crate::hub_ops::{
    handle_distill_trajectory, handle_export_skills, handle_hub_call, handle_hub_disconnect,
    handle_hub_discover, handle_hub_feedback, handle_hub_get, handle_hub_quick_add,
    handle_hub_register, handle_hub_review, handle_hub_set_active_version, handle_hub_set_enabled,
    handle_hub_stats, handle_run_skill, handle_skill_evolve, handle_tachi_audit_log,
    handle_vc_bind, handle_vc_list, handle_vc_register, handle_vc_resolve,
};
use crate::kanban::{
    gc_expired_kanban_cards, handle_check_inbox, handle_post_card, handle_update_card,
    CheckInboxParams, PostCardParams, UpdateCardParams, DEFAULT_KANBAN_GC_MAX_AGE_DAYS,
};
use crate::mcp_proxy::{
    append_warning, clear_mcp_discovery_metadata, filter_mcp_tools_by_permissions,
    resolve_mcp_tool_exposure, set_mcp_discovery_failure, set_mcp_discovery_success,
    McpToolExposureMode,
};
use crate::memory_ops::{
    handle_archive_memory, handle_delete_domain, handle_delete_memory, handle_get_domain,
    handle_get_memory, handle_list_domains, handle_list_memories, handle_memory_gc,
    handle_memory_stats, handle_register_domain,
};
use crate::memory_search_ops::{
    handle_find_similar_memory, handle_remember, handle_save_memory, handle_search_memory,
    search_memory_rows,
};
use crate::pack_ops::{
    handle_pack_get, handle_pack_list, handle_pack_project, handle_pack_register,
    handle_pack_remove, handle_projection_list,
};
use crate::pipeline_ops::{
    handle_extract_facts, handle_get_pipeline_status, handle_ingest, handle_ingest_event,
    handle_ingest_source, handle_sync_memories,
};
use crate::profiles::ToolProfile;
use crate::project_db_ops::handle_tachi_init_project_db;
use crate::sandbox_ops::{
    handle_sandbox_check, handle_sandbox_exec_audit, handle_sandbox_get_policy,
    handle_sandbox_list_policies, handle_sandbox_set_policy, handle_sandbox_set_rule,
};
use crate::shared_defs::{
    categorize_error, slim_entry, slim_entry_with_enrichment, slim_l0_rule, slim_search_result,
    DeadLetter, DLQ_MAX_ENTRIES, DLQ_TTL_SECS,
};
use crate::skill_chain_ops::handle_chain_skills;
use crate::tool_params::*;
use crate::utils::{
    find_git_root, is_active_global_rule, is_trusted_command, lock_or_recover, parse_env_bool,
    parse_env_u64, read_or_recover, sanitize_safe_path_name, stable_hash, value_to_template_text,
    write_or_recover,
};
use crate::vault_ops::{
    handle_vault_get, handle_vault_init, handle_vault_list, handle_vault_lock, handle_vault_remove,
    handle_vault_set, handle_vault_setup_rotation, handle_vault_status, handle_vault_unlock,
    load_unlocked_api_key_secrets, VaultGetParams, VaultInitParams, VaultListParams,
    VaultRemoveParams, VaultSetParams, VaultSetupRotationParams, VaultUnlockParams,
};
use crate::wiki_ops::{handle_wiki_browse, handle_wiki_lint, handle_wiki_search};

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
    tool, tool_router,
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
    "compact_rollup",
    "compact_session_memory",
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
    "ghost_reflect",
    "ghost_promote",
    "pack_register",
    "pack_remove",
    "pack_project",
    "register_domain",
    "delete_domain",
    "distill_trajectory",
    "wiki_lint",
    "tachi_wiki_write",
    // Facade tools (write / mixed)
    "tachi_save",
    "tachi_handoff",
];

struct CachedResult {
    result: rmcp::model::CallToolResult,
    created_at: Instant,
}

fn first_text_blocks(result: &rmcp::model::CallToolResult) -> Vec<String> {
    result
        .content
        .iter()
        .filter_map(|item| {
            serde_json::to_value(item).ok().and_then(|value| {
                value
                    .get("text")
                    .and_then(|text| text.as_str())
                    .map(String::from)
            })
        })
        .collect()
}

fn matches_web_search_backend(capability_id: &str, selector: Option<&str>) -> bool {
    let Some(selector) = selector.map(str::trim).filter(|value| !value.is_empty()) else {
        return true;
    };
    if selector.eq_ignore_ascii_case("auto") {
        return true;
    }
    let normalized = selector
        .strip_prefix("mcp:")
        .unwrap_or(selector)
        .to_ascii_lowercase();
    let cap = capability_id
        .strip_prefix("mcp:")
        .unwrap_or(capability_id)
        .to_ascii_lowercase();
    cap == normalized || cap.contains(&normalized)
}

fn discovered_tool_schema(definition: &Value, tool_name: &str) -> Option<Value> {
    definition
        .get("discovered_tools")
        .and_then(|tools| tools.as_array())
        .and_then(|tools| {
            tools.iter().find(|tool| {
                tool.get("name")
                    .and_then(|name| name.as_str())
                    .map(|name| name == tool_name)
                    .unwrap_or(false)
            })
        })
        .and_then(|tool| {
            tool.get("inputSchema")
                .or_else(|| tool.get("input_schema"))
                .cloned()
        })
}

fn choose_web_search_tool(
    capability_id: &str,
    definition: &Value,
    explicit_tool_name: Option<&str>,
) -> String {
    if let Some(name) = explicit_tool_name
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        return name.to_string();
    }

    if let Some(name) = definition
        .get("discovered_tools")
        .and_then(|tools| tools.as_array())
        .and_then(|tools| {
            tools
                .iter()
                .filter_map(|tool| tool.get("name").and_then(|name| name.as_str()))
                .find(|name| name.to_ascii_lowercase().contains("search"))
        })
    {
        return name.to_string();
    }

    match capability_id {
        "mcp:exa" => "web_search_exa".to_string(),
        "mcp:tavily" => "tavily_search".to_string(),
        "mcp:web-search" => "web_search".to_string(),
        _ => "search".to_string(),
    }
}

fn web_search_arguments(
    params: &TachiWebSearchParams,
    input_schema: Option<&Value>,
) -> serde_json::Map<String, Value> {
    let mut args = serde_json::Map::new();
    args.insert("query".to_string(), json!(params.query));

    let has_property = |name: &str| {
        input_schema
            .and_then(|schema| schema.get("properties"))
            .and_then(|properties| properties.as_object())
            .map(|properties| properties.contains_key(name))
            .unwrap_or(false)
    };

    for key in ["numResults", "max_results", "maxResults", "limit", "top_k"] {
        if has_property(key) {
            args.insert(key.to_string(), json!(params.top_k));
            break;
        }
    }

    if !params.include_domains.is_empty() {
        for key in ["include_domains", "includeDomains"] {
            if has_property(key) {
                args.insert(key.to_string(), json!(params.include_domains));
                break;
            }
        }
    }

    if !params.exclude_domains.is_empty() {
        for key in ["exclude_domains", "excludeDomains"] {
            if has_property(key) {
                args.insert(key.to_string(), json!(params.exclude_domains));
                break;
            }
        }
    }

    args
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
struct AgentProfile {
    agent_id: String,
    display_name: String,
    capabilities: Vec<String>,
    tool_filter: Option<Vec<String>>,
    rate_limit_rpm: Option<u64>,
    rate_limit_burst: Option<u64>,
    registered_at: String,
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
    // ─── Ghost Whispers (pub/sub) ───────────────────────────────────────────
    pubsub: Arc<StdMutex<PubSubState>>,
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
        let hot_project_db = Arc::new(StdRwLock::new(project_store.as_ref().map(|s| {
            ProjectDbState {
                store: Arc::clone(s),
                rw_gate: project_rw_gate
                    .clone()
                    .expect("project_rw_gate must exist if project_store exists"),
                db_path: project_db_path
                    .clone()
                    .expect("project_db_path must exist if project_store exists"),
                vec_available: project_vec_available,
            }
        })));

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
            pubsub: Arc::new(StdMutex::new(PubSubState::new())),
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
            tool_profile: Arc::new(StdRwLock::new(None)),
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

// ─── Ghost Whispers (Inter-Agent Pub/Sub) ─────────────────────────────────────

const PUBSUB_RING_MAX: usize = 100;
const PUBSUB_MAX_CURSORS: usize = 1000;

struct PubSubState {
    /// agent_id → (topic → last_seen_global_index)
    cursors: HashMap<String, HashMap<String, usize>>,
    /// agent_id → monotonic recency sequence (used for LRU cursor eviction)
    cursor_recency: HashMap<String, u64>,
    /// Monotonic sequence counter for cursor recency tracking
    cursor_seq: u64,
}

impl PubSubState {
    fn new() -> Self {
        Self {
            cursors: HashMap::new(),
            cursor_recency: HashMap::new(),
            cursor_seq: 0,
        }
    }
}

// ─── Tool Implementations ────────────────────────────────────────────────────────

#[tool_router]
impl MemoryServer {
    #[tool(
        description = "Save a memory entry to the store. Creates a new entry or updates an existing one if id is provided."
    )]
    async fn save_memory(
        &self,
        Parameters(params): Parameters<SaveMemoryParams>,
    ) -> Result<String, String> {
        handle_save_memory(self, params).await
    }

    #[tool(
        description = "Ghost-in-the-Shell style alias for save_memory. Write memory into cyberbrain."
    )]
    async fn cyberbrain_write(
        &self,
        Parameters(params): Parameters<SaveMemoryParams>,
    ) -> Result<String, String> {
        handle_save_memory(self, params).await
    }

    #[tool(
        description = "Low-friction shortcut to save a note. Only `text` is required; path defaults to /notes/{YYYY-MM-DD}, category to \"fact\", importance to 0.6, scope to \"project\". Use save_memory directly when you need full control over path, importance, retention, vector, or auto-link."
    )]
    async fn remember(
        &self,
        Parameters(params): Parameters<RememberParams>,
    ) -> Result<String, String> {
        handle_remember(self, params).await
    }

    #[tool(
        description = "Search memory entries using hybrid search (vector + FTS + symbolic). Returns ranked results with scores."
    )]
    async fn search_memory(
        &self,
        Parameters(params): Parameters<SearchMemoryParams>,
    ) -> Result<String, String> {
        handle_search_memory(self, params).await
    }

    #[tool(
        description = "Ghost-in-the-Shell style alias for search_memory. Query memories from cyberbrain."
    )]
    async fn cyberbrain_search(
        &self,
        Parameters(params): Parameters<SearchMemoryParams>,
    ) -> Result<String, String> {
        handle_search_memory(self, params).await
    }

    #[tool(
        description = "Find memory entries similar to a provided vector. Uses vector similarity only (no FTS/symbolic/decay weighting)."
    )]
    async fn find_similar_memory(
        &self,
        Parameters(params): Parameters<FindSimilarMemoryParams>,
    ) -> Result<String, String> {
        handle_find_similar_memory(self, params).await
    }

    #[tool(description = "Get a single memory entry by ID.")]
    async fn get_memory(
        &self,
        Parameters(params): Parameters<GetMemoryParams>,
    ) -> Result<String, String> {
        handle_get_memory(self, params).await
    }

    #[tool(description = "List memory entries under a path prefix.")]
    async fn list_memories(
        &self,
        Parameters(params): Parameters<ListMemoriesParams>,
    ) -> Result<String, String> {
        handle_list_memories(self, params).await
    }

    #[tool(description = "Get aggregate statistics about the memory store.")]
    async fn memory_stats(&self) -> Result<String, String> {
        handle_memory_stats(self).await
    }

    #[tool(
        description = "Doctor v2 — scan known memory.db roots, classify each (healthy / vec_extension_missing / wal_orphan / corrupt / legacy_schema / placeholder / backup), return JSON report. Read-only; no mutations."
    )]
    async fn tachi_doctor_scan(&self) -> Result<String, String> {
        crate::doctor_ops::handle_tachi_doctor_scan().await
    }

    #[tool(
        description = "Delete a memory entry permanently. Removes from main table, FTS, vectors, graph edges, and access history."
    )]
    async fn delete_memory(
        &self,
        Parameters(params): Parameters<DeleteMemoryParams>,
    ) -> Result<String, String> {
        handle_delete_memory(self, params).await
    }

    #[tool(
        description = "Archive a memory entry (soft-delete, set archived=1). Entry is hidden from default searches but can be retrieved with include_archived=true."
    )]
    async fn archive_memory(
        &self,
        Parameters(params): Parameters<ArchiveMemoryParams>,
    ) -> Result<String, String> {
        handle_archive_memory(self, params).await
    }

    #[tool(
        description = "Run garbage collection on growing tables. Prunes old access_history (keep latest 256 per memory), processed_events (30d), audit_log (30d + 100k cap), and agent_known_state (90d)."
    )]
    async fn memory_gc(&self) -> Result<String, String> {
        handle_memory_gc(self).await
    }

    #[tool(description = "Enable or disable a Hub capability by ID.")]
    async fn hub_set_enabled(
        &self,
        Parameters(params): Parameters<HubSetEnabledParams>,
    ) -> Result<String, String> {
        handle_hub_set_enabled(self, params).await
    }

    #[tool(description = "Set governance review status for a Hub capability.")]
    async fn hub_review(
        &self,
        Parameters(params): Parameters<HubReviewParams>,
    ) -> Result<String, String> {
        handle_hub_review(self, params).await
    }

    #[tool(
        description = "Ghost-in-the-Shell style alias for hub_review (Section 9 governance review)."
    )]
    async fn section9_review(
        &self,
        Parameters(params): Parameters<HubReviewParams>,
    ) -> Result<String, String> {
        handle_hub_review(self, params).await
    }

    #[tool(description = "Route an alias capability ID to a concrete active capability version.")]
    async fn hub_set_active_version(
        &self,
        Parameters(params): Parameters<HubSetActiveVersionParams>,
    ) -> Result<String, String> {
        handle_hub_set_active_version(self, params).await
    }

    #[tool(
        description = "Export Hub skills to agent-specific file formats. Targets: claude (SKILL.md + symlinks), openclaw (plugin manifest), cursor (.mdc rules), generic (raw files)."
    )]
    async fn hub_export_skills(
        &self,
        Parameters(params): Parameters<ExportSkillsParams>,
    ) -> Result<String, String> {
        handle_export_skills(self, params).await
    }

    #[tool(
        description = "Register a Virtual Capability (logical capability layer) on top of concrete backends."
    )]
    async fn vc_register(
        &self,
        Parameters(params): Parameters<VirtualCapabilityRegisterParams>,
    ) -> Result<String, String> {
        handle_vc_register(self, params).await
    }

    #[tool(
        description = "Bind a Virtual Capability to a concrete capability with deterministic priority and optional version pin."
    )]
    async fn vc_bind(
        &self,
        Parameters(params): Parameters<VirtualCapabilityBindParams>,
    ) -> Result<String, String> {
        handle_vc_bind(self, params).await
    }

    #[tool(description = "List Virtual Capabilities together with their current bindings.")]
    async fn vc_list(
        &self,
        Parameters(params): Parameters<HubDiscoverParams>,
    ) -> Result<String, String> {
        handle_vc_list(self, params).await
    }

    #[tool(
        description = "Resolve a Virtual Capability to the concrete capability currently selected for routing."
    )]
    async fn vc_resolve(
        &self,
        Parameters(params): Parameters<VirtualCapabilityResolveParams>,
    ) -> Result<String, String> {
        handle_vc_resolve(self, params).await
    }

    #[tool(
        description = "Add or update an edge in the memory graph. Edges represent causal, temporal, or entity relationships between memories."
    )]
    async fn add_edge(
        &self,
        Parameters(params): Parameters<AddEdgeParams>,
    ) -> Result<String, String> {
        handle_add_edge(self, params).await
    }

    #[tool(
        description = "Get edges connected to a memory entry. Returns causal, temporal, and entity relationship edges."
    )]
    async fn get_edges(
        &self,
        Parameters(params): Parameters<GetEdgesParams>,
    ) -> Result<String, String> {
        handle_get_edges(self, params).await
    }

    #[tool(
        description = "Inspect a read-only neighborhood from the memory graph, seeded by memory id or a search query. Returns seed nodes, neighboring nodes, and connecting edges."
    )]
    async fn memory_graph(
        &self,
        Parameters(params): Parameters<MemoryGraphParams>,
    ) -> Result<String, String> {
        handle_memory_graph(self, params).await
    }

    #[tool(
        description = "Run health checks over wiki memories and skill graph state. Returns orphan nodes, contradiction candidates, stale nodes, missing edge hints, and current skill quality guard status."
    )]
    async fn wiki_lint(
        &self,
        Parameters(params): Parameters<WikiLintParams>,
    ) -> Result<String, String> {
        handle_wiki_lint(self, params).await
    }

    #[tool(
        description = "Write a durable wiki entry under /wiki with sane defaults for path, retention, metadata, and auto-linking."
    )]
    async fn tachi_wiki_write(
        &self,
        Parameters(params): Parameters<WikiWriteParams>,
    ) -> Result<String, String> {
        handle_tachi_wiki_write(self, params).await
    }

    #[tool(
        description = "Search wiki entries under /wiki. Use this before debugging from scratch or when a prior lesson may exist."
    )]
    async fn tachi_wiki_search(
        &self,
        Parameters(params): Parameters<WikiSearchParams>,
    ) -> Result<String, String> {
        handle_tachi_wiki_search(self, params).await
    }

    #[tool(
        description = "Prepare a task brief before non-trivial work: relevant wiki lessons, memory hits, lightweight skill suggestions, and debugging checklist."
    )]
    async fn tachi_task_brief(
        &self,
        Parameters(params): Parameters<TaskBriefParams>,
    ) -> Result<String, String> {
        handle_tachi_task_brief(self, params).await
    }

    #[tool(
        description = "Check whether an agent is stuck after repeated attempts. Returns reframe advice, relevant wiki hits, and an ask-codex prompt when useful."
    )]
    async fn tachi_progress_check(
        &self,
        Parameters(params): Parameters<ProgressCheckParams>,
    ) -> Result<String, String> {
        handle_tachi_progress_check(self, params).await
    }

    #[tool(
        description = "Search the wiki knowledge base for relevant entries. The wiki contains distilled knowledge from past development sessions organized by category (quant, engineering, agent, product). Supports short category aliases like 'quant', 'strategy', 'tachi', 'debugging', etc."
    )]
    async fn wiki_search(
        &self,
        Parameters(params): Parameters<WikiSearchParams>,
    ) -> Result<String, String> {
        handle_wiki_search(self, params).await
    }

    #[tool(
        description = "Browse wiki entries by category. Without a category, returns category stats (counts per path). With a category, lists entries under that path. Supports short aliases like 'quant', 'engineering', 'tachi', etc."
    )]
    async fn wiki_browse(
        &self,
        Parameters(params): Parameters<WikiBrowseParams>,
    ) -> Result<String, String> {
        handle_wiki_browse(self, params)
    }

    #[tool(description = "Set a key-value pair in server state (stored in hard_state table).")]
    async fn set_state(
        &self,
        Parameters(params): Parameters<SetStateParams>,
    ) -> Result<String, String> {
        handle_set_state(self, params).await
    }

    #[tool(description = "Get a value from server state by key.")]
    async fn get_state(
        &self,
        Parameters(params): Parameters<GetStateParams>,
    ) -> Result<String, String> {
        handle_get_state(self, params).await
    }

    #[tool(description = "Extract structured facts from text using LLM and save to memory.")]
    async fn extract_facts(
        &self,
        Parameters(params): Parameters<ExtractFactsParams>,
    ) -> Result<String, String> {
        handle_extract_facts(self, params).await
    }

    #[tool(description = "Ingest a conversation event and extract facts from messages.")]
    async fn ingest_event(
        &self,
        Parameters(params): Parameters<IngestEventParams>,
    ) -> Result<String, String> {
        handle_ingest_event(self, params).await
    }

    #[tool(
        description = "Unified ingest entrypoint for conversation events and source documents. Use ingest_type to select event vs source behavior."
    )]
    async fn ingest(&self, Parameters(params): Parameters<IngestParams>) -> Result<String, String> {
        handle_ingest(self, params).await
    }

    #[tool(
        description = "Batch ingest source content with optional chunking, enrichment, and graph edge building."
    )]
    async fn ingest_source(
        &self,
        Parameters(params): Parameters<IngestSourceParams>,
    ) -> Result<String, String> {
        handle_ingest_source(self, params).await
    }

    #[tool(description = "Get pipeline status and statistics.")]
    async fn get_pipeline_status(&self) -> Result<String, String> {
        handle_get_pipeline_status(self).await
    }

    #[tool(
        description = "Get only new or changed memories since last sync for this agent. Returns incremental diff to save tokens. Use agent_id to identify your agent uniquely."
    )]
    async fn sync_memories(
        &self,
        Parameters(params): Parameters<SyncMemoriesParams>,
    ) -> Result<String, String> {
        handle_sync_memories(self, params).await
    }

    #[tool(description = "Post a kanban card from one agent to another.")]
    async fn post_card(
        &self,
        Parameters(params): Parameters<PostCardParams>,
    ) -> Result<String, String> {
        handle_post_card(self, params).await
    }

    #[tool(description = "Check kanban inbox for a target agent.")]
    async fn check_inbox(
        &self,
        Parameters(params): Parameters<CheckInboxParams>,
    ) -> Result<String, String> {
        handle_check_inbox(self, params).await
    }

    #[tool(description = "Update status of a kanban card.")]
    async fn update_card(
        &self,
        Parameters(params): Parameters<UpdateCardParams>,
    ) -> Result<String, String> {
        handle_update_card(self, params).await
    }

    #[tool(description = "Register a capability (skill, plugin, or MCP server) in the Hub.")]
    async fn hub_register(
        &self,
        Parameters(params): Parameters<HubRegisterParams>,
    ) -> Result<String, String> {
        handle_hub_register(self, params).await
    }

    #[tool(
        description = "Composite: hub_register followed by an optional hub_review approve+enable. \
                       Honors the trusted-command allowlist — auto_approve is silently dropped \
                       (with a warning) for untrusted stdio MCP commands."
    )]
    async fn hub_quick_add(
        &self,
        Parameters(params): Parameters<HubQuickAddParams>,
    ) -> Result<String, String> {
        handle_hub_quick_add(self, params).await
    }

    #[tool(
        description = "Discover available capabilities (skills, plugins, MCP servers) in the Hub."
    )]
    async fn hub_discover(
        &self,
        Parameters(params): Parameters<HubDiscoverParams>,
    ) -> Result<String, String> {
        handle_hub_discover(self, params).await
    }

    #[tool(description = "Get a specific capability from the Hub by ID.")]
    async fn hub_get(
        &self,
        Parameters(params): Parameters<HubGetParams>,
    ) -> Result<String, String> {
        handle_hub_get(self, params).await
    }

    #[tool(description = "Record feedback for a Hub capability invocation.")]
    async fn hub_feedback(
        &self,
        Parameters(params): Parameters<HubFeedbackParams>,
    ) -> Result<String, String> {
        handle_hub_feedback(self, params).await
    }

    #[tool(description = "Get Hub capability statistics and metrics.")]
    async fn hub_stats(&self) -> Result<String, String> {
        handle_hub_stats(self).await
    }

    #[tool(
        description = "Execute a registered Skill from the Hub using the internal LLM pipeline."
    )]
    async fn run_skill(
        &self,
        Parameters(params): Parameters<RunSkillParams>,
    ) -> Result<String, String> {
        handle_run_skill(self, params).await
    }

    #[tool(
        description = "Distill a completed task trajectory into a reusable Skill, persist a permanent skill snapshot, and register/update the distilled Hub Skill."
    )]
    async fn distill_trajectory(
        &self,
        Parameters(params): Parameters<DistillTrajectoryParams>,
    ) -> Result<String, String> {
        handle_distill_trajectory(self, params).await
    }

    #[tool(
        description = "Evolve a skill by analyzing its telemetry and using LLM to produce an improved prompt. Creates a new versioned capability."
    )]
    async fn skill_evolve(
        &self,
        Parameters(params): Parameters<SkillEvolveParams>,
    ) -> Result<String, String> {
        handle_skill_evolve(self, params).await
    }

    #[tool(
        description = "Recommend the best Tachi capability for a task query. Uses Hub metadata, visibility, host constraints, and telemetry to rank candidate capabilities."
    )]
    async fn recommend_capability(
        &self,
        Parameters(params): Parameters<RecommendCapabilityParams>,
    ) -> Result<String, String> {
        handle_recommend_capability(self, params).await
    }

    #[tool(
        description = "Recommend the most relevant skills for a task query. Returns ranked skill candidates plus callable tool aliases when available."
    )]
    async fn recommend_skill(
        &self,
        Parameters(params): Parameters<RecommendSkillParams>,
    ) -> Result<String, String> {
        handle_recommend_skill(self, params).await
    }

    #[tool(
        description = "Recommend a host-aware toolchain for a task query, including skills, supporting capabilities, projected packs, and suggested host-native execution tools."
    )]
    async fn recommend_toolchain(
        &self,
        Parameters(params): Parameters<RecommendToolchainParams>,
    ) -> Result<String, String> {
        handle_recommend_toolchain(self, params).await
    }

    #[tool(
        description = "Prepare a host-aware capability bundle for a task query. Returns the primary skill, supporting capabilities, relevant packs, suggested host-native tools, and a ready-to-inject bundle section."
    )]
    async fn prepare_capability_bundle(
        &self,
        Parameters(params): Parameters<PrepareCapabilityBundleParams>,
    ) -> Result<String, String> {
        handle_prepare_capability_bundle(self, params).await
    }

    #[tool(
        description = "Recall structured memory context for an active agent turn. Returns ranked results plus a ready-to-inject prepend_context block."
    )]
    async fn recall_context(
        &self,
        Parameters(params): Parameters<RecallContextParams>,
    ) -> Result<String, String> {
        handle_recall_context(self, params).await
    }

    #[tool(
        description = "Capture durable memories from a recent session window. Extracts structured memories, embeds them inside Tachi, and writes them to the configured store."
    )]
    async fn capture_session(
        &self,
        Parameters(params): Parameters<CaptureSessionParams>,
    ) -> Result<String, String> {
        handle_capture_session(self, params).await
    }

    #[tool(
        description = "Compact a soon-to-be-evicted session window into a ready-to-inject context block. Designed for host runtimes that know when token pressure requires compaction."
    )]
    async fn compact_context(
        &self,
        Parameters(params): Parameters<CompactContextParams>,
    ) -> Result<String, String> {
        handle_compact_context(self, params).await
    }

    #[tool(
        description = "Render a structured context section with explicit layer and cache-boundary markers. Useful for host runtimes assembling static/session/live prompt sections."
    )]
    async fn section_build(
        &self,
        Parameters(params): Parameters<SectionBuildParams>,
    ) -> Result<String, String> {
        handle_section_build(self, params).await
    }

    #[tool(
        description = "Roll up multiple compacted session artifacts into a new compact summary block, preserving salient topics and durable signals for later reinjection."
    )]
    async fn compact_rollup(
        &self,
        Parameters(params): Parameters<CompactRollupParams>,
    ) -> Result<String, String> {
        handle_compact_rollup(self, params).await
    }

    #[tool(
        description = "Persist a compacted session artifact and its durable signals into Tachi memory, then optionally queue Foundry maintenance jobs."
    )]
    async fn compact_session_memory(
        &self,
        Parameters(params): Parameters<CompactSessionMemoryParams>,
    ) -> Result<String, String> {
        handle_compact_session_memory(self, params).await
    }

    #[tool(
        description = "Synthesize agent evolution proposals from canonical profile documents and evidence. Returns structured JSON proposals; use dry_run=true to inspect the normalized request without calling the model."
    )]
    async fn synthesize_agent_evolution(
        &self,
        Parameters(params): Parameters<SynthesizeAgentEvolutionParams>,
    ) -> Result<String, String> {
        handle_synthesize_agent_evolution(self, params).await
    }

    #[tool(
        description = "Queue an agent evolution synthesis job. Persists job state and stores generated proposals for later review."
    )]
    async fn queue_agent_evolution(
        &self,
        Parameters(params): Parameters<SynthesizeAgentEvolutionParams>,
    ) -> Result<String, String> {
        handle_queue_agent_evolution(self, params).await
    }

    #[tool(
        description = "List persisted agent evolution proposals for a target agent. Optionally filter by review status."
    )]
    async fn list_agent_evolution_proposals(
        &self,
        Parameters(params): Parameters<ListAgentEvolutionProposalsParams>,
    ) -> Result<String, String> {
        handle_list_agent_evolution_proposals(self, params).await
    }

    #[tool(
        description = "Review a persisted agent evolution proposal by marking it approved, rejected, or applied."
    )]
    async fn review_agent_evolution_proposal(
        &self,
        Parameters(params): Parameters<ReviewAgentEvolutionProposalParams>,
    ) -> Result<String, String> {
        handle_review_agent_evolution_proposal(self, params).await
    }

    #[tool(
        description = "Project approved agent evolution proposals into host documents. Returns projected content and can optionally write back to disk paths."
    )]
    async fn project_agent_profile(
        &self,
        Parameters(params): Parameters<ProjectAgentProfileParams>,
    ) -> Result<String, String> {
        handle_project_agent_profile(self, params).await
    }

    #[tool(description = "View audit log of proxy tool calls through the Hub.")]
    async fn tachi_audit_log(
        &self,
        Parameters(params): Parameters<AuditLogParams>,
    ) -> Result<String, String> {
        handle_tachi_audit_log(self, params).await
    }

    #[tool(
        description = "Ghost-in-the-Shell style alias for tachi_audit_log (Section 9 audit view)."
    )]
    async fn section9_audit_log(
        &self,
        Parameters(params): Parameters<AuditLogParams>,
    ) -> Result<String, String> {
        handle_tachi_audit_log(self, params).await
    }

    #[tool(
        description = "Call a tool on a registered MCP server through the Hub using the shared connection pool."
    )]
    async fn hub_call(
        &self,
        Parameters(params): Parameters<HubCallParams>,
    ) -> Result<String, String> {
        handle_hub_call(self, params).await
    }

    #[tool(
        description = "Disconnect a cached MCP server connection from the pool. Forces a fresh reconnect (with updated env/config) on next hub_call."
    )]
    async fn hub_disconnect(
        &self,
        Parameters(params): Parameters<HubDisconnectParams>,
    ) -> Result<String, String> {
        handle_hub_disconnect(self, params).await
    }

    #[tool(
        description = "Initialize a project-scoped Tachi memory DB under the current or target git repository."
    )]
    async fn tachi_init_project_db(
        &self,
        Parameters(params): Parameters<InitProjectDbParams>,
    ) -> Result<String, String> {
        handle_tachi_init_project_db(self, params).await
    }

    // ─── Agent Profile ───────────────────────────────────────────────────────

    #[tool(
        description = "Register this agent session with an identity profile. Enables per-agent memory scoping, tool filtering, and rate limit customization."
    )]
    async fn agent_register(
        &self,
        Parameters(params): Parameters<AgentRegisterParams>,
    ) -> Result<String, String> {
        let profile = AgentProfile {
            agent_id: params.agent_id.clone(),
            display_name: params
                .display_name
                .unwrap_or_else(|| params.agent_id.clone()),
            capabilities: params.capabilities,
            tool_filter: params.tool_filter,
            rate_limit_rpm: params.rate_limit_rpm,
            rate_limit_burst: params.rate_limit_burst,
            registered_at: Utc::now().to_rfc3339(),
        };

        let response = serde_json::to_string(&serde_json::json!({
            "status": "registered",
            "agent_id": profile.agent_id,
            "display_name": profile.display_name,
            "capabilities": profile.capabilities,
            "tool_filter": profile.tool_filter,
            "rate_limit_rpm": profile.rate_limit_rpm,
            "rate_limit_burst": profile.rate_limit_burst,
            "registered_at": profile.registered_at,
        }))
        .map_err(|e| format!("serialize: {e}"))?;

        let mut guard = self
            .agent_profile
            .write()
            .unwrap_or_else(|e| e.into_inner());
        *guard = Some(profile);

        Ok(response)
    }

    #[tool(
        description = "Return the current agent profile for this session, or null if no agent has registered."
    )]
    async fn agent_whoami(
        &self,
        Parameters(_params): Parameters<AgentWhoamiParams>,
    ) -> Result<String, String> {
        let guard = self.agent_profile.read().unwrap_or_else(|e| e.into_inner());
        match guard.as_ref() {
            Some(profile) => serde_json::to_string(&profile).map_err(|e| format!("serialize: {e}")),
            None => Ok(r#"{"status":"unregistered","message":"No agent profile set. Call agent_register to identify this session."}"#.to_string()),
        }
    }

    // ─── Cross-Agent Handoff ─────────────────────────────────────────────────

    #[tool(
        description = "Leave a handoff memo for the next agent session. Contains session summary, next steps, and optional context."
    )]
    async fn handoff_leave(
        &self,
        Parameters(params): Parameters<HandoffLeaveParams>,
    ) -> Result<String, String> {
        handle_handoff_leave(self, params).await
    }

    #[tool(
        description = "Check for pending handoff memos from previous agent sessions. Call this at the start of a new session."
    )]
    async fn handoff_check(
        &self,
        Parameters(params): Parameters<HandoffCheckParams>,
    ) -> Result<String, String> {
        handle_handoff_check(self, params).await
    }

    // ─── Ghost Whispers (Inter-Agent Pub/Sub) ────────────────────────────────

    #[tool(
        description = "Publish a message to a Ghost Whispers topic. Other agents can poll for new messages via ghost_subscribe."
    )]
    async fn ghost_publish(
        &self,
        Parameters(params): Parameters<GhostPublishParams>,
    ) -> Result<String, String> {
        handle_ghost_publish(self, params).await
    }

    #[tool(
        description = "Ghost-in-the-Shell style alias for ghost_publish. Send a ghost whisper to a topic."
    )]
    async fn ghost_whisper(
        &self,
        Parameters(params): Parameters<GhostPublishParams>,
    ) -> Result<String, String> {
        handle_ghost_publish(self, params).await
    }

    #[tool(
        description = "Subscribe to Ghost Whispers topics and get new messages since last poll. Advances the cursor so the same messages are not returned again."
    )]
    async fn ghost_subscribe(
        &self,
        Parameters(params): Parameters<GhostSubscribeParams>,
    ) -> Result<String, String> {
        handle_ghost_subscribe(self, params).await
    }

    #[tool(
        description = "Ghost-in-the-Shell style alias for ghost_subscribe. Listen for new whispers."
    )]
    async fn ghost_listen(
        &self,
        Parameters(params): Parameters<GhostSubscribeParams>,
    ) -> Result<String, String> {
        handle_ghost_subscribe(self, params).await
    }

    #[tool(
        description = "List active Ghost Whispers topics with message counts and last message time."
    )]
    async fn ghost_topics(&self) -> Result<String, String> {
        handle_ghost_topics(self).await
    }

    #[tool(
        description = "Ghost-in-the-Shell style alias for ghost_topics. List active whisper channels."
    )]
    async fn ghost_channels(&self) -> Result<String, String> {
        handle_ghost_topics(self).await
    }

    #[tool(
        description = "Acknowledge a Ghost topic cursor for an agent. Supports explicit index or message_id."
    )]
    async fn ghost_ack(
        &self,
        Parameters(params): Parameters<GhostAckParams>,
    ) -> Result<String, String> {
        handle_ghost_ack(self, params).await
    }

    #[tool(
        description = "Write a Ghost reflection entry and optionally promote it into derived rules."
    )]
    async fn ghost_reflect(
        &self,
        Parameters(params): Parameters<GhostReflectParams>,
    ) -> Result<String, String> {
        handle_ghost_reflect(self, params).await
    }

    #[tool(
        description = "Promote a Ghost message into long-term memory and mark the message as promoted."
    )]
    async fn ghost_promote(
        &self,
        Parameters(params): Parameters<GhostPromoteParams>,
    ) -> Result<String, String> {
        handle_ghost_promote(self, params).await
    }

    // ─── Skill Chaining (Unix Pipe-Style Composition) ────────────────────────

    #[tool(
        description = "Execute a chain of skills in sequence (Unix pipe style). Output of each skill feeds as input to the next."
    )]
    async fn chain_skills(
        &self,
        Parameters(params): Parameters<ChainSkillsParams>,
    ) -> Result<String, String> {
        handle_chain_skills(self, params).await
    }

    // ─── Dead Letter Queue Tools ──────────────────────────────────────────────

    #[tool(
        description = "List dead letter queue entries (failed tool calls). Filter by status: pending, retrying, resolved, abandoned."
    )]
    async fn dlq_list(
        &self,
        Parameters(params): Parameters<DlqListParams>,
    ) -> Result<String, String> {
        handle_dlq_list(self, params).await
    }

    #[tool(
        description = "Manually retry a dead letter queue entry by its ID. Re-dispatches the failed tool call."
    )]
    async fn dlq_retry(
        &self,
        Parameters(params): Parameters<DlqRetryParams>,
    ) -> Result<String, String> {
        handle_dlq_retry(self, params).await
    }

    // ─── Semantic Sandboxing Tools ───────────────────────────────────────────

    #[tool(
        description = "Set a sandbox access rule for an agent role + path pattern. Controls which memories a role can access. Access levels: read, write, deny."
    )]
    async fn sandbox_set_rule(
        &self,
        Parameters(params): Parameters<SandboxSetRuleParams>,
    ) -> Result<String, String> {
        handle_sandbox_set_rule(self, params).await
    }

    #[tool(
        description = "Check if an agent role can access a given path for a specific operation. Advisory mode — not enforced in search_memory yet (TODO: future enforcement integration)."
    )]
    async fn sandbox_check(
        &self,
        Parameters(params): Parameters<SandboxCheckParams>,
    ) -> Result<String, String> {
        handle_sandbox_check(self, params).await
    }

    #[tool(
        description = "Set runtime sandbox policy for a capability (timeouts, concurrency, env allowlist, fs/cwd roots)."
    )]
    async fn sandbox_set_policy(
        &self,
        Parameters(params): Parameters<SandboxSetPolicyParams>,
    ) -> Result<String, String> {
        handle_sandbox_set_policy(self, params).await
    }

    #[tool(
        description = "Ghost-in-the-Shell style alias for sandbox_set_policy. Configure shell execution policy."
    )]
    async fn shell_set_policy(
        &self,
        Parameters(params): Parameters<SandboxSetPolicyParams>,
    ) -> Result<String, String> {
        handle_sandbox_set_policy(self, params).await
    }

    #[tool(description = "Get runtime sandbox policy for a capability.")]
    async fn sandbox_get_policy(
        &self,
        Parameters(params): Parameters<SandboxGetPolicyParams>,
    ) -> Result<String, String> {
        handle_sandbox_get_policy(self, params).await
    }

    #[tool(
        description = "Ghost-in-the-Shell style alias for sandbox_get_policy. Read shell execution policy."
    )]
    async fn shell_get_policy(
        &self,
        Parameters(params): Parameters<SandboxGetPolicyParams>,
    ) -> Result<String, String> {
        handle_sandbox_get_policy(self, params).await
    }

    #[tool(description = "List runtime sandbox policies.")]
    async fn sandbox_list_policies(
        &self,
        Parameters(params): Parameters<SandboxListPoliciesParams>,
    ) -> Result<String, String> {
        handle_sandbox_list_policies(self, params).await
    }

    #[tool(
        description = "Ghost-in-the-Shell style alias for sandbox_list_policies. List shell policies."
    )]
    async fn shell_list_policies(
        &self,
        Parameters(params): Parameters<SandboxListPoliciesParams>,
    ) -> Result<String, String> {
        handle_sandbox_list_policies(self, params).await
    }

    #[tool(
        description = "List sandbox execution audit rows (policy decisions, startup, runtime outcomes)."
    )]
    async fn sandbox_exec_audit(
        &self,
        Parameters(params): Parameters<SandboxExecAuditParams>,
    ) -> Result<String, String> {
        handle_sandbox_exec_audit(self, params).await
    }

    #[tool(
        description = "Ghost-in-the-Shell style alias for sandbox_exec_audit. Inspect shell execution audit."
    )]
    async fn shell_exec_audit(
        &self,
        Parameters(params): Parameters<SandboxExecAuditParams>,
    ) -> Result<String, String> {
        handle_sandbox_exec_audit(self, params).await
    }

    // ─── Pack System Tools ────────────────────────────────────────────────────

    #[tool(description = "List installed skill packs. Optionally filter by enabled_only.")]
    async fn pack_list(
        &self,
        Parameters(params): Parameters<PackListParams>,
    ) -> Result<String, String> {
        handle_pack_list(self, params).await
    }

    #[tool(description = "Get details of a single installed skill pack by ID.")]
    async fn pack_get(
        &self,
        Parameters(params): Parameters<PackGetParams>,
    ) -> Result<String, String> {
        handle_pack_get(self, params).await
    }

    #[tool(
        description = "Register a skill pack after git clone / download. Records the pack in the registry with its metadata, source, and skill count."
    )]
    async fn pack_register(
        &self,
        Parameters(params): Parameters<PackRegisterParams>,
    ) -> Result<String, String> {
        handle_pack_register(self, params).await
    }

    #[tool(
        description = "Remove a skill pack from the registry. Also cleans up projected files in agent directories unless clean_files=false."
    )]
    async fn pack_remove(
        &self,
        Parameters(params): Parameters<PackRemoveParams>,
    ) -> Result<String, String> {
        handle_pack_remove(self, params).await
    }

    #[tool(
        description = "Project a pack's skills, workflows, and host overlays to one or more agents. Converts SKILL.md files to each agent's native format (e.g. .mdc rules for Cursor) and emits a tachi-projection manifest for adapters such as OpenClaw."
    )]
    async fn pack_project(
        &self,
        Parameters(params): Parameters<PackProjectParams>,
    ) -> Result<String, String> {
        handle_pack_project(self, params).await
    }

    #[tool(description = "List agent projections. Filter by agent and/or pack_id.")]
    async fn projection_list(
        &self,
        Parameters(params): Parameters<ProjectionListParams>,
    ) -> Result<String, String> {
        handle_projection_list(self, params).await
    }

    // ─── Domain Management ─────────────────────────────────────────────────

    #[tool(
        description = "Register a domain configuration for memory routing, GC thresholds, and default retention policies."
    )]
    async fn register_domain(
        &self,
        Parameters(params): Parameters<RegisterDomainParams>,
    ) -> Result<String, String> {
        handle_register_domain(self, params).await
    }

    #[tool(description = "Get a domain configuration by name.")]
    async fn get_domain(
        &self,
        Parameters(params): Parameters<GetDomainParams>,
    ) -> Result<String, String> {
        handle_get_domain(self, params).await
    }

    #[tool(description = "List all registered domain configurations.")]
    async fn list_domains(
        &self,
        Parameters(_params): Parameters<ListDomainsParams>,
    ) -> Result<String, String> {
        handle_list_domains(self).await
    }

    #[tool(description = "Delete a domain configuration by name.")]
    async fn delete_domain(
        &self,
        Parameters(params): Parameters<DeleteDomainParams>,
    ) -> Result<String, String> {
        handle_delete_domain(self, params).await
    }

    // ─── Vault (Encrypted Secret Storage) ────────────────────────────────────

    #[tool(description = "Initialize the vault with a master password. Can only be called once.")]
    async fn vault_init(
        &self,
        Parameters(params): Parameters<VaultInitParams>,
    ) -> Result<String, String> {
        handle_vault_init(self, params).await
    }

    #[tool(description = "Unlock the vault by verifying the master password.")]
    async fn vault_unlock(
        &self,
        Parameters(params): Parameters<VaultUnlockParams>,
    ) -> Result<String, String> {
        handle_vault_unlock(self, params).await
    }

    #[tool(description = "Lock the vault (clear encryption key from memory).")]
    async fn vault_lock(&self) -> Result<String, String> {
        handle_vault_lock(self).await
    }

    #[tool(
        description = "Store or update an encrypted secret in the vault. Supports multi-key rotation when name ends with _N."
    )]
    async fn vault_set(
        &self,
        Parameters(params): Parameters<VaultSetParams>,
    ) -> Result<String, String> {
        handle_vault_set(self, params).await
    }

    #[tool(
        description = "Retrieve and decrypt a secret from the vault. Supports auto-rotation for multi-key secrets."
    )]
    async fn vault_get(
        &self,
        Parameters(params): Parameters<VaultGetParams>,
    ) -> Result<String, String> {
        handle_vault_get(self, params).await
    }

    #[tool(
        description = "List all stored secrets (names and metadata only, not values). Does not require vault to be unlocked."
    )]
    async fn vault_list(
        &self,
        Parameters(params): Parameters<VaultListParams>,
    ) -> Result<String, String> {
        handle_vault_list(self, params).await
    }

    #[tool(description = "Delete a secret from the vault.")]
    async fn vault_remove(
        &self,
        Parameters(params): Parameters<VaultRemoveParams>,
    ) -> Result<String, String> {
        handle_vault_remove(self, params).await
    }

    #[tool(description = "Check vault status (initialized, locked/unlocked, entry count).")]
    async fn vault_status(&self) -> Result<String, String> {
        handle_vault_status(self).await
    }

    #[tool(
        description = "Setup key rotation for a prefix. Requires keys like PREFIX_1, PREFIX_2, etc. to already exist."
    )]
    async fn vault_setup_rotation(
        &self,
        Parameters(params): Parameters<VaultSetupRotationParams>,
    ) -> Result<String, String> {
        handle_vault_setup_rotation(self, params).await
    }

    // ─── Facade tools (consolidated surface for Antigravity minimal profile) ──────

    #[tool(
        description = "Unified search across wiki and memory. Use scope to target 'wiki', 'memory', or 'all' (default)."
    )]
    async fn tachi_search(
        &self,
        Parameters(params): Parameters<TachiSearchParams>,
    ) -> Result<String, String> {
        let scope = params.scope.to_ascii_lowercase();
        let mut parts = Vec::new();

        if scope == "wiki" || scope == "all" {
            let wiki_params = WikiSearchParams {
                query: params.query.clone(),
                path_prefix: params.path_prefix.clone(),
                category: params.category.clone(),
                top_k: params.top_k,
                include_archived: params.include_archived,
                agent_role: None,
                project: params.project.clone(),
                domain: params.domain.clone(),
                weights: None,
            };
            let wiki_result = handle_tachi_wiki_search(self, wiki_params).await;
            match wiki_result {
                Ok(r) => parts.push(format!("## Wiki results\n{}", r)),
                Err(e) => parts.push(format!("## Wiki results\nError: {e}")),
            }
        }

        if scope == "memory" || scope == "all" {
            let mem_params = SearchMemoryParams {
                query: params.query.clone(),
                query_vec: None,
                top_k: params.top_k,
                path_prefix: params.path_prefix.clone(),
                include_archived: params.include_archived,
                candidates_per_channel: 20,
                mmr_threshold: Some(0.85),
                graph_expand_hops: 1,
                graph_relation_filter: None,
                weights: None,
                agent_role: None,
                project: params.project.clone(),
                domain: params.domain.clone(),
            };
            let mem_result = handle_search_memory(self, mem_params).await;
            match mem_result {
                Ok(r) => parts.push(format!("## Memory results\n{}", r)),
                Err(e) => parts.push(format!("## Memory results\nError: {e}")),
            }
        }

        Ok(parts.join("\n\n"))
    }

    #[tool(
        description = "Search the live web through Tachi Hub. Uses vc:web_search routing and automatically falls back across available backends."
    )]
    async fn tachi_web_search(
        &self,
        Parameters(params): Parameters<TachiWebSearchParams>,
    ) -> Result<String, String> {
        let (bindings, binding_db) = self.get_virtual_capability_bindings("vc:web_search")?;
        let mut candidates: Vec<String> = bindings
            .into_iter()
            .filter(|binding| binding.enabled)
            .map(|binding| binding.capability_id)
            .collect();

        for fallback in ["mcp:web-search", "mcp:exa", "mcp:tavily"] {
            if !candidates.iter().any(|candidate| candidate == fallback) {
                candidates.push(fallback.to_string());
            }
        }

        let backend_selector = params.backend.as_deref();
        let mut attempts = Vec::new();

        for capability_id in candidates {
            if !matches_web_search_backend(&capability_id, backend_selector) {
                continue;
            }

            let cap = match self.get_capability(&capability_id) {
                Ok(cap) => cap,
                Err(err) => {
                    attempts.push(json!({
                        "capability_id": capability_id,
                        "status": "missing",
                        "error": format!("{err}"),
                    }));
                    continue;
                }
            };

            if !capability_callable(&cap) {
                attempts.push(json!({
                    "capability_id": capability_id,
                    "status": "not_callable",
                    "enabled": cap.enabled,
                    "review_status": cap.review_status,
                    "health_status": cap.health_status,
                }));
                continue;
            }

            let def: Value = match serde_json::from_str(&cap.definition) {
                Ok(def) => def,
                Err(err) => {
                    attempts.push(json!({
                        "capability_id": capability_id,
                        "status": "bad_definition",
                        "error": err.to_string(),
                    }));
                    continue;
                }
            };

            let tool_name =
                choose_web_search_tool(&capability_id, &def, params.tool_name.as_deref());
            let schema = discovered_tool_schema(&def, &tool_name);
            let arguments = web_search_arguments(&params, schema.as_ref());
            let result = self
                .proxy_call_capability_internal(
                    &capability_id,
                    Some("vc:web_search"),
                    &tool_name,
                    Some(arguments.clone()),
                )
                .await;

            match result {
                Ok(result) if !result.is_error.unwrap_or(false) => {
                    let content = first_text_blocks(&result);
                    let response = json!({
                        "query": params.query,
                        "backend": capability_id,
                        "tool": tool_name,
                        "binding_db": binding_db,
                        "arguments": arguments,
                        "content": content,
                        "attempts": attempts,
                    });
                    return serde_json::to_string(&response)
                        .map_err(|err| format!("serialize web search response: {err}"));
                }
                Ok(result) => {
                    attempts.push(json!({
                        "capability_id": capability_id,
                        "tool": tool_name,
                        "status": "tool_error",
                        "content": first_text_blocks(&result),
                    }));
                }
                Err(err) => {
                    attempts.push(json!({
                        "capability_id": capability_id,
                        "tool": tool_name,
                        "status": "call_failed",
                        "error": format!("{err}"),
                    }));
                }
            }
        }

        Err(format!(
            "No web search backend succeeded for query '{}'. Attempts: {}",
            params.query,
            serde_json::to_string(&attempts).unwrap_or_else(|_| "[]".to_string())
        ))
    }

    #[tool(
        description = "Unified save: writes a wiki entry, memory, or quick note. Set kind to 'wiki', 'note', or 'memory' — or omit for auto-detection."
    )]
    async fn tachi_save(
        &self,
        Parameters(params): Parameters<TachiSaveParams>,
    ) -> Result<String, String> {
        let kind = params.kind.as_deref().unwrap_or("").to_ascii_lowercase();

        // Auto-detect: title present → wiki, short text → note, otherwise → memory
        let resolved_kind = if kind.is_empty() {
            if params.title.is_some() {
                "wiki"
            } else if params.text.len() < 200 {
                "note"
            } else {
                "memory"
            }
        } else {
            &kind
        };

        match resolved_kind {
            "wiki" => {
                let title = params
                    .title
                    .clone()
                    .unwrap_or_else(|| "Untitled".to_string());
                let wiki_params = WikiWriteParams {
                    title,
                    text: params.text.clone(),
                    path: params.path.clone(),
                    topic: params.topic.clone(),
                    summary: params.summary.clone(),
                    category: params
                        .category
                        .clone()
                        .unwrap_or_else(|| "experience".to_string()),
                    keywords: params.keywords.clone(),
                    entities: params.entities.clone(),
                    importance: params.importance.unwrap_or(0.85),
                    scope: params.scope.clone().unwrap_or_else(|| "global".to_string()),
                    retention_policy: params
                        .retention_policy
                        .clone()
                        .unwrap_or_else(|| "permanent".to_string()),
                    domain: params.domain.clone(),
                    project: params.project.clone(),
                    force: params.force,
                };
                handle_tachi_wiki_write(self, wiki_params).await
            }
            "note" => {
                let remember_params = RememberParams {
                    text: params.text.clone(),
                    summary: params.summary.clone().unwrap_or_default(),
                    tags: params.keywords.clone(),
                    topic: params.topic.clone().unwrap_or_default(),
                    importance: params.importance,
                    scope: params.scope.clone(),
                    project: params.project.clone(),
                    path: params.path.clone(),
                    category: params.category.clone(),
                    domain: params.domain.clone(),
                    retention_policy: params.retention_policy.clone(),
                    force: params.force,
                };
                handle_remember(self, remember_params).await
            }
            _ => {
                // "memory" or any other value
                let mem_params = SaveMemoryParams {
                    text: params.text.clone(),
                    summary: params.summary.clone().unwrap_or_default(),
                    path: params.path.clone().unwrap_or_else(|| "/".to_string()),
                    importance: params.importance.unwrap_or(0.7),
                    category: params
                        .category
                        .clone()
                        .unwrap_or_else(|| "fact".to_string()),
                    topic: params.topic.clone().unwrap_or_default(),
                    keywords: params.keywords.clone(),
                    persons: Vec::new(),
                    entities: params.entities.clone(),
                    location: String::new(),
                    scope: params
                        .scope
                        .clone()
                        .unwrap_or_else(|| "project".to_string()),
                    vector: None,
                    id: params.id.clone(),
                    force: params.force,
                    auto_link: true,
                    project: params.project.clone(),
                    retention_policy: params.retention_policy.clone(),
                    domain: params.domain.clone(),
                    timestamp: None,
                    metadata: None,
                };
                handle_save_memory(self, mem_params).await
            }
        }
    }

    #[tool(
        description = "Unified handoff: 'leave' a memo for the next session or 'check' for pending memos."
    )]
    async fn tachi_handoff(
        &self,
        Parameters(params): Parameters<TachiHandoffParams>,
    ) -> Result<String, String> {
        let action = params.action.to_ascii_lowercase();
        match action.as_str() {
            "leave" => {
                let summary = params
                    .summary
                    .clone()
                    .ok_or_else(|| "summary is required when action='leave'".to_string())?;
                let leave_params = HandoffLeaveParams {
                    summary,
                    next_steps: params.next_steps.clone(),
                    target_agent: params.target_agent.clone(),
                    context: params.context.clone(),
                };
                handle_handoff_leave(self, leave_params).await
            }
            "check" => {
                let check_params = HandoffCheckParams {
                    agent_id: params.agent_id.clone(),
                    acknowledge: params.acknowledge,
                };
                handle_handoff_check(self, check_params).await
            }
            _ => Err(format!(
                "Invalid action '{}'. Use 'leave' or 'check'.",
                params.action
            )),
        }
    }

    #[tool(
        description = "Prepare a task brief before non-trivial work: relevant wiki lessons, memory hits, lightweight skill suggestions, and debugging checklist. (Alias: tachi_task_brief)"
    )]
    async fn tachi_plan(
        &self,
        Parameters(params): Parameters<TaskBriefParams>,
    ) -> Result<String, String> {
        handle_tachi_task_brief(self, params).await
    }

    #[tool(
        description = "Check whether an agent is stuck after repeated attempts. Returns reframe advice, relevant wiki hits, and an ask-codex prompt when useful. (Alias: tachi_progress_check)"
    )]
    async fn tachi_unstick(
        &self,
        Parameters(params): Parameters<ProgressCheckParams>,
    ) -> Result<String, String> {
        handle_tachi_progress_check(self, params).await
    }

    #[tool(
        description = "Browse wiki entries by category. Without a category, returns category stats. Supports short aliases like 'quant', 'engineering', 'tachi', etc. (Alias: wiki_browse)"
    )]
    async fn tachi_browse(
        &self,
        Parameters(params): Parameters<WikiBrowseParams>,
    ) -> Result<String, String> {
        handle_wiki_browse(self, params)
    }

    #[tool(
        description = "Dispatch a task to a delegate agent (Claude Code CLI, Codex CLI, or custom). Assembles prompt with context from memory/wiki + injected skills, spawns agent subprocess, returns structured result. Call tachi_complete afterwards to record the eval."
    )]
    async fn tachi_dispatch(
        &self,
        Parameters(params): Parameters<TachiDispatchParams>,
    ) -> Result<String, String> {
        dispatch_ops::handle_tachi_dispatch(self, params).await
    }

    #[tool(
        description = "Declare task completion and write an entry to the eval ledger. Records agent, outcome, duration, cost, skills used, and (optionally) trajectory/diff for later distillation. Returns a review bundle. Does NOT auto-merge worktrees — use approve_merge for that."
    )]
    async fn tachi_complete(
        &self,
        Parameters(params): Parameters<TachiCompleteParams>,
    ) -> Result<String, String> {
        use chrono::Utc;

        let now = Utc::now();
        let date = now.format("%Y-%m-%d").to_string();
        let ts = now.format("%Y%m%dT%H%M%SZ").to_string();

        let task_id = params.task_id.clone().unwrap_or_else(|| {
            let agent_slug = params
                .agent
                .replace(|c: char| !c.is_ascii_alphanumeric(), "-")
                .to_ascii_lowercase();
            format!("{}-{}", ts, agent_slug)
        });

        let path = format!("/eval/{}/{}", date, task_id);

        let outcome_norm = params.outcome.to_ascii_lowercase();
        let outcome_emoji = match outcome_norm.as_str() {
            "success" => "✓",
            "failure" => "✗",
            "partial" => "~",
            "aborted" => "⊘",
            _ => "?",
        };

        let duration_display = params
            .duration_ms
            .map(|ms| {
                if ms < 1000 {
                    format!("{}ms", ms)
                } else if ms < 60_000 {
                    format!("{:.1}s", (ms as f64) / 1000.0)
                } else {
                    format!("{:.1}min", (ms as f64) / 60_000.0)
                }
            })
            .unwrap_or_else(|| "?".to_string());

        let cost_display = match (params.cost_tokens, params.cost_usd) {
            (Some(t), Some(u)) => format!(" | {} tok | ${:.4}", t, u),
            (Some(t), None) => format!(" | {} tok", t),
            (None, Some(u)) => format!(" | ${:.4}", u),
            _ => String::new(),
        };

        let mut summary_lines = vec![format!(
            "[{}] {} completed task in {}{}",
            outcome_emoji, params.agent, duration_display, cost_display
        )];
        summary_lines.push(format!("Task: {}", params.task));
        if !params.skills_used.is_empty() {
            summary_lines.push(format!("Skills: {}", params.skills_used.join(", ")));
        }
        if let Some(q) = params.quality_score {
            summary_lines.push(format!("Quality: {:.2}", q));
        }
        if let Some(notes) = &params.notes {
            if !notes.is_empty() {
                summary_lines.push(format!("Notes: {}", notes));
            }
        }
        let text = summary_lines.join("\n");

        let mut keywords: Vec<String> = Vec::new();
        keywords.push(params.agent.clone());
        keywords.push(outcome_norm.clone());
        keywords.push("eval".to_string());
        for skill in &params.skills_used {
            keywords.push(skill.clone());
        }

        let mut metadata_map = serde_json::Map::new();
        metadata_map.insert("task_id".into(), serde_json::json!(task_id));
        metadata_map.insert("agent".into(), serde_json::json!(params.agent));
        metadata_map.insert("outcome".into(), serde_json::json!(outcome_norm));
        if let Some(ms) = params.duration_ms {
            metadata_map.insert("duration_ms".into(), serde_json::json!(ms));
        }
        if !params.skills_used.is_empty() {
            metadata_map.insert(
                "skills_used".into(),
                serde_json::json!(params.skills_used),
            );
        }
        if let Some(t) = params.cost_tokens {
            metadata_map.insert("cost_tokens".into(), serde_json::json!(t));
        }
        if let Some(u) = params.cost_usd {
            metadata_map.insert("cost_usd".into(), serde_json::json!(u));
        }
        if let Some(q) = params.quality_score {
            metadata_map.insert("quality_score".into(), serde_json::json!(q));
        }
        if let Some(traj) = &params.trajectory {
            metadata_map.insert("trajectory".into(), traj.clone());
        }
        if let Some(diff) = &params.diff {
            if !diff.is_empty() {
                metadata_map.insert("diff".into(), serde_json::json!(diff));
            }
        }
        if let Some(wt) = &params.worktree {
            metadata_map.insert("worktree".into(), serde_json::json!(wt));
        }
        if let Some(did) = &params.dispatch_id {
            metadata_map.insert("dispatch_id".into(), serde_json::json!(did));
        }

        let mem_params = SaveMemoryParams {
            text,
            summary: format!(
                "[{}] {} / {}",
                outcome_emoji, params.agent, params.task
            ),
            path: path.clone(),
            importance: match outcome_norm.as_str() {
                "success" => 0.55,
                "failure" => 0.75,
                "partial" => 0.6,
                "aborted" => 0.5,
                _ => 0.5,
            },
            category: "experience".to_string(),
            topic: params.task.clone(),
            keywords,
            persons: Vec::new(),
            entities: params.skills_used.clone(),
            location: String::new(),
            scope: params
                .scope
                .clone()
                .unwrap_or_else(|| "project".to_string()),
            vector: None,
            id: None,
            force: false,
            auto_link: true,
            project: params.project.clone(),
            retention_policy: None,
            domain: None,
            timestamp: None,
            metadata: Some(serde_json::Value::Object(metadata_map)),
        };

        let save_result = handle_save_memory(self, mem_params).await?;
        let save_json: serde_json::Value = serde_json::from_str(&save_result)
            .unwrap_or_else(|_| serde_json::json!({"raw": save_result}));

        let review_bundle = serde_json::json!({
            "recorded": true,
            "task_id": task_id,
            "path": path,
            "outcome": outcome_norm,
            "eval_entry": save_json,
            "next_steps": [
                "Use tachi_search with 'eval' keyword to find related outcomes.",
                "For worktree-based dispatch, run approve_merge / task_merge when ready.",
                "Distill and skill_evolve pipelines are not yet auto-triggered (MVP).",
            ],
            "pending_pipeline": {
                "distill_trajectory": "not yet auto-enqueued (pending worker)",
                "skill_evolve": "not yet auto-enqueued (pending worker)",
            }
        });

        serde_json::to_string(&review_bundle)
            .map_err(|e| format!("Failed to serialize review bundle: {}", e))
    }
}

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
