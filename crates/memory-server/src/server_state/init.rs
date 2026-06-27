use super::cache::{ToolDiscovery, DEFAULT_MCP_DISCOVERY_TIMEOUT_MS};
use super::memory_server::MemoryServer;
use super::read_pool::{configured_memory_read_pool_size, ReadStorePool};
use super::runtime::{
    AgentRuntime, DbScope, EnrichmentRuntime, FoundryRuntime, ProjectDbState, RateLimiter,
    VaultState, DEFAULT_RATE_LIMIT_BURST, DEFAULT_RATE_LIMIT_RPM, ENRICH_CHANNEL_CAPACITY,
    FOUNDRY_CHANNEL_CAPACITY,
};
use crate::builtins::seed_builtin_capabilities;
use crate::claude_pool;
use crate::foundry_runtime_ops::{
    run_foundry_maintenance_worker, FoundryMaintenanceItem, FoundryWorkerStats,
};
use crate::llm;
use crate::mcp_pool::McpClientPool;
use crate::mcp_proxy::McpToolExposureMode;
use crate::utils::parse_env_u64;
use memory_core::MemoryStore;
use std::collections::{HashMap, VecDeque};
use std::path::PathBuf;
use std::sync::{Arc, Mutex as StdMutex, RwLock as StdRwLock};
use tokio::sync::mpsc;

fn env_truthy(name: &str) -> bool {
    std::env::var(name)
        .map(|value| {
            let value = value.trim();
            value == "1" || value.eq_ignore_ascii_case("true") || value.eq_ignore_ascii_case("yes")
        })
        .unwrap_or(false)
}

fn embedded_mcp_facade() -> bool {
    env_truthy("TACHI_EMBEDDED_MCP") && !crate::cli_client::is_daemon_process()
}

fn background_workers_enabled() -> bool {
    #[cfg(test)]
    {
        env_truthy("TACHI_TEST_ENABLE_BACKGROUND_WORKERS")
    }
    #[cfg(not(test))]
    {
        !embedded_mcp_facade()
    }
}

