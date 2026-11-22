use super::cache::ToolDiscovery;
use super::runtime::{
    AgentRuntime, EnrichmentRuntime, FoundryRuntime, ENRICH_CHANNEL_CAPACITY,
    FOUNDRY_CHANNEL_CAPACITY,
};
use super::tachi_server::MemoryServer;
use super::{
    configured_memory_read_pool_size, DbRuntime, DbScope, ProjectDbState, RateLimiter,
    ReadStorePool, StoreLabel, VaultState, DEFAULT_RATE_LIMIT_BURST, DEFAULT_RATE_LIMIT_RPM,
};
use crate::builtins::seed_builtin_capabilities;
use crate::foundry_runtime_ops::{
    run_foundry_maintenance_worker, FoundryMaintenanceItem, FoundryWorkerStats,
};
use crate::memory_search_ops::routing_config::RoutingConfigProvider;
use crate::utils::parse_env_u64;
use memcore::MemoryStore;
use memcore::{DbOpenContext, MigrationAuthority, ProfileRequirement, StoreProfile};
use std::collections::{HashMap, VecDeque};
use std::path::PathBuf;
use std::sync::{Arc, Mutex as StdMutex, RwLock as StdRwLock};
use tokio::sync::mpsc;

#[cfg(any(test, feature = "delivery-test-api", feature = "vault-test-api"))]
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
        // Tests run in parallel, so a process-global environment variable
        // cannot safely choose whether an otherwise unrelated constructor
        // calls `tokio::spawn`. Tests that exercise workers must opt in via
        // `new_with_background_workers_for_test` on their own thread.
        TEST_BACKGROUND_WORKERS_OVERRIDE.get().unwrap_or(false)
    }
    #[cfg(not(test))]
    {
        #[cfg(any(feature = "delivery-test-api", feature = "vault-test-api"))]
        if let Some(enabled) = TEST_BACKGROUND_WORKERS_OVERRIDE.get() {
            return enabled;
        }
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

    #[cfg(feature = "delivery-test-api")]
    pub(crate) fn new_isolated_for_test(
        global_db_path: PathBuf,
        project_db_path: Option<PathBuf>,
    ) -> Result<Self, Box<dyn std::error::Error>> {
        TEST_BACKGROUND_WORKERS_OVERRIDE.with(|override_value| {
            let previous = override_value.replace(Some(false));
            let result = Self::new(global_db_path, project_db_path);
            override_value.set(previous);
            result
        })
    }

    #[cfg(any(test, feature = "delivery-test-api", feature = "vault-test-api"))]
    pub(crate) fn new_isolated_with_home_for_test(
        global_db_path: PathBuf,
        project_db_path: Option<PathBuf>,
        home_dir: PathBuf,
    ) -> Result<Self, Box<dyn std::error::Error>> {
        TEST_BACKGROUND_WORKERS_OVERRIDE.with(|override_value| {
            let previous = override_value.replace(Some(false));
            let result = Self::new_with_migration_authority_and_home(
                global_db_path,
                project_db_path,
                MigrationAuthority::Deny,
                home_dir,
            );
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

    #[cfg(test)]
    pub(crate) fn new_with_home_for_test(
        global_db_path: PathBuf,
        project_db_path: Option<PathBuf>,
        home_dir: PathBuf,
    ) -> Result<Self, Box<dyn std::error::Error>> {
        Self::new_with_migration_authority_and_home(
            global_db_path,
            project_db_path,
            MigrationAuthority::Deny,
            home_dir,
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
        Self::new_with_migration_authority_and_home(
            global_db_path,
            project_db_path,
            schema_migration,
            crate::path_utils::tachi_home(),
        )
    }

    fn new_with_migration_authority_and_home(
        global_db_path: PathBuf,
        project_db_path: Option<PathBuf>,
        schema_migration: MigrationAuthority,
        home_dir: PathBuf,
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
            migration: schema_migration.clone(),
            // #1585 D2: the server's global store backs the full product
            // surface (Vault, Hub, Foundry, dispatch ledgers).
            required_profile: ProfileRequirement::AtLeast(StoreProfile::TachiFull),
            ..DbOpenContext::open_existing_deny()
        };
        // tachi#1579: `"global"` here is a RESOLVED role, not a guess — this is
        // the server's own global store by construction — so it is a legitimate
        // conferral. On a store already stamped `global` it verifies; on one
        // stamped anything else the open fails loudly rather than the server
        // quietly operating someone else's database as its global store.
        // #1585 D5: resolve `TACHI_*` / `config.env` exactly ONCE, here, before
        // the first store opens — `memcore` reads no environment of its own
        // any more. Every store this server owns (global write handle, global
        // read pool, the bound project DB, and every dynamic project open the
        // `DbRuntime` performs later) carries this same resolution, so read
        // and write handles for one file cannot rank with different weights.
        let kernel_policy = crate::kernel_policy_adapter::resolve_kernel_policy();
        let global_store =
            MemoryStore::open_with_label_and_context(global_db_str, "global", &global_open_ctx)?
                .with_kernel_policy(kernel_policy.clone());
        // #1696: memcore's admitted open above owns schema installation and
        // migration. This second typed connection performs no DDL and refuses
        // a missing/drifted CurrentTruth schema.
        let current_truth =
            tachi_params::current_truth::store::CurrentTruthSqliteStore::open_existing(
                global_db_str,
            )?;
        let read_pool_size = configured_memory_read_pool_size();
        // Same label as the write store two lines up (tachi#1569): the read
        // pool's handles must not disagree with it about which store this is.
        // Taken from the resolved handle rather than re-typed, so the two
        // cannot drift.
        let global_read_pool = ReadStorePool::open_read_only(
            global_db_str,
            read_pool_size,
            global_store.db_label(),
            &kernel_policy,
        )?;
        let global_vec_available = global_store.vec_available;

        let project_db_state = if let Some(ref p) = project_db_path {
            Some(
                // The `--project-db` path is an operator-supplied path with no
                // resolved manifest role, so it confers nothing: the store's
                // own stamp answers (tachi#1579).
                ProjectDbState::open(
                    p.clone(),
                    read_pool_size,
                    &schema_migration,
                    StoreLabel::inferred(p),
                    &kernel_policy,
                )
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
        let home_dir = Arc::new(home_dir);
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
        let (enrich_tx, enrich_rx) = mpsc::channel(ENRICH_CHANNEL_CAPACITY);
        let mut enrich_rx = Some(enrich_rx);
        let (foundry_tx, foundry_rx) = mpsc::channel(FOUNDRY_CHANNEL_CAPACITY);
        let foundry_stats = Arc::new(FoundryWorkerStats::default());
        let start_background_workers = background_workers_enabled();
        #[cfg(test)]
        let retained_enrich_rx = Arc::new(StdMutex::new(if start_background_workers {
            None
        } else {
            enrich_rx.take()
        }));

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
            // #1585 D5: the same single resolution the startup opens above
            // used, carried so dynamic project opens (activate/attach,
            // request-scoped path reads) get it too instead of silently
            // falling back to `RecallConfig::default()`.
            kernel_policy,
        };

        let server = Self {
            #[cfg(any(unix, windows))]
            task_notifications: Arc::new(super::task_notifications::Registry::default()),
            #[cfg(any(unix, windows))]
            native_acp_approvals: Arc::new(crate::dispatch_ops::NativeAcpApprovals::default()),
            managed_run_controls: Arc::new(
                crate::managed_run_control::ManagedRunControlRegistry::default(),
            ),
            controller_epoch: crate::managed_run_epoch::mint_controller_epoch(),
            startup_reconciliation: Arc::new(std::sync::OnceLock::new()),
            db,
            current_truth: Arc::new(StdMutex::new(current_truth)),
            llm,
            llm_recorder,
            pipeline_enabled,
            tool_discovery: Arc::new(ToolDiscovery {
                skill_tools: StdMutex::new(HashMap::new()),
                skill_tool_defs: StdMutex::new(HashMap::new()),
                tool_cache: StdMutex::new(HashMap::new()),
                dead_letters: StdMutex::new(VecDeque::new()),
            }),
            tool_router: Self::native_tool_router(),
            cache_hits: Arc::new(std::sync::atomic::AtomicU64::new(0)),
            cache_misses: Arc::new(std::sync::atomic::AtomicU64::new(0)),
            last_activity_ms: Arc::new(std::sync::atomic::AtomicI64::new(
                jiff::Timestamp::now().as_nanosecond().div_euclid(1_000_000) as i64,
            )),
            enrichment: EnrichmentRuntime {
                enrich_tx,
                durable_dispatch_runtime_id: Arc::from(uuid::Uuid::new_v4().to_string()),
                #[cfg(test)]
                retained_enrich_rx,
            },
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
            staff_presence: Arc::new(crate::staffing_ops::presence::Registry::default()),
            request_project: None,
            agent_runtime: Arc::new(StdRwLock::new(AgentRuntime {
                agent_profile: None,
                tool_profile: Some(tachi_hub::default_tool_profile()),
                session_client: None,
                presence_client_label: None,
                presence_session_id: uuid::Uuid::new_v4().to_string(),
                presence_report: None,
                presence_report_error: None,
                session_project: None,
                work_claim_connection: None,
                daemon_proxy_token: None,
                published_daemon_mcp_url: None,
                session_dispatch_depth: None,
                rate_limit_session_id: uuid::Uuid::new_v4().to_string(),
            })),
            bound_agent_id: Arc::new(StdRwLock::new(bound_agent_id)),
            home_dir,
            routing_config,
            skill_quality_refresh: Arc::new(crate::wiki_ops::SkillQualityRefreshQueue::new(
                start_background_workers,
            )),
        };

        if start_background_workers {
            // Spawn the enrichment batcher worker
            {
                let batcher_server = server.clone();
                tokio::spawn(Self::run_enrichment_batcher(
                    batcher_server,
                    enrich_rx
                        .take()
                        .expect("background enrichment worker owns its receiver"),
                ));
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

                    match tokio::task::spawn_blocking(move || {
                        let foundry_tx = replay_server.foundry_lock().foundry_tx.clone();
                        replay_startup_foundry_jobs(&replay_server, &foundry_tx)
                    })
                    .await
                    {
                        Ok(replayed) if replayed > 0 => {
                            eprintln!("[foundry] replayed {replayed} pending jobs from DB");
                        }
                        Err(error) => eprintln!("[foundry] startup replay task failed: {error}"),
                        _ => {}
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

/// Replay one capacity-bounded batch per startup store; the scheduler owns
/// later delivery of the durable tail. Sending does not claim a worker lease.
fn replay_startup_foundry_jobs(
    server: &MemoryServer,
    foundry_tx: &mpsc::Sender<FoundryMaintenanceItem>,
) -> usize {
    let running_cutoff = memcore::time_compat::rfc3339_compat(
        jiff::Timestamp::now()
            - jiff::SignedDuration::from_secs(crate::foundry_runtime_ops::STUCK_THRESHOLD_SECS),
    );
    let mut replayed = 0;
    for (scope, label) in [(DbScope::Global, "global"), (DbScope::Project, "project")] {
        let capacity = foundry_tx.capacity();
        let load = |store: &mut MemoryStore| {
            memcore::load_pending_foundry_jobs_batch(
                store.connection(),
                &running_cutoff,
                capacity,
                |_| true,
            )
            .map_err(|error| format!("load pending foundry jobs ({label}): {error}"))
        };
        let batch = match scope {
            DbScope::Global => server.with_global_store(load),
            DbScope::Project => server.with_project_store(load),
        };
        if let Ok(batch) = batch {
            for job in batch.jobs {
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
                if foundry_tx.try_send(item).is_err() {
                    break;
                }
                replayed += 1;
            }
        }
    }
    replayed
}

fn ensure_db_parent(path: &PathBuf) -> Result<(), Box<dyn std::error::Error>> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{replay_startup_foundry_jobs, DbScope, MemoryServer};
    use memcore::{MemoryEntry, MigrationAuthority};
    use rusqlite::Connection;
    use serde_json::json;

    fn test_entry(id: &str) -> MemoryEntry {
        MemoryEntry {
            id: id.to_string(),
            path: "/".to_string(),
            summary: String::new(),
            text: "test memory".to_string(),
            importance: 0.7,
            timestamp: memcore::time_compat::rfc3339_compat(jiff::Timestamp::now()),
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
            scored_count: 0,
            last_access: None,
            last_use_at: None,
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
    fn memory_server_new_ignores_process_background_worker_env_in_tests() {
        let _lock = crate::utils::global_test_lock()
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let _workers =
            crate::test_support::EnvRestore::set("TACHI_TEST_ENABLE_BACKGROUND_WORKERS", "1");
        let temp = tempfile::tempdir().expect("tempdir");

        let server = MemoryServer::new(temp.path().join("global/memory.db"), None)
            .expect("ambient test env must not enable background workers");

        assert!(!server.has_project_db());
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

    #[test]
    fn startup_foundry_replay_bounds_each_store_and_keeps_the_tail_durable() {
        let temp = tempfile::tempdir().expect("tempdir");
        let server = MemoryServer::new_with_migration_authority_and_home(
            temp.path().join("global/memory.db"),
            Some(temp.path().join("project/memory.db")),
            MigrationAuthority::Deny,
            temp.path().join("home"),
        )
        .expect("isolated server without background workers");
        let seed = |store: &mut memcore::MemoryStore, prefix: &str, target: &str| {
            for index in 0..3 {
                let id = format!("{prefix}{index}");
                store
                    .connection()
                    .execute(
                        "INSERT INTO foundry_jobs
                     (id,kind,lane,status,target_db,path_prefix,memory_ids,created_at,updated_at)
                     VALUES (?1,'forget_sweep','reasoning','queued',?2,?3,?4,
                             '2026-01-01T00:00:00Z','2026-01-01T00:00:00Z')",
                        rusqlite::params![
                            id,
                            target,
                            format!("/replay/{id}"),
                            json!([format!("memory-{id}")]).to_string()
                        ],
                    )
                    .map_err(|error| error.to_string())?;
            }
            Ok(())
        };
        server
            .with_global_store(|store| seed(store, "g", "global"))
            .unwrap();
        server
            .with_project_store(|store| seed(store, "p", "project"))
            .unwrap();
        // These rows are beyond the first available capacity. Eager loading
        // either store's whole payload set would poison the valid FIFO head.
        for scope in [DbScope::Global, DbScope::Project] {
            server
                .with_store_for_scope(scope, |store| {
                    store
                        .connection()
                        .execute(
                            "UPDATE foundry_jobs SET metadata = ?1 WHERE id IN ('g2','p0')",
                            rusqlite::params![vec![0_u8]],
                        )
                        .map(|_| ())
                        .map_err(|error| error.to_string())
                })
                .unwrap();
        }

        let (tx, mut rx) = tokio::sync::mpsc::channel(2);
        let cutoff = memcore::time_compat::rfc3339_compat(
            jiff::Timestamp::now() - jiff::SignedDuration::from_mins(10),
        );
        for (round, expected) in [["g0", "g1"], ["g2", "p0"], ["p1", "p2"]]
            .into_iter()
            .enumerate()
        {
            assert_eq!(replay_startup_foundry_jobs(&server, &tx), 2);
            let items = [rx.try_recv().unwrap(), rx.try_recv().unwrap()];
            assert_eq!(
                items
                    .iter()
                    .map(|item| item.job.id.as_str())
                    .collect::<Vec<_>>(),
                expected
            );
            assert!(rx.try_recv().is_err());
            if round == 0 {
                for scope in [DbScope::Global, DbScope::Project] {
                    server
                        .with_store_for_scope(scope, |store| {
                            let pending = memcore::probe_pending_foundry_jobs(
                                store.connection(),
                                &cutoff,
                                jiff::Timestamp::now(),
                            )
                            .map_err(|error| error.to_string())?;
                            assert_eq!(
                                pending.pending.len(),
                                3,
                                "send must not claim or remove durable rows"
                            );
                            store
                                .connection()
                                .execute("UPDATE foundry_jobs SET metadata = '{}'", [])
                                .map(|_| ())
                                .map_err(|error| error.to_string())
                        })
                        .unwrap();
                }
            }
            if round == 2 {
                let (closed, mut receiver) = tokio::sync::mpsc::channel(2);
                receiver.close();
                assert_eq!(replay_startup_foundry_jobs(&server, &closed), 0);
            }
            for item in items {
                let scope = if item.job.id.starts_with('g') {
                    DbScope::Global
                } else {
                    DbScope::Project
                };
                assert_eq!(item.target_db, scope);
                assert_eq!(item.path_prefix, format!("/replay/{}", item.job.id));
                assert_eq!(item.memory_ids, vec![format!("memory-{}", item.job.id)]);
                assert!(item.named_project.is_none() && item.db_path.is_none());
                assert!(!item.counted_queue_slot);
                server
                    .with_store_for_scope(scope, |store| {
                        assert_eq!(
                            memcore::claim_foundry_job_for_run(
                                store.connection(),
                                &item.job.id,
                                &cutoff
                            )
                            .map_err(|error| error.to_string())?,
                            Some(memcore::FoundryJobLease::Queued),
                            "only the consumer may claim the replayed job"
                        );
                        memcore::update_foundry_job_status_with_reason(
                            store.connection(),
                            &item.job.id,
                            "completed",
                            None,
                        )
                        .map_err(|error| error.to_string())
                    })
                    .unwrap();
            }
        }
        assert_eq!(replay_startup_foundry_jobs(&server, &tx), 0);
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

    #[test]
    fn current_truth_schema_obeys_server_migration_authority() {
        fn sqlite_schema(conn: &Connection) -> Vec<(String, String, Option<String>)> {
            let mut statement = conn
                .prepare(
                    "SELECT type, name, sql FROM sqlite_master
                     WHERE name NOT LIKE 'sqlite_%'
                     ORDER BY type, name",
                )
                .unwrap();
            statement
                .query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))
                .unwrap()
                .collect::<Result<Vec<_>, _>>()
                .unwrap()
        }

        let _lock = crate::utils::global_test_lock()
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let temp = tempfile::tempdir().expect("tempdir");
        let global_db = temp.path().join("global/memory.db");
        let home = temp.path().join("home");

        // Seed a canonically stamped current product DB, then model the exact
        // pre-v38 state an existing deployment presents to server startup.
        drop(
            MemoryServer::new_with_migration_authority_and_home(
                global_db.clone(),
                None,
                MigrationAuthority::Deny,
                home.clone(),
            )
            .expect("seed current product DB"),
        );
        let conn = Connection::open(&global_db).expect("open seeded DB");
        memcore::db::ensure_reserved_reference_write_guard(&conn).unwrap();
        conn.execute(
            "DELETE FROM hard_state WHERE namespace = ?1 AND key = ?2",
            rusqlite::params!["migrations", "v43_adjudication_provenance"],
        )
        .expect("remove v43 sentinel");
        conn.execute_batch(
            "DROP TABLE adjudication_rowid_minus_one_witness;
             PRAGMA user_version = 42;",
        )
        .expect("downgrade fixture to pending v42");
        let before_denied_open = sqlite_schema(&conn);
        drop(conn);

        let denied = MemoryServer::new_with_migration_authority_and_home(
            global_db.clone(),
            None,
            MigrationAuthority::Deny,
            home.clone(),
        );
        assert!(
            denied.is_err(),
            "ordinary startup must refuse the pending v43 migration"
        );
        let conn = Connection::open(&global_db).expect("inspect denied startup");
        assert_eq!(
            sqlite_schema(&conn),
            before_denied_open,
            "denied startup must not acquire v43 witness table or any other schema"
        );
        let witness_objects: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master
                 WHERE name = 'adjudication_rowid_minus_one_witness'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(witness_objects, 0);
        drop(conn);

        drop(
            MemoryServer::new_with_migration_authority_and_home(
                global_db.clone(),
                None,
                MigrationAuthority::Allow {
                    approved_by: "test:adjudication-provenance-v43".to_string(),
                },
                home.clone(),
            )
            .expect("authorized startup migrates v42 -> v43 cleanly"),
        );
        let conn = Connection::open(&global_db).expect("inspect migrated DB");
        let witness_objects: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master
                 WHERE name = 'adjudication_rowid_minus_one_witness'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(witness_objects, 1);
        assert_eq!(
            conn.query_row("PRAGMA user_version", [], |row| row.get::<_, i64>(0))
                .unwrap(),
            i64::from(memcore::db::migrations::EXPECTED_SCHEMA_VERSION)
        );
        drop(conn);

        drop(
            MemoryServer::new_with_migration_authority_and_home(
                global_db,
                None,
                MigrationAuthority::Deny,
                home,
            )
            .expect("current DB reopens without migration authority"),
        );
    }
}
