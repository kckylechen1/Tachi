use super::cache::{ToolDiscovery, DEFAULT_MCP_DISCOVERY_TIMEOUT_MS};
use super::runtime::{
    AgentRuntime, EnrichmentRuntime, FoundryRuntime, ENRICH_CHANNEL_CAPACITY,
    FOUNDRY_CHANNEL_CAPACITY,
};
use super::tachi_server::MemoryServer;
use super::{
    configured_memory_read_pool_size, DbRuntime, DbScope, ProjectDbState, RateLimiter,
    ReadStorePool, VaultState, DEFAULT_RATE_LIMIT_BURST, DEFAULT_RATE_LIMIT_RPM,
};
use crate::builtins::seed_builtin_capabilities;
use crate::foundry_runtime_ops::{
    run_foundry_maintenance_worker, FoundryMaintenanceItem, FoundryWorkerStats,
};
use crate::mcp_pool::McpClientPool;
use crate::mcp_proxy::McpToolExposureMode;
use crate::memory_search_ops::routing_config::RoutingConfigProvider;
use crate::utils::parse_env_u64;
use memcore::MemoryStore;
use memcore::{DbOpenContext, MigrationAuthority, OpenIntent};
use std::collections::{HashMap, VecDeque};
use std::path::PathBuf;
use std::sync::{Arc, Mutex as StdMutex, RwLock as StdRwLock};
use tokio::sync::mpsc;

#[cfg(test)]
thread_local! {
    static TEST_BACKGROUND_WORKERS_OVERRIDE: std::cell::Cell<Option<bool>> = const {
        std::cell::Cell::new(None)
    };
}

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
        TEST_BACKGROUND_WORKERS_OVERRIDE
            .get()
            .unwrap_or_else(|| env_truthy("TACHI_TEST_ENABLE_BACKGROUND_WORKERS"))
    }
    #[cfg(not(test))]
    {
        !embedded_mcp_facade()
    }
}

