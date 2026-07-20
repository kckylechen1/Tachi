use super::*;
use tokio_util::sync::CancellationToken;

pub(super) fn spawn_idle_connection_cleanup(
    server: &MemoryServer,
    shutdown: CancellationToken,
) -> tokio::task::JoinHandle<()> {
    let pool = server.pool.clone();
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_secs(60));
        loop {
            tokio::select! {
                _ = shutdown.cancelled() => break,
                _ = interval.tick() => {
                    for key in pool.remove_idle_connections(Instant::now()) {
                        eprintln!("Idle cleanup: disconnecting '{}'", key);
                    }
                }
            }
        }
    })
}

/// Periodic WAL TRUNCATE checkpoint. SQLite's default PASSIVE auto-checkpoint
/// merges WAL frames into the DB but never shrinks the `-wal` file; a write
/// burst — or long-lived readers blocking truncation — lets it balloon
/// (observed: a 25 MB orphaned WAL on a busy agent DB, causing slow reads and
/// lock contention). Cadence via `TACHI_WAL_CHECKPOINT_SECS` (default 300s,
/// min 30s; set 0 to disable).
///
/// Also runs `PRAGMA optimize` (see `MemoryStore::run_optimize`) on the same
/// tick: both are cheap, best-effort, write-connection maintenance ops that
/// want a "quiet moment" cadence, so they share this timer rather than
/// running two near-identical interval loops. `maintain_named_projects` must
/// follow the daemon's manifest-background scope: a scoped/global-only daemon
/// must not attach DBs it does not own merely to perform maintenance.
pub(super) fn spawn_wal_checkpoint(
    server: &MemoryServer,
    maintain_named_projects: bool,
    shutdown: CancellationToken,
) -> tokio::task::JoinHandle<()> {
    let ckpt_secs = parse_env_u64("TACHI_WAL_CHECKPOINT_SECS").unwrap_or(300);
    if ckpt_secs == 0 {
        return tokio::spawn(async {});
    }
    if !maintain_named_projects {
        eprintln!("[wal] named-project maintenance disabled for scoped daemon");
    }
    let ckpt_secs = ckpt_secs.max(30);
    let ckpt_server = server.clone();
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_secs(ckpt_secs));
        interval.tick().await; // consume the immediate first tick
        loop {
            tokio::select! {
                _ = shutdown.cancelled() => break,
                _ = interval.tick() => {
                    run_wal_checkpoint(&ckpt_server, maintain_named_projects);
                }
            }
        }
    })
}

/// Reap expired `hard_state` rows (e.g. capture-manifest staging TTLs, see
/// `memcore::db::reap_expired_state`) on one store. Best-effort like the
/// checkpoint/optimize calls above it, but unlike them a reap failure must
/// never be swallowed silently: it is always logged (even when it reaps
/// nothing worth reporting, the checkpoint/optimize calls stay silent on
/// success — a reap failure is not allowed the same silence).
fn reap_expired_hard_state(
    ckpt_server: &MemoryServer,
    label: &str,
    now_rfc3339: &str,
    run: impl FnOnce(&MemoryServer, &str) -> Result<usize, String>,
) {
    match run(ckpt_server, now_rfc3339) {
        Ok(removed) => {
            if removed > 0 {
                eprintln!("[state-reap] {label}: reaped {removed} expired hard_state row(s)");
            }
        }
        Err(e) => eprintln!("[state-reap] {label} reap failed: {e}"),
    }
}