impl MemoryServer {
    pub(crate) fn new(
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
        let read_pool_size = configured_memory_read_pool_size();
        let global_read_pool = ReadStorePool::open_read_only(global_db_str, read_pool_size)?;
        let global_vec_available = global_store.vec_available;

        let (
            project_store,
            project_read_pool,
            project_rw_gate,
            project_db_path,
            project_vec_available,
        ) = if let Some(ref p) = project_db_path {
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
            let read_pool = ReadStorePool::open_read_only(project_db_str, read_pool_size)?;
            let v = store.vec_available;
            (
                Some(Arc::new(StdMutex::new(store))),
                Some(read_pool),
                Some(Arc::new(StdRwLock::new(()))),
                Some(Arc::new(p.clone())),
                v,
            )
        } else {
            (None, None, None, None, false)
        };

        let llm = Arc::new(llm::LlmClient::new_with_vault_db(Some(
            global_db_path.as_path(),
        ))?);
        let claude_pool_max = std::env::var("CLAUDE_POOL_MAX_CONCURRENT")
            .ok()
            .and_then(|v| v.parse::<usize>().ok())
            .unwrap_or(claude_pool::DEFAULT_MAX_CONCURRENT);
        let claude_pool = Arc::new(claude_pool::ClaudePool::new(claude_pool_max));
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

        let (enrich_tx, enrich_rx) = mpsc::channel(ENRICH_CHANNEL_CAPACITY);
        let (foundry_tx, foundry_rx) = mpsc::channel(FOUNDRY_CHANNEL_CAPACITY);
        let foundry_stats = Arc::new(FoundryWorkerStats::default());

        // Build hot-swap state before moving project_store into the struct
        let hot_project_db = Arc::new(StdRwLock::new(
            match (
                project_store.as_ref(),
                project_read_pool.as_ref(),
                project_rw_gate.clone(),
                project_db_path.clone(),
            ) {
                (Some(store), Some(read_pool), Some(rw_gate), Some(db_path)) => {
                    Some(ProjectDbState {
                        store: Arc::clone(store),
                        read_pool: read_pool.clone(),
                        rw_gate,
                        db_path,
                    })
                }
                _ => None,
            },
        ));

        let server = Self {
            global_store: Arc::new(StdMutex::new(global_store)),
            global_read_pool,
            project_store,
            project_read_pool,
            global_rw_gate: Arc::new(StdRwLock::new(())),
            project_rw_gate,
            global_db_path: Arc::new(global_db_path),
            project_db_path,
            global_vec_available,
            project_vec_available,
            hot_project_db,
            llm,
            claude_pool,
            pipeline_enabled,
            tool_discovery: Arc::new(ToolDiscovery {
                proxy_tools: StdMutex::new(HashMap::new()),
                skill_tools: StdMutex::new(HashMap::new()),
                skill_tool_defs: StdMutex::new(HashMap::new()),
                tool_cache: StdMutex::new(HashMap::new()),
                dead_letters: StdMutex::new(VecDeque::new()),
                mcp_discovery_timeout: std::time::Duration::from_millis(mcp_discovery_timeout_ms),
                mcp_tool_exposure_mode,
            }),
            pool: Arc::new(McpClientPool::new()),
            tool_router: Self::tool_router()
                + Self::pack_tool_router()
                + Self::domain_tool_router()
                + Self::vault_tool_router()
                + Self::sandbox_tool_router(),
            cache_hits: Arc::new(std::sync::atomic::AtomicU64::new(0)),
            cache_misses: Arc::new(std::sync::atomic::AtomicU64::new(0)),
            last_activity_ms: Arc::new(std::sync::atomic::AtomicI64::new(
                chrono::Utc::now().timestamp_millis(),
            )),
            enrichment: EnrichmentRuntime { enrich_tx },
            foundry: FoundryRuntime {
                foundry_tx,
                foundry_stats,
            },
            vault: Arc::new(StdRwLock::new(VaultState {
                key: None,
                unlock_time: None,
                failed_attempts: (0, None),
                auto_lock_after_secs: 1800,
            })),
            rate_limiter: Arc::new(StdMutex::new(RateLimiter {
                windows: HashMap::new(),
                bursts: HashMap::new(),
                rpm: parse_env_u64("RATE_LIMIT_RPM").unwrap_or(DEFAULT_RATE_LIMIT_RPM),
                burst: parse_env_u64("RATE_LIMIT_BURST").unwrap_or(DEFAULT_RATE_LIMIT_BURST),
            })),
            agent_runtime: Arc::new(StdRwLock::new(AgentRuntime {
                agent_profile: None,
                tool_profile: Some(crate::profiles::default_tool_profile()),
                handoff_memos: Vec::new(),
            })),
        };

        if background_workers_enabled() {
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
                                db_path: None,
                                path_prefix: job.path_prefix,
                                memory_ids: job.memory_ids,
                                counted_queue_slot: false,
                            };
                            if replay_server
                                .foundry_lock()
                                .foundry_tx
                                .try_send(item)
                                .is_ok()
                            {
                                count += 1;
                            }
                        }
                        count
                    };

                    let running_cutoff = (chrono::Utc::now()
                        - chrono::Duration::seconds(crate::status_ops::STUCK_THRESHOLD_SECS))
                    .to_rfc3339();

                    // Replay from global DB
                    if let Ok(jobs) = replay_server.with_global_store(|store| {
                        memory_core::load_pending_foundry_jobs(store.connection(), &running_cutoff)
                            .map_err(|e| format!("load pending foundry jobs (global): {e}"))
                    }) {
                        replayed += replay_from(jobs);
                    }

                    // Replay from project DB
                    if let Ok(jobs) = replay_server.with_project_store(|store| {
                        memory_core::load_pending_foundry_jobs(store.connection(), &running_cutoff)
                            .map_err(|e| format!("load pending foundry jobs (project): {e}"))
                    }) {
                        replayed += replay_from(jobs);
                    }

                    if replayed > 0 {
                        eprintln!("[foundry] replayed {replayed} pending jobs from DB");
                    }
                });
            }
        } else if embedded_mcp_facade() {
            eprintln!(
                "[embedded-mcp] background enrichment/foundry workers disabled; scoped daemon owns queues"
            );
        }

        seed_builtin_capabilities(&server)
            .map_err(|e| std::io::Error::other(format!("seed builtin capabilities: {e}")))?;

        Ok(server)
    }
}
