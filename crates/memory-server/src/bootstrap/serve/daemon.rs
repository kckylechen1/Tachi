use super::*;
use std::sync::Arc;

pub(super) async fn serve_http_daemon(
    server: MemoryServer,
    app_home: PathBuf,
    global_db_path: PathBuf,
    project_db_path: Option<PathBuf>,
    port: u16,
) -> Result<(), Box<dyn std::error::Error>> {
    // Mark this process so MCP write handlers execute locally instead of
    // re-forwarding to ourselves over HTTP.
    std::env::set_var("TACHI_DAEMON", "1");

    // HTTP daemon mode.
    // In daemon mode the cwd-derived repo-local project DB is RETAINED
    // (see the `cli.daemon && project_db_path.is_some()` block above): the
    // multi-DB FoundryScheduler gives every manifest DB equal coverage, so
    // we keep the auto-detected project and only emit a warning that the
    // daemon's bound project follows the launch cwd. Users can still pin a
    // single project explicitly via --project-db.

    // PR-4 singleton enforcement: acquire a daemon lock scoped to the
    // global DB before binding HTTP. Embedded runtimes can share the same
    // app_home without blocking each other when their global DBs differ.
    let lock_path = crate::daemon_lock::scoped_daemon_lock_path(&app_home, &global_db_path);
    let _daemon_lock = match crate::daemon_lock::DaemonLock::acquire(&lock_path) {
        Ok(g) => {
            eprintln!(
                "[daemon] acquired singleton lock at {} (pid {})",
                lock_path.display(),
                std::process::id()
            );
            g
        }
        Err(e) => {
            eprintln!(
                "[daemon] refusing to start: another tachi daemon already holds {} ({e})",
                lock_path.display()
            );
            return Err(format!("tachi daemon singleton lock unavailable: {e}").into());
        }
    };

    // PR-4 multi-DB scheduler: periodically scan ~/.tachi/manifest.json
    // and run a per-DB safety-net poll against foundry_jobs. Re-injects
    // pending jobs into the existing foundry_tx mpsc channel for
    // routable scopes (own global/project + named projects), counts
    // orphans for unroutable manifest entries (dark DBs).
    let manifest_path = app_home.join("manifest.json");
    let manifest_background =
        daemon_uses_manifest_background(&app_home, &global_db_path, project_db_path.as_deref());
    let scheduler = if manifest_background {
        crate::foundry_scheduler::FoundryScheduler::start(
            manifest_path.clone(),
            server.foundry_tx_clone(),
            server.global_db_path_buf(),
            server.project_db_path_buf(),
        )
    } else {
        crate::foundry_scheduler::FoundryScheduler::start_own_dbs(
            manifest_path.clone(),
            server.foundry_tx_clone(),
            server.global_db_path_buf(),
            server.project_db_path_buf(),
        )
    };
    if manifest_background {
        eprintln!(
            "[daemon] foundry scheduler started (manifest={})",
            manifest_path.display()
        );
    } else {
        eprintln!("[daemon] foundry scheduler scoped to daemon DBs only");
    }
    // Hold scheduler for the whole daemon lifetime; Drop cancels
    // the manifest watcher + per-DB workers.
    let _scheduler = scheduler;

    let _vector_sweep = if manifest_background {
        let scheduler = crate::vector_sweep::VectorSweepScheduler::start(
            manifest_path.clone(),
            global_db_path.clone(),
            project_db_path.clone(),
            server.llm.clone(),
        );
        eprintln!(
            "[daemon] vector sweep scheduled (manifest={})",
            manifest_path.display()
        );
        scheduler
    } else {
        let scheduler = crate::vector_sweep::VectorSweepScheduler::start_own_dbs(
            manifest_path.clone(),
            global_db_path.clone(),
            project_db_path.clone(),
            server.llm.clone(),
        );
        eprintln!("[daemon] vector sweep scoped to daemon DBs only");
        scheduler
    };

    let _continuity_projection = if manifest_background {
        let scheduler = crate::continuity_projector::ContinuityProjectionScheduler::start(
            manifest_path.clone(),
            server.clone(),
            global_db_path.clone(),
            project_db_path.clone(),
        );
        eprintln!(
            "[daemon] continuity projection scheduled (manifest={})",
            manifest_path.display()
        );
        scheduler
    } else {
        let scheduler = crate::continuity_projector::ContinuityProjectionScheduler::start_own_dbs(
            manifest_path.clone(),
            server.clone(),
            global_db_path.clone(),
            project_db_path.clone(),
        );
        eprintln!("[daemon] continuity projection scoped to daemon DBs only");
        scheduler
    };

    if manifest_background {
        let daily_server = server.clone();
        tokio::spawn(async move {
            loop {
                let next_run = crate::daily_pipeline::next_daily_run_time();
                tokio::time::sleep_until(next_run).await;
                match crate::daily_pipeline::run_daily_pipeline(&daily_server).await {
                    Ok(report) => {
                        eprintln!("[daily-pipeline] completed: {}", report.summary())
                    }
                    Err(e) => eprintln!("[daily-pipeline] failed: {e}"),
                }
            }
        });
        eprintln!("[daemon] daily pipeline scheduled for 04:00 Asia/Shanghai");

        let rem_server = server.clone();
        tokio::spawn(async move {
            loop {
                let next_run = crate::daily_pipeline::next_weekly_rem_run_time();
                tokio::time::sleep_until(next_run).await;
                match crate::foundry_runtime_ops::wiki_evolver::run_weekly_wiki_evolution(
                    &rem_server,
                )
                .await
                {
                    Ok(report) => eprintln!(
                        "[rem-wiki-evolver] completed: clusters={} drafts={} skipped={} errors={}",
                        report.clusters_found, report.drafts_written, report.skipped, report.errors
                    ),
                    Err(e) => eprintln!("[rem-wiki-evolver] failed: {e}"),
                }
            }
        });
        eprintln!("[daemon] REM wiki evolver scheduled for Sunday 05:00 Asia/Shanghai");
    } else {
        eprintln!("[daemon] daily pipeline and REM wiki evolver disabled for scoped daemon");
    }

    use rmcp::transport::streamable_http_server::{
        session::local::LocalSessionManager, StreamableHttpServerConfig, StreamableHttpService,
    };
    use tokio_util::sync::CancellationToken;

    let ct = CancellationToken::new();
    let ct_shutdown = ct.clone();
    let requested_bind_addr = format!("127.0.0.1:{}", port);
    let listener = tokio::net::TcpListener::bind(&requested_bind_addr).await?;
    let local_addr = listener.local_addr()?;
    let bind_addr = local_addr.to_string();
    let port = local_addr.port();

    // Idle reaper: a detached daemon has no parent whose death would signal
    // it to stop, so without this it lingers forever — one per global DB,
    // accumulating across every host restart. After
    // TACHI_DAEMON_IDLE_TIMEOUT_SECS with no MCP tool call (default 1800s;
    // 0 disables) it cancels its own serve token; the next stdio invocation
    // auto-respawns one on demand.
    if let Some(idle_timeout) = daemon_idle_timeout() {
        let clock = server.activity_clock();
        let ct_idle = ct.clone();
        let tick = std::time::Duration::from_secs(idle_timeout.as_secs().clamp(5, 60));
        tokio::spawn(async move {
            loop {
                tokio::time::sleep(tick).await;
                let last = clock.load(std::sync::atomic::Ordering::Relaxed);
                let idle_ms = chrono::Utc::now().timestamp_millis() - last;
                if idle_ms >= idle_timeout.as_millis() as i64 {
                    eprintln!(
                        "[idle-reaper] daemon idle {}s (limit {}s); shutting down — will auto-respawn on demand",
                        idle_ms / 1000,
                        idle_timeout.as_secs()
                    );
                    ct_idle.cancel();
                    break;
                }
            }
        });
    }

    let health_server = server.clone();

    let mut http_config = StreamableHttpServerConfig::default();
    http_config.stateful_mode = true;
    http_config.cancellation_token = ct.child_token();

    let service = StreamableHttpService::new(
        move || Ok(server.clone()),
        Arc::new(LocalSessionManager::default()),
        http_config,
    );

    let router = axum::Router::new()
        .route(
            "/health",
            axum::routing::get(move || {
                let health_server = health_server.clone();
                async move {
                    let db_ok = health_server
                        .with_global_store_read(|store| {
                            store.stats(false).map(|_| true).map_err(|e| e.to_string())
                        })
                        .is_ok();
                    let vec_available = health_server.global_vec_available();
                    let status = if db_ok { "ok" } else { "degraded" };
                    let code = if db_ok {
                        axum::http::StatusCode::OK
                    } else {
                        axum::http::StatusCode::SERVICE_UNAVAILABLE
                    };
                    (
                        code,
                        axum::Json(serde_json::json!({
                            "status": status,
                            "version": crate::build_info::PKG_VERSION,
                            "git_sha": crate::build_info::GIT_SHA,
                            "build_time": crate::build_info::BUILD_TIME,
                            "transport": "http",
                            "mcp": "streamable-http",
                            "db_ready": db_ok,
                            "vec_available": vec_available,
                        })),
                    )
                }
            }),
        )
        .nest_service("/mcp", service);
    eprintln!("Tachi daemon listening on http://{bind_addr}");

    // Write daemon discovery file so CLI invocations can forward writes
    // to the running daemon instead of contending for the DB write lock.
    let pid_path = crate::daemon_lock::scoped_daemon_pid_path(&app_home, &global_db_path);
    let pid_payload = serde_json::json!({
        "pid": std::process::id(),
        "port": port,
        "url": format!("http://{bind_addr}/mcp"),
        "global_db": global_db_path.display().to_string(),
        "project_db": project_db_path
            .as_ref()
            .map(|p| p.display().to_string()),
        "started_at": Utc::now().to_rfc3339(),
        "version": crate::build_info::PKG_VERSION,
        "git_sha": crate::build_info::GIT_SHA,
    });
    if let Some(parent) = pid_path.parent() {
        let _ = tokio::fs::create_dir_all(parent).await;
    }
    let pid_body = serde_json::to_string_pretty(&pid_payload).unwrap_or_default();
    match tokio::task::spawn_blocking({
        let pid_path = pid_path.clone();
        move || crate::utils::write_owner_only_file(&pid_path, pid_body.as_bytes())
    })
    .await
    {
        Ok(Ok(())) => {}
        Ok(Err(e)) => {
            eprintln!(
                "warning: failed to write daemon discovery file {}: {e}",
                pid_path.display()
            );
        }
        Err(e) => {
            eprintln!(
                "warning: failed to write daemon discovery file {}: {e}",
                pid_path.display()
            );
        }
    }
    let pid_path_cleanup = pid_path.clone();

    tokio::select! {
        result = axum::serve(listener, router)
            .with_graceful_shutdown(async move { ct_shutdown.cancelled_owned().await }) => {
            if let Err(e) = result {
                eprintln!("HTTP server error: {e}");
            }
        }
        _ = tokio::signal::ctrl_c() => {
            eprintln!("Received SIGINT, shutting down gracefully...");
            ct.cancel();
        }
        _ = sigterm() => {
            eprintln!("Received SIGTERM, shutting down gracefully...");
            ct.cancel();
        }
    }

    // Best-effort cleanup of daemon discovery file
    let _ = tokio::fs::remove_file(&pid_path_cleanup).await;
    Ok(())
}