fn run_wal_checkpoint(ckpt_server: &MemoryServer, maintain_named_projects: bool) {
    let now_rfc3339 = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true);
    if let Err(e) = ckpt_server
        .with_global_store(|store| store.checkpoint_wal_truncate().map_err(|e| e.to_string()))
    {
        eprintln!("[wal] global checkpoint skipped: {e}");
    }
    if let Err(e) =
        ckpt_server.with_global_store(|store| store.run_optimize().map_err(|e| e.to_string()))
    {
        eprintln!("[optimize] global optimize skipped: {e}");
    }
    reap_expired_hard_state(ckpt_server, "global", &now_rfc3339, |server, now| {
        server.with_global_store(|store| store.reap_expired_state(now).map_err(|e| e.to_string()))
    });
    if ckpt_server.has_project_db() {
        if let Err(e) = ckpt_server
            .with_project_store(|store| store.checkpoint_wal_truncate().map_err(|e| e.to_string()))
        {
            eprintln!("[wal] project checkpoint skipped: {e}");
        }
        if let Err(e) =
            ckpt_server.with_project_store(|store| store.run_optimize().map_err(|e| e.to_string()))
        {
            eprintln!("[optimize] project optimize skipped: {e}");
        }
        reap_expired_hard_state(ckpt_server, "project", &now_rfc3339, |server, now| {
            server.with_project_store(|store| {
                store.reap_expired_state(now).map_err(|e| e.to_string())
            })
        });
    }
    if maintain_named_projects {
        for name in crate::path_utils::list_named_projects() {
            if let Err(e) = ckpt_server.with_named_project_store(&name, |store| {
                store.checkpoint_wal_truncate().map_err(|e| e.to_string())
            }) {
                eprintln!("[wal] named-project '{name}' checkpoint skipped: {e}");
            }
            if let Err(e) = ckpt_server.with_named_project_store(&name, |store| {
                store.run_optimize().map_err(|e| e.to_string())
            }) {
                eprintln!("[optimize] named-project '{name}' optimize skipped: {e}");
            }
            reap_expired_hard_state(
                ckpt_server,
                &format!("named-project '{name}'"),
                &now_rfc3339,
                |server, now| {
                    server.with_named_project_store(&name, |store| {
                        store.reap_expired_state(now).map_err(|e| e.to_string())
                    })
                },
            );
        }
    }
}

pub(super) fn spawn_background_gc(
    server: &MemoryServer,
    gc_enabled: bool,
    gc_initial_delay_secs: u64,
    gc_interval_secs: u64,
    shutdown: CancellationToken,
) -> tokio::task::JoinHandle<()> {
    if !gc_enabled {
        eprintln!("Background GC disabled");
        return tokio::spawn(async {});
    }
    eprintln!(
        "Background GC enabled (initial_delay={}s, interval={}s)",
        gc_initial_delay_secs, gc_interval_secs
    );
    let gc_server = server.clone();
    tokio::spawn(async move {
        tokio::select! {
            _ = shutdown.cancelled() => return,
            _ = tokio::time::sleep(Duration::from_secs(gc_initial_delay_secs)) => {}
        }
        let mut interval = tokio::time::interval(Duration::from_secs(gc_interval_secs));
        loop {
            tokio::select! {
                _ = shutdown.cancelled() => break,
                _ = interval.tick() => {
                    eprintln!("[gc] Running scheduled garbage collection...");
                    match gc_server.with_global_store(|store: &mut MemoryStore| {
                        let mut gc = store
                            .gc_tables(&memcore::GcConfig::default())
                            .map_err(|e| format!("{e}"))?;
                        let kanban_deleted =
                            gc_expired_kanban_cards(store, DEFAULT_KANBAN_GC_MAX_AGE_DAYS)?;
                        let foundry_deleted =
                            memcore::gc_foundry_jobs(store.connection(), 30).unwrap_or(0);
                        if let Some(object) = gc.as_object_mut() {
                            object.insert("kanban_cards_pruned".into(), json!(kanban_deleted));
                            object.insert("foundry_jobs_pruned".into(), json!(foundry_deleted));
                        }
                        let stale_days: u32 = std::env::var("MEMORY_GC_STALE_DAYS")
                            .ok()
                            .and_then(|v| v.parse().ok())
                            .unwrap_or(90);
                        match store.archive_stale_memories(stale_days) {
                            Ok(archived) => {
                                if archived > 0 {
                                    eprintln!("[gc] Archived {} stale memories", archived);
                                }
                                if let Some(object) = gc.as_object_mut() {
                                    object.insert("memories_archived".into(), json!(archived));
                                }
                            }
                            Err(e) => eprintln!("[gc] archive_stale_memories error: {}", e),
                        }
                        Ok(gc)
                    }) {
                        Ok(result) => eprintln!("[gc] Global DB: {}", result),
                        Err(e) => eprintln!("[gc] Global DB error: {}", e),
                    }
                    if gc_server.has_project_db() {
                        match gc_server.with_project_store(|store: &mut MemoryStore| {
                            let mut gc = store
                                .gc_tables(&memcore::GcConfig::default())
                                .map_err(|e| format!("{e}"))?;
                            let kanban_deleted =
                                gc_expired_kanban_cards(store, DEFAULT_KANBAN_GC_MAX_AGE_DAYS)?;
                            let foundry_deleted =
                                memcore::gc_foundry_jobs(store.connection(), 30).unwrap_or(0);
                            if let Some(object) = gc.as_object_mut() {
                                object.insert("kanban_cards_pruned".into(), json!(kanban_deleted));
                                object.insert("foundry_jobs_pruned".into(), json!(foundry_deleted));
                            }
                            let stale_days: u32 = std::env::var("MEMORY_GC_STALE_DAYS")
                                .ok()
                                .and_then(|v| v.parse().ok())
                                .unwrap_or(90);
                            match store.archive_stale_memories(stale_days) {
                                Ok(archived) => {
                                    if archived > 0 {
                                        eprintln!(
                                            "[gc] Archived {} stale memories (project)",
                                            archived
                                        );
                                    }
                                    if let Some(object) = gc.as_object_mut() {
                                        object.insert("memories_archived".into(), json!(archived));
                                    }
                                }
                                Err(e) => eprintln!("[gc] archive_stale_memories error: {}", e),
                            }
                            Ok(gc)
                        }) {
                            Ok(result) => eprintln!("[gc] Project DB: {}", result),
                            Err(e) => eprintln!("[gc] Project DB error: {}", e),
                        }
                    }
                }
            }
        }
    })
}