fn read_bound_agent_id_from_env() -> Option<String> {
    std::env::var("TACHI_AGENT_ID")
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

/// Parse `TACHI_VAULT_AUTOLOCK_SECS`. Default 1800 (30 min). `0` = never
/// auto-lock (for long-running daemons). Non-numeric → warn + default.
fn parse_auto_lock_secs() -> u64 {
    const DEFAULT: u64 = 1800;
    match std::env::var("TACHI_VAULT_AUTOLOCK_SECS") {
        Ok(raw) => match raw.trim().parse::<u64>() {
            Ok(0) => {
                tracing::warn!(
                    "[vault] TACHI_VAULT_AUTOLOCK_SECS=0 — auto-lock disabled (daemon mode)"
                );
                0
            }
            Ok(secs) => secs,
            Err(_) => {
                tracing::warn!(
                    "[vault] TACHI_VAULT_AUTOLOCK_SECS='{raw}' is not a valid non-negative integer — using default {DEFAULT}"
                );
                DEFAULT
            }
        },
        Err(_) => DEFAULT,
    }
}

impl MemoryServer {
    #[cfg(test)]
    pub(crate) fn new_with_background_workers_for_test(
        global_db_path: PathBuf,
        project_db_path: Option<PathBuf>,
        enabled: bool,
    ) -> Result<Self, Box<dyn std::error::Error>> {
        TEST_BACKGROUND_WORKERS_OVERRIDE.with(|override_value| {
            let previous = override_value.replace(Some(enabled));
            let result = Self::new(global_db_path, project_db_path);
            override_value.set(previous);
            result
        })
    }

    /// Fail-closed constructor: no schema-migration authority
    /// ([`MigrationAuthority::Deny`]). Every existing caller (tests, CLI tools
    /// that are not the deploy daemon) keeps this behavior — a fresh DB
    /// builds, a current DB opens, but an older-schema DB refuses to migrate
    /// in place (#1119). The deploy path uses
    /// [`Self::new_with_migration_authority`].
    pub(crate) fn new(
        global_db_path: PathBuf,
        project_db_path: Option<PathBuf>,
    ) -> Result<Self, Box<dyn std::error::Error>> {
        Self::new_with_migration_authority(
            global_db_path,
            project_db_path,
            MigrationAuthority::Deny,
        )
    }

    /// #1119: construct with an explicit schema-migration authority threaded
    /// down to every write-open point (global store, the initial project
    /// store, and the [`DbRuntime`] that owns *dynamic* project opens). Callers
    /// pass [`MigrationAuthority::Allow`] only from an explicit
    /// `--allow-schema-migration` CLI decision; ordinary constructors remain
    /// fail-closed.
    pub(crate) fn new_with_migration_authority(
        global_db_path: PathBuf,
        project_db_path: Option<PathBuf>,
        schema_migration: MigrationAuthority,
    ) -> Result<Self, Box<dyn std::error::Error>> {
        ensure_db_parent(&global_db_path)?;
        if let Some(project_db_path) = project_db_path.as_ref() {
            ensure_db_parent(project_db_path)?;
        }

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
        let global_open_ctx = DbOpenContext {
            intent: OpenIntent::OpenExisting,
            migration: schema_migration.clone(),
        };
        let global_store =
            MemoryStore::open_with_label_and_context(global_db_str, "global", &global_open_ctx)?;
        let read_pool_size = configured_memory_read_pool_size();
        let global_read_pool = ReadStorePool::open_read_only(global_db_str, read_pool_size)?;
        let global_vec_available = global_store.vec_available;

        let project_db_state = if let Some(ref p) = project_db_path {
            Some(
                ProjectDbState::open(p.clone(), read_pool_size, &schema_migration)
                    .map_err(std::io::Error::other)?,
            )
        } else {
            None
        };

        let llm = Arc::new(tachi_llm::LlmClient::new_with_vault_db(Some(
            global_db_path.as_path(),
        ))?);
        // Resolved once, ahead of `llm_recorder` construction, so both the
        // server's own `home_dir` field and the recorder bind to the SAME
        // resolution instead of each independently re-reading
        // TACHI_HOME/SIGIL_HOME/TACHI_APP_HOME (#1096 leaf-2a).
        let home_dir = Arc::new(crate::path_utils::tachi_home());
        let routing_config = Arc::new(RoutingConfigProvider::new((*home_dir).clone()));
        // #1261: `CLAUDE_POOL_MAX_CONCURRENT` env name is kept for back-compat
        // (existing deployments pin it); it now controls the LLM-call
        // recorder's bounded concurrency, not a CLI pool. A rename to
        // `LLM_RECORDER_MAX_CONCURRENT` is tracked as follow-up.
        let recorder_max = std::env::var("CLAUDE_POOL_MAX_CONCURRENT")
            .ok()
            .and_then(|v| v.parse::<usize>().ok())
            .unwrap_or(tachi_llm::llm_recorder::DEFAULT_MAX_CONCURRENT);
        let llm_recorder = Arc::new(tachi_llm::llm_recorder::LlmCallRecorder::new_in_app_home(
            recorder_max,
            (*home_dir).clone(),
        ));
        let pipeline_enabled = std::env::var("ENABLE_PIPELINE")
            .map(|v| v == "true" || v == "1")
            .unwrap_or(false);
        let bound_agent_id = read_bound_agent_id_from_env();
        if bound_agent_id.is_none() {
            tracing::warn!(
                "TACHI_AGENT_ID is not set; vault ACL falls back to caller-supplied agent_id"
            );
        }
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

        let db = DbRuntime {
            global_store: Arc::new(StdMutex::new(global_store)),
            global_read_pool,
            global_rw_gate: Arc::new(StdRwLock::new(())),
            global_contention_recorder: Arc::new(std::sync::OnceLock::new()),
            global_db_path: Arc::new(global_db_path),
            global_vec_available,
            project_db: Arc::new(StdRwLock::new(project_db_state)),
            attached_project_dbs: Arc::new(StdRwLock::new(HashMap::new())),
            project_attach_init_gate: Arc::new(StdMutex::new(())),
            schema_migration,
        };

        let server = Self {
            db,
            llm,
            llm_recorder,
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
            // graph/state primitives (add_edge/get_edges/memory_graph/
            // set_state/get_state) were deleted outright (#757 surface prune;
            // #913 dead-code round found zero remaining in-crate callers).
            tool_router: Self::continuity_tool_router()
                + Self::component_tool_router()
                + Self::copilot_tool_router()
                + Self::dispatch_tool_router()
                + Self::handoff_tool_router()
                + Self::runtime_context_tool_router()
                + Self::hub_tool_router()
                + Self::pipeline_tool_router()
                + Self::kanban_tool_router()
                + Self::memory_tool_router()
                + Self::vault_tool_router()
                + Self::workflow_tool_router()
                + Self::wiki_tool_router()
                + Self::sandbox_tool_router()
                + Self::peer_tool_router(),
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
                auto_lock_after_secs: parse_auto_lock_secs(),
            })),
            rate_limiter: Arc::new(StdMutex::new(RateLimiter {
                windows: HashMap::new(),
                bursts: HashMap::new(),
                rpm: parse_env_u64("RATE_LIMIT_RPM").unwrap_or(DEFAULT_RATE_LIMIT_RPM),
                burst: parse_env_u64("RATE_LIMIT_BURST").unwrap_or(DEFAULT_RATE_LIMIT_BURST),
            })),
            agent_runtime: Arc::new(StdRwLock::new(AgentRuntime {
                agent_profile: None,
                tool_profile: Some(tachi_hub::default_tool_profile()),
                session_client: None,
                session_project: None,
                work_claim_connection: None,
                session_dispatch_depth: None,
                rate_limit_session_id: uuid::Uuid::new_v4().to_string(),
            })),
            bound_agent_id: Arc::new(StdRwLock::new(bound_agent_id)),
            home_dir,
            routing_config,
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
            {
                let auto_ingest_server = server.clone();
                tokio::spawn(crate::pipeline_ops::run_auto_ingest_replay_consumer(
                    auto_ingest_server,
                ));
            }

            // Replay pending foundry jobs from DB (survive process restart)
            {
                let replay_server = server.clone();
                tokio::spawn(async move {
                    // Short delay to let the foundry worker start receiving
                    tokio::time::sleep(std::time::Duration::from_millis(200)).await;

                    let mut replayed = 0usize;

                    // Helper: replay jobs from a store
                    let replay_from = |jobs: Vec<memcore::PersistedFoundryJob>| -> usize {
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
                        memcore::load_pending_foundry_jobs(store.connection(), &running_cutoff)
                            .map_err(|e| format!("load pending foundry jobs (global): {e}"))
                    }) {
                        replayed += replay_from(jobs);
                    }

                    // Replay from project DB
                    if let Ok(jobs) = replay_server.with_project_store(|store| {
                        memcore::load_pending_foundry_jobs(store.connection(), &running_cutoff)
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

fn ensure_db_parent(path: &PathBuf) -> Result<(), Box<dyn std::error::Error>> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::MemoryServer;
    use chrono::Utc;
    use memcore::MemoryEntry;
    use serde_json::json;

    fn test_entry(id: &str) -> MemoryEntry {
        MemoryEntry {
            id: id.to_string(),
            path: "/".to_string(),
            summary: String::new(),
            text: "test memory".to_string(),
            importance: 0.7,
            timestamp: Utc::now().to_rfc3339(),
            valid_from: String::new(),
            valid_until: None,
            category: "fact".to_string(),
            topic: String::new(),
            keywords: Vec::new(),
            persons: Vec::new(),
            entities: Vec::new(),
            location: String::new(),
            source: "test".to_string(),
            scope: "general".to_string(),
            archived: false,
            access_count: 0,
            last_access: None,
            revision: 1,
            metadata: json!({}),
            vector: None,
            retention_policy: None,
            domain: None,
            recall_count: 0,
            query_diversity: 0,
            tier: "raw".to_string(),
        }
    }

    #[test]
    fn memory_server_new_creates_global_and_project_db_parents() {
        let temp = tempfile::tempdir().expect("tempdir");
        let global_db = temp.path().join("nested/global/memory.db");
        let project_db = temp.path().join("nested/project/memory.db");

        let server = MemoryServer::new(global_db.clone(), Some(project_db.clone()))
            .expect("server should create missing db parent dirs");

        assert!(global_db.parent().expect("global parent").is_dir());
        assert!(project_db.parent().expect("project parent").is_dir());
        assert_eq!(server.global_db_path_buf(), global_db);
        assert_eq!(server.project_db_path_buf(), Some(project_db));
        assert!(server.has_project_db());

        server
            .with_project_store(|store| {
                store
                    .upsert(&test_entry("startup-project-read-visible"))
                    .map_err(|e| format!("project upsert failed: {e}"))
            })
            .expect("startup project writer should be active");
        let found = server
            .with_project_store_read(|store| {
                store
                    .get("startup-project-read-visible")
                    .map_err(|e| format!("project read get failed: {e}"))
            })
            .expect("startup project read pool should be active");
        assert_eq!(
            found.expect("project entry exists").id,
            "startup-project-read-visible"
        );
    }

    /// #1096 leaf-2a round-2 (codex B2): pins the invariant documented on
    /// `MemoryServer::home_dir` — a normally-constructed server's frozen
    /// `tachi_home_dir()` must equal a live re-read of the canonical funnel
    /// (`path_utils::tachi_home()`), because production never mutates
    /// `TACHI_HOME`/`SIGIL_HOME`/`TACHI_APP_HOME` after constructing a
    /// server. If this test starts failing, something is re-deriving home
    /// AFTER construction with different env than construction saw — fix by
    /// setting env before `MemoryServer::new`, not by moving more call sites
    /// off the frozen field.
    #[test]
    fn memory_server_tachi_home_dir_matches_canonical_resolution() {
        let _lock = crate::utils::global_test_lock()
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let temp_home = tempfile::tempdir().expect("tachi home tempdir");
        let _tachi_home = crate::test_support::EnvRestore::set_path("TACHI_HOME", temp_home.path());
        let _sigil_home = crate::test_support::EnvRestore::remove("SIGIL_HOME");
        let _app_home = crate::test_support::EnvRestore::remove("TACHI_APP_HOME");

        let global_db = temp_home.path().join("global/memory.db");
        let server = MemoryServer::new(global_db, None).expect("server construction");

        assert_eq!(server.tachi_home_dir(), crate::path_utils::tachi_home());
    }
}