fn daemon_uses_manifest_background(
    app_home: &Path,
    global_db_path: &Path,
    project_db_path: Option<&Path>,
) -> bool {
    project_db_path.is_some()
        && paths_match(global_db_path, &app_home.join("global").join("memory.db"))
}

fn paths_match(left: &Path, right: &Path) -> bool {
    if left == right {
        return true;
    }
    std::fs::canonicalize(left)
        .ok()
        .zip(std::fs::canonicalize(right).ok())
        .map(|(left, right)| left == right)
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn manifest_background_only_runs_for_default_global_db() {
        let tmp = tempfile::tempdir().expect("tmp");
        let app_home = tmp.path().join(".tachi");
        let default_global = app_home.join("global").join("memory.db");
        let agent_global = app_home.join("agents").join("main").join("memory.db");
        std::fs::create_dir_all(default_global.parent().unwrap()).expect("default parent");
        std::fs::create_dir_all(agent_global.parent().unwrap()).expect("agent parent");
        std::fs::write(&default_global, b"").expect("default db");
        std::fs::write(&agent_global, b"").expect("agent db");

        let project_db = app_home.join("projects").join("repo").join("memory.db");

        assert!(daemon_uses_manifest_background(
            &app_home,
            &default_global,
            Some(&project_db)
        ));
        assert!(!daemon_uses_manifest_background(
            &app_home,
            &default_global,
            None
        ));
        assert!(!daemon_uses_manifest_background(
            &app_home,
            &agent_global,
            Some(&project_db)
        ));
    }
}