pub(super) fn run_startup_integrity_checks(
    server: &MemoryServer,
    check_project_db: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    // Integrity check on global DB
    server
        .with_global_store(|store| {
            match store.quick_check() {
                Ok(true) => eprintln!("Global database integrity: OK"),
                Ok(false) => eprintln!("WARNING: Global database may be corrupted!"),
                Err(e) => eprintln!("WARNING: Could not check global database integrity: {e}"),
            }
            Ok(())
        })
        .map_err(|e| format!("startup check: {e}"))?;

    // Integrity check on project DB
    if check_project_db {
        server
            .with_project_store(|store| {
                match store.quick_check() {
                    Ok(true) => eprintln!("Project database integrity: OK"),
                    Ok(false) => eprintln!("WARNING: Project database may be corrupted!"),
                    Err(e) => {
                        eprintln!("WARNING: Could not check project database integrity: {e}")
                    }
                }
                Ok(())
            })
            .map_err(|e| format!("startup check: {e}"))?;
    }
    Ok(())
}

pub(super) fn load_cached_hub_tools(server: &MemoryServer) {
    // Load cached proxy tools from Hub
    {
        let load_proxy_tools = |store: &mut MemoryStore| -> Result<(), String> {
            let mcp_caps = store
                .hub_list(Some("mcp"), true)
                .map_err(|e| format!("hub list: {e}"))?;
            for cap in mcp_caps {
                let def: serde_json::Value = match serde_json::from_str(&cap.definition) {
                    Ok(def) => def,
                    Err(e) => {
                        eprintln!(
                            "[startup] skip MCP '{}' due to invalid definition JSON: {e}",
                            cap.id
                        );
                        continue;
                    }
                };
                if let Some(tools_json) = def.get("discovered_tools") {
                    match serde_json::from_value::<Vec<rmcp::model::Tool>>(tools_json.clone()) {
                        Ok(tools) => {
                            let server_name = cap.id.strip_prefix("mcp:").unwrap_or(&cap.id);
                            let filtered_tools = filter_mcp_tools_by_permissions(&def, tools);
                            lock_or_recover(&server.tool_discovery.proxy_tools, "proxy_tools")
                                .insert(server_name.to_string(), filtered_tools);
                        }
                        Err(e) => {
                            eprintln!(
                                "[startup] skip cached tools for '{}' due to invalid tool payload: {e}",
                                cap.id
                            );
                        }
                    }
                }
            }
            Ok(())
        };
        if let Err(e) = server.with_global_store(load_proxy_tools) {
            eprintln!("[startup] failed loading global MCP proxy cache: {e}");
        }
        if server.has_project_db() {
            if let Err(e) = server.with_project_store(load_proxy_tools) {
                eprintln!("[startup] failed loading project MCP proxy cache: {e}");
            }
        }
    }
    {
        let load_skill_tools = |store: &mut MemoryStore| -> Result<(), String> {
            let skill_caps = store
                .hub_list(Some("skill"), true)
                .map_err(|e| format!("hub list: {e}"))?;
            for cap in skill_caps {
                if should_expose_skill_tool(&cap) {
                    if let Err(e) = server.register_skill_tool(&cap) {
                        eprintln!(
                            "[startup] failed to register skill tool for '{}': {}",
                            cap.id, e
                        );
                    }
                }
            }
            Ok(())
        };
        if let Err(e) = server.with_global_store(load_skill_tools) {
            eprintln!("[startup] failed loading global skill tools: {e}");
        }
        if server.has_project_db() {
            if let Err(e) = server.with_project_store(load_skill_tools) {
                eprintln!("[startup] failed loading project skill tools: {e}");
            }
        }
    }
}

pub(super) fn report_pipeline_and_spawn_daily_distill(
    server: &MemoryServer,
    app_home: &std::path::Path,
    shutdown: CancellationToken,
) -> tokio::task::JoinHandle<()> {
    if server.pipeline_enabled {
        eprintln!("Pipeline workers: ENABLED (external)");
    } else {
        eprintln!("Pipeline workers: DISABLED (set ENABLE_PIPELINE=true to enable)");
    }

    if daily_distill_scheduler_enabled(server) {
        // Phase 1 daily batch distill. Default cadence is 24h; the legacy
        // per-capture `MemoryDistill` enqueue is gone, so this scheduler must
        // remain active even when external pipeline workers are disabled.
        let distill_interval_secs: u64 = std::env::var("DISTILL_INTERVAL_SECS")
            .ok()
            .and_then(|value| value.parse().ok())
            .unwrap_or(86_400);

        let distill_server = server.clone();
        let marker_path = daily_distill_marker_path(app_home);
        tokio::spawn(async move {
            tokio::select! {
                _ = shutdown.cancelled() => return,
                _ = tokio::time::sleep(Duration::from_secs(60)) => {}
            }
            eprintln!(
                "Distill scheduler: ENABLED (daily batch, interval={}s)",
                distill_interval_secs
            );

            let run_once = |server: &crate::MemoryServer, marker: &std::path::Path| {
                let server = server.clone();
                let marker = marker.to_path_buf();
                async move {
                    match crate::foundry_runtime_ops::run_daily_batch_distill(&server).await {
                        Ok(report) => {
                            eprintln!(
                                "[distill] daily batch: dispatched={} distilled={} skipped={} fallback={} errors={}",
                                report.batches_dispatched,
                                report.groups_distilled,
                                report.groups_skipped,
                                report.fallback_used,
                                report.errors.len()
                            );
                            if let Some(parent) = marker.parent() {
                                if let Err(err) = tokio::fs::create_dir_all(parent).await {
                                    eprintln!(
                                        "[distill] failed to create marker directory {}: {err}",
                                        parent.display()
                                    );
                                    return;
                                }
                            }
                            // Marker carries the batch quality summary (not just a
                            // timestamp) so `tachi_status` can surface distill
                            // degradation and so hard errors reach the health score.
                            // read_distill_marker stays backward-compatible with the
                            // legacy bare-timestamp format.
                            let marker_body = serde_json::json!({
                                "ts": chrono::Utc::now().to_rfc3339(),
                                "groups_distilled": report.groups_distilled,
                                "groups_skipped": report.groups_skipped,
                                "fallback_used": report.fallback_used,
                                "errors": report.errors.len(),
                            })
                            .to_string();
                            if let Err(err) = tokio::fs::write(&marker, marker_body).await {
                                eprintln!(
                                    "[distill] failed to write marker {}: {err}",
                                    marker.display()
                                );
                            }
                        }
                        Err(err) => {
                            eprintln!("[distill] daily batch error: {err}");
                            // Write a failure marker so `tachi_status` can surface
                            // the *reason* the marker is stale instead of a bare
                            // "stale" that repeats day after day with no owner.
                            // Same JSON shape as the success marker plus `error`;
                            // read_distill_marker is backward-compatible.
                            if let Some(parent) = marker.parent() {
                                if let Err(mk_err) = tokio::fs::create_dir_all(parent).await {
                                    eprintln!(
                                        "[distill] failed to create marker directory {}: {mk_err}",
                                        parent.display()
                                    );
                                    return;
                                }
                            }
                            let marker_body = serde_json::json!({
                                "ts": chrono::Utc::now().to_rfc3339(),
                                "error": err.to_string(),
                                "groups_distilled": 0,
                                "groups_skipped": 0,
                                "fallback_used": 0,
                                "errors": 0,
                            })
                            .to_string();
                            if let Err(write_err) = tokio::fs::write(&marker, marker_body).await {
                                eprintln!(
                                    "[distill] failed to write failure marker {}: {write_err}",
                                    marker.display()
                                );
                            }
                        }
                    }
                }
            };

            // Catch-up: if the marker is missing or older than the cadence,
            // run immediately after the 60s warmup.
            let should_run_now = match tokio::fs::metadata(&marker_path)
                .await
                .and_then(|m| m.modified())
            {
                Ok(modified) => modified
                    .elapsed()
                    .map(|d| d.as_secs() >= distill_interval_secs)
                    .unwrap_or(true),
                Err(_) => true,
            };
            if should_run_now {
                run_once(&distill_server, &marker_path).await;
            }

            let mut interval = tokio::time::interval(Duration::from_secs(distill_interval_secs));
            // First tick fires immediately; consume it so we wait a full cadence.
            interval.tick().await;
            loop {
                tokio::select! {
                    _ = shutdown.cancelled() => break,
                    _ = interval.tick() => {
                        run_once(&distill_server, &marker_path).await;
                    }
                }
            }
        })
    } else {
        eprintln!("Distill scheduler: DISABLED (no project DB available)");
        tokio::spawn(async {})
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::EnvRestore;

    fn make_server() -> (tempfile::TempDir, MemoryServer) {
        let tmp = tempfile::tempdir().expect("tempdir");
        let global_db = tmp.path().join("global.db");
        let server = MemoryServer::new(global_db, None).expect("server");
        (tmp, server)
    }

    #[tokio::test]
    async fn idle_cleanup_exits_on_shutdown_cancel() {
        let (_tmp, server) = make_server();
        let shutdown = CancellationToken::new();
        let handle = spawn_idle_connection_cleanup(&server, shutdown.clone());
        shutdown.cancel();
        tokio::time::timeout(Duration::from_secs(2), handle)
            .await
            .expect("idle cleanup task did not exit within 2s after shutdown")
            .expect("task panicked");
    }

    #[tokio::test]
    async fn wal_checkpoint_exits_on_shutdown_cancel() {
        let (_tmp, server) = make_server();
        let shutdown = CancellationToken::new();
        let handle = spawn_wal_checkpoint(&server, true, shutdown.clone());
        shutdown.cancel();
        tokio::time::timeout(Duration::from_secs(2), handle)
            .await
            .expect("wal checkpoint task did not exit within 2s after shutdown")
            .expect("task panicked");
    }

    #[test]
    fn wal_checkpoint_scoped_to_active_dbs_does_not_attach_named_projects() {
        let _guard = crate::utils::global_test_lock()
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let tmp = tempfile::tempdir().expect("tempdir");
        let tachi_home = tmp.path().join("home");
        let _tachi_home = EnvRestore::set_path("TACHI_HOME", &tachi_home);

        let foreign_db = tachi_home
            .join("projects")
            .join("foreign")
            .join(memcore::MEMORY_DB_FILENAME);
        drop(MemoryServer::new(foreign_db.clone(), None).expect("foreign named-project DB"));

        let server = MemoryServer::new(tachi_home.join("global.db"), None).expect("server");
        run_wal_checkpoint(&server, false);

        assert!(
            server
                .db
                .attached_project_dbs
                .read()
                .unwrap_or_else(|e| e.into_inner())
                .is_empty(),
            "active-DB-only maintenance must not open or cache named-project DBs"
        );

        run_wal_checkpoint(&server, true);
        let attached = server
            .db
            .attached_project_dbs
            .read()
            .unwrap_or_else(|e| e.into_inner());
        assert_eq!(attached.len(), 1);
        assert!(
            attached
                .contains_key(&std::fs::canonicalize(foreign_db).expect("canonical foreign DB")),
            "manifest-wide maintenance must keep maintaining named-project DBs"
        );
    }

    #[test]
    fn wal_checkpoint_reaps_expired_hard_state_on_global_and_project_scope() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let global_db = tmp.path().join("global.db");
        let project_db = tmp.path().join("project.db");
        let server =
            MemoryServer::new(global_db, Some(project_db)).expect("server with project db");

        server
            .with_global_store(|store| {
                store
                    .set_state(
                        "capture_manifest",
                        "expired-global",
                        r#"{"expires_at":"2000-01-01T00:00:00Z"}"#,
                    )
                    .map_err(|e| e.to_string())
            })
            .expect("seed expired global hard_state row");
        server
            .with_global_store(|store| {
                store
                    .set_state(
                        "capture_manifest",
                        "not-yet-expired-global",
                        r#"{"expires_at":"2999-01-01T00:00:00Z"}"#,
                    )
                    .map_err(|e| e.to_string())
            })
            .expect("seed not-yet-expired global hard_state row");
        server
            .with_project_store(|store| {
                store
                    .set_state(
                        "capture_manifest",
                        "expired-project",
                        r#"{"expires_at":"2000-01-01T00:00:00Z"}"#,
                    )
                    .map_err(|e| e.to_string())
            })
            .expect("seed expired project hard_state row");

        run_wal_checkpoint(&server, false);

        let global_expired = server
            .with_global_store(|store| {
                store
                    .get_state_kv("capture_manifest", "expired-global")
                    .map_err(|e| e.to_string())
            })
            .expect("read global expired state");
        assert!(
            global_expired.is_none(),
            "expired global hard_state row must be reaped by wal-checkpoint maintenance"
        );

        let global_future = server
            .with_global_store(|store| {
                store
                    .get_state_kv("capture_manifest", "not-yet-expired-global")
                    .map_err(|e| e.to_string())
            })
            .expect("read global not-yet-expired state");
        assert!(
            global_future.is_some(),
            "not-yet-expired global hard_state row must survive"
        );

        let project_expired = server
            .with_project_store(|store| {
                store
                    .get_state_kv("capture_manifest", "expired-project")
                    .map_err(|e| e.to_string())
            })
            .expect("read project expired state");
        assert!(
            project_expired.is_none(),
            "expired project hard_state row must be reaped by wal-checkpoint maintenance \
             (own-project scope, no maintain_named_projects gate)"
        );
    }

    #[test]
    fn wal_checkpoint_reaps_named_project_hard_state_only_when_maintenance_enabled() {
        let _guard = crate::utils::global_test_lock()
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let tmp = tempfile::tempdir().expect("tempdir");
        let tachi_home = tmp.path().join("home");
        let _tachi_home = EnvRestore::set_path("TACHI_HOME", &tachi_home);

        let foreign_db = tachi_home
            .join("projects")
            .join("foreign")
            .join(memcore::MEMORY_DB_FILENAME);
        {
            let foreign_server =
                MemoryServer::new(foreign_db.clone(), None).expect("foreign named-project DB");
            foreign_server
                .with_global_store(|store| {
                    store
                        .set_state(
                            "capture_manifest",
                            "expired-named",
                            r#"{"expires_at":"2000-01-01T00:00:00Z"}"#,
                        )
                        .map_err(|e| e.to_string())
                })
                .expect("seed expired named-project hard_state row");
        }

        let server = MemoryServer::new(tachi_home.join("global.db"), None).expect("server");

        run_wal_checkpoint(&server, false);
        let row_after_disabled = server
            .with_named_project_store("foreign", |store| {
                store
                    .get_state_kv("capture_manifest", "expired-named")
                    .map_err(|e| e.to_string())
            })
            .expect("read named-project state after disabled maintenance");
        assert!(
            row_after_disabled.is_some(),
            "named-project hard_state reap must not run when maintain_named_projects=false, \
             mirroring the checkpoint/optimize scope gate"
        );

        run_wal_checkpoint(&server, true);
        let row_after_enabled = server
            .with_named_project_store("foreign", |store| {
                store
                    .get_state_kv("capture_manifest", "expired-named")
                    .map_err(|e| e.to_string())
            })
            .expect("read named-project state after enabled maintenance");
        assert!(
            row_after_enabled.is_none(),
            "named-project hard_state reap must run when maintain_named_projects=true"
        );
    }

    #[tokio::test]
    async fn background_gc_exits_on_shutdown_cancel_during_initial_delay() {
        let (_tmp, server) = make_server();
        let shutdown = CancellationToken::new();
        let handle = spawn_background_gc(&server, true, 3600, 3600, shutdown.clone());
        shutdown.cancel();
        tokio::time::timeout(Duration::from_secs(2), handle)
            .await
            .expect("background gc task did not exit within 2s after shutdown")
            .expect("task panicked");
    }

    #[tokio::test]
    async fn background_gc_returns_noop_handle_when_disabled() {
        let (_tmp, server) = make_server();
        let shutdown = CancellationToken::new();
        let handle = spawn_background_gc(&server, false, 0, 1, shutdown);
        tokio::time::timeout(Duration::from_secs(2), handle)
            .await
            .expect("disabled gc handle should complete immediately")
            .expect("task panicked");
    }
}
