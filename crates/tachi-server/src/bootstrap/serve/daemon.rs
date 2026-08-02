use super::*;
use std::sync::Arc;

use super::malformed_json_middleware::normalize_malformed_mcp_json_response;

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
                        "[rem-wiki-evolver] {}: clusters={} drafts={} skipped={} errors={} recovered_completed={} recovered_aborted={} foreign_pending_skipped={}",
                        if report.errors == 0 { "completed" } else { "completed_with_errors" },
                        report.clusters_found,
                        report.drafts_written,
                        report.skipped,
                        report.errors,
                        report.recovered_completed,
                        report.recovered_aborted,
                        report.foreign_pending_skipped,
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

    // Discovery pid path + its RAII cleanup guard, computed/armed BEFORE the
    // bind attempt (#936 review follow-up). Bind failure below is a NORMAL
    // function return (`?`-shaped / explicit `return Err`) — destructors run,
    // unlike `std::process::exit` — so `DiscoveryPidGuard::drop` fires and
    // removes any STALE discovery pid file a prior run left behind. Before
    // this fix the pid path was computed only AFTER a successful bind, so a
    // bind failure (port conflict, etc.) returned before any cleanup site
    // existed: under `KeepAlive` the daemon retries every ~10s while a stale
    // pid from the last successful run keeps pointing CLI/MCP clients at a
    // dead process.
    let pid_path = crate::daemon_lock::scoped_daemon_pid_path(&app_home, &global_db_path);
    let mut pid_guard = DiscoveryPidGuard::new(pid_path.clone());

    let requested_bind_addr = format!("127.0.0.1:{}", port);
    let listener = match tokio::net::TcpListener::bind(&requested_bind_addr).await {
        Ok(l) => l,
        Err(e) => {
            // Tagged `bind_failed`, distinct from the post-bind `[fatal]` serve
            // -death exits below, so a launchd retry loop is diagnosable from
            // the log alone: repeated `bind_failed` means the port never opens
            // (an external conflict), whereas a bare `[fatal]` after "Tachi
            // daemon listening on" means the surface bound fine and then died.
            eprintln!("[fatal] [bind_failed] daemon failed to bind {requested_bind_addr}: {e}");
            return Err(e.into());
        }
    };
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

    // `pid_path` / `pid_guard` were computed above, before the bind attempt,
    // so the guard covers a bind failure too. The discovery file itself is
    // written further below, after the router binds; the watchdog's
    // `exit(2)` path also removes it explicitly (see below), matching the
    // fail-loud serve path.

    // Liveness watchdog (#936): launchd checks the *process*, not the
    // *service*. This self-probe hits `/health/live` through the real TCP
    // listener (never the handler fn directly) every interval; after N
    // consecutive TRANSPORT failures — no HTTP response at all: connection
    // refused, timeout, or a transport error — it presumes the HTTP/MCP surface
    // wedged (§ the incident's "background-alive, HTTP-dead" class) and
    // `exit(2)`. An ANSWERED probe (any status) proves the surface is alive and
    // resets the counter, so a degraded-but-serving daemon is never killed; the
    // probe targets `/health/live` (which touches no store) precisely so a slow
    // or locked DB read cannot masquerade as a dead surface.
    //
    // Recovery premise: exiting only helps if the launchd job restarts the
    // process. Both supervisor surfaces are now configured for that —
    // `scripts/install.sh`'s LaunchAgent plist sets `<key>KeepAlive</key>` and
    // the Homebrew formula's `service do` block sets `keep_alive true`
    // (launchd's default ~10s ThrottleInterval), so any exit respawns a fresh
    // serving daemon. Pre-existing installs predating this must reload the job
    // (`brew services restart tachi` / re-run the installer) to pick it up.
    //
    // The watchdog shares `ct` so a normal shutdown stops it cleanly without
    // false-firing, and it only counts failures after a grace window or the
    // first success.
    if let Some((watchdog_interval, watchdog_fails)) = daemon_watchdog_config() {
        let ct_watchdog = ct.clone();
        let health_url = format!("http://{bind_addr}/health/live");
        let grace = daemon_watchdog_grace();
        let pid_path_watchdog = pid_path.clone();
        tokio::spawn(async move {
            // reqwest is built with `rustls-no-provider`; install the crypto
            // provider before building any client or `.build()` panics (even
            // for loopback http, the builder resolves the provider eagerly).
            crate::ensure_tls_provider();
            let client = match reqwest::Client::builder()
                .timeout(std::time::Duration::from_secs(5))
                .build()
            {
                Ok(c) => c,
                Err(e) => {
                    eprintln!(
                        "[watchdog] failed to build probe client: {e}; liveness watchdog disabled"
                    );
                    return;
                }
            };
            eprintln!(
                "[watchdog] liveness watchdog armed: probing {health_url} every {}s, exit after {watchdog_fails} consecutive failures (grace {}s)",
                watchdog_interval.as_secs(),
                grace.as_secs()
            );
            let mut counter = WatchdogCounter::new(watchdog_fails);
            let started = std::time::Instant::now();
            loop {
                tokio::select! {
                    _ = ct_watchdog.cancelled() => break,
                    _ = tokio::time::sleep(watchdog_interval) => {}
                }
                let probe = match client.get(&health_url).send().await {
                    Ok(resp) if resp.status().is_success() => Probe::Healthy,
                    Ok(resp) => {
                        // Answered, but non-2xx (e.g. a 503 while the DB is
                        // degraded). The surface is ALIVE — it accepted the TCP
                        // connection and produced an HTTP response — so this does
                        // NOT count toward the liveness exit. Killing an
                        // alive-but-degraded daemon only makes recovery harder.
                        eprintln!(
                            "[watchdog] /health/live answered {} — surface alive but degraded; not counting toward liveness exit",
                            resp.status()
                        );
                        Probe::Degraded
                    }
                    Err(e) => {
                        // No HTTP response at all: connection refused, timeout, or
                        // a transport error — the surface is presumed dead.
                        eprintln!(
                            "[watchdog] /health/live probe transport failure (no response): {e}"
                        );
                        Probe::Unreachable
                    }
                };
                let grace_elapsed = started.elapsed() >= grace;
                match counter.record(probe, grace_elapsed) {
                    WatchdogVerdict::Continue => {}
                    WatchdogVerdict::Exit(code) => {
                        // Re-check cancellation: a shutdown racing an in-flight
                        // failing probe must not be mistaken for a wedge.
                        if ct_watchdog.is_cancelled() {
                            break;
                        }
                        eprintln!(
                            "[fatal] [watchdog] /health/live unreachable (no HTTP response) {watchdog_fails} consecutive time(s); HTTP surface presumed dead — exiting {code} so launchd (KeepAlive) respawns a serving daemon"
                        );
                        // Drop the discovery pid file before exiting, same as the
                        // fail-loud serve path, so a respawning daemon or CLI
                        // client never forwards a write to this dead pid.
                        let _ = tokio::fs::remove_file(&pid_path_watchdog).await;
                        std::process::exit(code);
                    }
                }
            }
        });
    }

    let health_server = server.clone();

    let mut http_config = StreamableHttpServerConfig::default();
    http_config.stateful_mode = true;
    http_config.cancellation_token = ct.child_token();

    let service = StreamableHttpService::new(
        move || Ok(server.clone_for_mcp_session()),
        Arc::new(LocalSessionManager::default()),
        http_config,
    );

    let router = axum::Router::new()
        .route(
            "/health/live",
            axum::routing::get(|| async {
                // Liveness ONLY: proves the HTTP surface can accept a connection
                // and route a response. Deliberately touches no store, so a
                // degraded or lock-contended DB read can never turn into a false
                // "surface dead" signal for the liveness watchdog (#936). The
                // DB-touching readiness check is the separate `/health` route.
                (
                    axum::http::StatusCode::OK,
                    axum::Json(serde_json::json!({ "status": "live" })),
                )
            }),
        )
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
                        axum::Json(daemon_health_payload(db_ok, vec_available, status)),
                    )
                }
            }),
        )
        .nest_service("/mcp", service)
        .layer(axum::middleware::from_fn(
            normalize_malformed_mcp_json_response,
        ));
    eprintln!("Tachi daemon listening on http://{bind_addr}");

    // Write daemon discovery file so CLI invocations can forward writes
    // to the running daemon instead of contending for the DB write lock.
    // (`pid_path` was computed earlier so the watchdog can clean it up too.)
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
            // FAIL-LOUD serve contract (#936): the HTTP serve future must only
            // resolve because we asked it to (SIGINT/SIGTERM/idle-reaper cancels
            // `ct`, which `ct_shutdown` clones). Any *self-initiated* completion
            // — accept loop ending on its own (`Ok`) or an `axum::serve` error
            // (`Err`) — means the MCP/HTTP surface is dead while the process is
            // otherwise alive. Before this fix both were swallowed and the fn
            // returned `Ok(())` unconditionally, so the process kept running
            // deaf and launchd saw `state=running`. Now we exit non-zero so
            // `KeepAlive` respawns a daemon that can actually serve.
            match serve_exit_decision(&result, ct.is_cancelled()) {
                ExitDecision::Clean => {
                    eprintln!("[daemon] HTTP serve future completed after a shutdown signal");
                }
                ExitDecision::Fatal { code, reason } => {
                    eprintln!("[fatal] {reason}; exiting {code} so launchd (KeepAlive) respawns a serving daemon");
                    let _ = tokio::fs::remove_file(&pid_path_cleanup).await;
                    std::process::exit(code);
                }
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
    // This call site already did its own (async, logged-on-failure) removal
    // above, so disarm the guard rather than let its synchronous Drop attempt
    // a redundant no-op removal.
    pid_guard.disarm();
    Ok(())
}

pub(super) fn daemon_uses_manifest_background(
    app_home: &Path,
    global_db_path: &Path,
    project_db_path: Option<&Path>,
) -> bool {
    project_db_path.is_some()
        && paths_match(
            global_db_path,
            &app_home.join("global").join(memcore::MEMORY_DB_FILENAME),
        )
}

/// Shared `/health` JSON for the streamable-HTTP daemon (#732).
///
/// Auth posture v1 is **loopback trust**: the listener is bound to 127.0.0.1
/// only; no bearer token is required on single-user workstations. Multi-user
/// ACL for header claims is #495. Reconnect fields document HTTP client
/// behavior after daemon restart (stdio adapters are unaffected).
pub(crate) fn daemon_health_payload(
    db_ok: bool,
    vec_available: bool,
    status: &str,
) -> serde_json::Value {
    serde_json::json!({
        "status": status,
        "version": crate::build_info::PKG_VERSION,
        "git_sha": crate::build_info::GIT_SHA,
        "build_time": crate::build_info::BUILD_TIME,
        "transport": "http",
        "mcp": "streamable-http",
        "bind": "127.0.0.1",
        "auth_posture": "loopback-trust-v1",
        "db_ready": db_ok,
        "vec_available": vec_available,
        "reconnect": {
            "on_disconnect": "re-initialize MCP session (new mcp-session-id)",
            "client_hint": "expect brief JSON-RPC -32000 / connection errors until /health returns ok; then POST initialize again",
            "stdio_unaffected": true,
            "docs": "docs/engineering/architecture/http-direct-connect.md"
        },
    })
}

/// What to do once the HTTP serve future has resolved (#936). Factored out of
/// the `tokio::select!` arm so the fail-loud decision is a pure function that
/// tests can assert on — `std::process::exit` lives at the single call site.
#[derive(Debug, PartialEq, Eq)]
pub(super) enum ExitDecision {
    /// The serve future resolved because we asked it to (`ct` was cancelled by
    /// a signal/idle-reaper). Fall through to normal graceful teardown.
    Clean,
    /// The serve future resolved on its own — a dead surface. Log and exit
    /// non-zero so launchd respawns.
    Fatal { code: i32, reason: String },
}

/// Fail-loud serve contract. `result` is the output of `axum::serve(..).await`;
/// `ct_cancelled` is whether the daemon's shutdown token was cancelled (i.e. we
/// asked the surface to stop). An `Ok` with no shutdown signal means the accept
/// loop ended unexpectedly; an `Err` is always a fatal serve error.
pub(super) fn serve_exit_decision(
    result: &std::io::Result<()>,
    ct_cancelled: bool,
) -> ExitDecision {
    match result {
        Ok(()) if ct_cancelled => ExitDecision::Clean,
        Ok(()) => ExitDecision::Fatal {
            code: 1,
            reason: "HTTP serve future returned Ok without a shutdown signal (accept loop ended unexpectedly — MCP/HTTP surface is dead)".to_string(),
        },
        Err(e) => ExitDecision::Fatal {
            code: 1,
            reason: format!("HTTP serve future returned error: {e}"),
        },
    }
}

/// Outcome of a single watchdog self-probe (#936).
///
/// The distinction that matters for liveness: an *answered* probe (`Healthy` or
/// `Degraded`) proves the HTTP surface accepted a connection and produced a
/// response — the daemon is alive — so it resets the counter. Only a *transport*
/// failure (`Unreachable`: no response at all) is evidence the surface wedged.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Probe {
    /// The probe answered with a 2xx status through the real socket.
    Healthy,
    /// The probe answered with a non-2xx status (e.g. 503 while the DB is
    /// degraded). The surface is STILL ALIVE — it accepted the connection and
    /// produced an HTTP response — so this resets the watchdog, exactly like a
    /// 2xx. A degraded-but-serving daemon must never be killed by the watchdog.
    Degraded,
    /// No HTTP response at all: connection refused, timeout, or transport error.
    /// The surface is presumed dead → counts toward the kill threshold.
    Unreachable,
}

/// What the watchdog loop should do after recording a probe (#936).
#[derive(Debug, PartialEq, Eq)]
pub(super) enum WatchdogVerdict {
    Continue,
    Exit(i32),
}

/// Consecutive-failure state machine for the liveness watchdog (#936). Kept
/// pure and side-effect-free so the exit logic is unit-testable without a live
/// socket or `std::process::exit`. Failures before the surface is "armed" (the
/// grace window has elapsed, or a first success was observed) are ignored to
/// avoid a false-positive exit storm during a slow startup; any success resets
/// the counter.
pub(super) struct WatchdogCounter {
    max_failures: u32,
    consecutive_failures: u32,
    armed: bool,
}

impl WatchdogCounter {
    pub(super) fn new(max_failures: u32) -> Self {
        Self {
            max_failures,
            consecutive_failures: 0,
            armed: false,
        }
    }

    /// Record one probe outcome. `grace_elapsed` = the post-bind grace window
    /// has passed. Returns whether the daemon should exit.
    pub(super) fn record(&mut self, probe: Probe, grace_elapsed: bool) -> WatchdogVerdict {
        match probe {
            // Any answered response — 2xx OR a degraded non-2xx — proves the
            // HTTP surface is alive, so it arms and resets the counter. Only a
            // transport failure (no response) is treated as a wedge.
            Probe::Healthy | Probe::Degraded => {
                self.armed = true;
                self.consecutive_failures = 0;
                WatchdogVerdict::Continue
            }
            Probe::Unreachable => {
                if !self.armed && !grace_elapsed {
                    // Still inside the startup grace window and never healthy
                    // yet — do not count this as a wedge.
                    return WatchdogVerdict::Continue;
                }
                // Grace elapsed arms the counter even without a prior success:
                // a surface that never once answered is itself a failure.
                self.armed = true;
                self.consecutive_failures += 1;
                if self.max_failures > 0 && self.consecutive_failures >= self.max_failures {
                    WatchdogVerdict::Exit(2)
                } else {
                    WatchdogVerdict::Continue
                }
            }
        }
    }
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

/// RAII guard for the daemon discovery pid file (#936 review follow-up).
///
/// Construct it right after the pid path is computed and BEFORE the bind
/// attempt, so any early `?`-shaped return between there and the discovery
/// file actually being written (chiefly: a failed bind) runs this guard's
/// `Drop`, removing a STALE discovery pid file a prior run left behind.
/// Mirrors the existing [`crate::daemon_lock::DaemonLock`] Drop pattern: a
/// same-shaped guard rather than a bespoke defer closure, so the cleanup
/// contract reads the same way at both call sites.
///
/// Note: `std::process::exit` (used on the fail-loud serve and watchdog exit
/// paths) does NOT run destructors, so this guard cannot replace the explicit
/// `tokio::fs::remove_file` calls on those paths — it only protects the
/// early-`?`-return paths that precede bind, plus the normal graceful-
/// shutdown fallthrough (which calls [`DiscoveryPidGuard::disarm`] after
/// doing its own explicit removal, so the guard's `Drop` there is a no-op
/// rather than a redundant duplicate attempt).
struct DiscoveryPidGuard {
    path: PathBuf,
    disarmed: bool,
}

impl DiscoveryPidGuard {
    fn new(path: PathBuf) -> Self {
        Self {
            path,
            disarmed: false,
        }
    }

    /// Mark cleanup as already handled by the call site; `Drop` becomes a
    /// no-op.
    fn disarm(&mut self) {
        self.disarmed = true;
    }
}

impl Drop for DiscoveryPidGuard {
    fn drop(&mut self) {
        if self.disarmed {
            return;
        }
        // Drop cannot be async; a small pid-file removal is an acceptable
        // synchronous fs op here (the same tradeoff `DaemonLock::drop`
        // makes). Best-effort: a bind failure with no prior discovery file
        // (e.g. the very first run) is a harmless no-op.
        let _ = std::fs::remove_file(&self.path);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // #1308 follow-up: `normalize_malformed_mcp_json_response` /
    // `parse_error_response` moved into `malformed_json_middleware.rs`
    // (own unit test lives there now, alongside the fn). portable-server
    // is deliberately zero-dependency on tachi-server (see
    // `crates/portable-server/Cargo.toml`), so there is no shared crate to
    // hold this ~30-line axum middleware in without adding a new
    // dependency edge — the ruled-on tradeoff is an enforced-parity
    // duplicate instead. This test is the enforcement: it fails loudly if
    // the two copies ever drift.
    #[test]
    fn malformed_json_middleware_stays_in_parity_with_portable_server() {
        let tachi_server_copy = include_str!("malformed_json_middleware.rs");
        let portable_server_copy =
            include_str!("../../../../portable-server/src/malformed_json_middleware.rs");
        assert_eq!(
            tachi_server_copy, portable_server_copy,
            "crates/tachi-server/src/bootstrap/serve/malformed_json_middleware.rs \
             and crates/portable-server/src/malformed_json_middleware.rs must stay \
             byte-identical (tachi #1308 follow-up) — edit one, edit its twin too"
        );
    }

    #[test]
    fn health_payload_advertises_loopback_trust_and_reconnect() {
        let payload = daemon_health_payload(true, true, "ok");
        assert_eq!(payload["status"], "ok");
        assert_eq!(payload["transport"], "http");
        assert_eq!(payload["mcp"], "streamable-http");
        assert_eq!(payload["bind"], "127.0.0.1");
        assert_eq!(payload["auth_posture"], "loopback-trust-v1");
        assert_eq!(payload["reconnect"]["stdio_unaffected"], true);
        assert!(
            payload["reconnect"]["on_disconnect"]
                .as_str()
                .unwrap_or_default()
                .contains("re-initialize"),
            "reconnect must tell clients to re-init: {payload}"
        );
        assert!(
            payload["reconnect"]["docs"]
                .as_str()
                .unwrap_or_default()
                .contains("http-direct-connect.md"),
            "health must point at the cookbook: {payload}"
        );
    }

    #[test]
    fn manifest_background_only_runs_for_default_global_db() {
        let tmp = tempfile::tempdir().expect("tmp");
        let app_home = tmp.path().join(".tachi");
        let default_global = app_home.join("global").join(memcore::MEMORY_DB_FILENAME);
        let agent_global = app_home
            .join("agents")
            .join("main")
            .join(memcore::MEMORY_DB_FILENAME);
        std::fs::create_dir_all(default_global.parent().unwrap()).expect("default parent");
        std::fs::create_dir_all(agent_global.parent().unwrap()).expect("agent parent");
        std::fs::write(&default_global, b"").expect("default db");
        std::fs::write(&agent_global, b"").expect("agent db");

        let project_db = app_home
            .join("projects")
            .join("repo")
            .join(memcore::MEMORY_DB_FILENAME);

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

    // ---- #936 fail-loud serve contract ---------------------------------

    #[test]
    fn serve_exit_ok_after_shutdown_is_clean() {
        // Graceful path: axum returned Ok because `ct` was cancelled by a
        // signal / idle-reaper. This is the only non-fatal outcome.
        assert_eq!(serve_exit_decision(&Ok(()), true), ExitDecision::Clean);
    }

    #[test]
    fn serve_exit_ok_without_shutdown_is_fatal() {
        // The #936 core bug: accept loop ended on its own, no shutdown signal.
        // Pre-fix this was swallowed and the fn returned Ok(()) → process lived
        // on deaf. Now it must be fatal, exit code 1.
        match serve_exit_decision(&Ok(()), false) {
            ExitDecision::Fatal { code, reason } => {
                assert_eq!(code, 1);
                assert!(
                    reason.contains("without a shutdown signal"),
                    "reason should name the missing shutdown signal: {reason}"
                );
            }
            other => panic!("expected Fatal, got {other:?}"),
        }
    }

    #[test]
    fn serve_exit_err_is_fatal() {
        let err = std::io::Error::new(std::io::ErrorKind::Other, "listener exploded");
        // An error is fatal regardless of whether a shutdown was in progress.
        for cancelled in [false, true] {
            match serve_exit_decision(&Err(err_like(&err)), cancelled) {
                ExitDecision::Fatal { code, reason } => {
                    assert_eq!(code, 1);
                    assert!(reason.contains("listener exploded"), "reason: {reason}");
                }
                other => panic!("expected Fatal (cancelled={cancelled}), got {other:?}"),
            }
        }
    }

    fn err_like(e: &std::io::Error) -> std::io::Error {
        std::io::Error::new(e.kind(), e.to_string())
    }

    // ---- #936 liveness watchdog consecutive-failure logic ---------------

    #[test]
    fn watchdog_exits_after_max_consecutive_failures() {
        let mut c = WatchdogCounter::new(4);
        // Grace elapsed → failures count. First 3 keep going, 4th exits(2).
        assert_eq!(
            c.record(Probe::Unreachable, true),
            WatchdogVerdict::Continue
        );
        assert_eq!(
            c.record(Probe::Unreachable, true),
            WatchdogVerdict::Continue
        );
        assert_eq!(
            c.record(Probe::Unreachable, true),
            WatchdogVerdict::Continue
        );
        assert_eq!(c.record(Probe::Unreachable, true), WatchdogVerdict::Exit(2));
    }

    #[test]
    fn watchdog_success_resets_the_counter() {
        let mut c = WatchdogCounter::new(4);
        assert_eq!(
            c.record(Probe::Unreachable, true),
            WatchdogVerdict::Continue
        );
        assert_eq!(
            c.record(Probe::Unreachable, true),
            WatchdogVerdict::Continue
        );
        // A single healthy probe wipes the streak…
        assert_eq!(c.record(Probe::Healthy, true), WatchdogVerdict::Continue);
        // …so it now takes another full run of 4 to exit.
        assert_eq!(
            c.record(Probe::Unreachable, true),
            WatchdogVerdict::Continue
        );
        assert_eq!(
            c.record(Probe::Unreachable, true),
            WatchdogVerdict::Continue
        );
        assert_eq!(
            c.record(Probe::Unreachable, true),
            WatchdogVerdict::Continue
        );
        assert_eq!(c.record(Probe::Unreachable, true), WatchdogVerdict::Exit(2));
    }

    #[test]
    fn watchdog_ignores_failures_inside_grace_window() {
        let mut c = WatchdogCounter::new(2);
        // grace not yet elapsed and no success yet → failures ignored entirely.
        assert_eq!(
            c.record(Probe::Unreachable, false),
            WatchdogVerdict::Continue
        );
        assert_eq!(
            c.record(Probe::Unreachable, false),
            WatchdogVerdict::Continue
        );
        assert_eq!(
            c.record(Probe::Unreachable, false),
            WatchdogVerdict::Continue
        );
        // Once grace elapses, counting starts from zero.
        assert_eq!(
            c.record(Probe::Unreachable, true),
            WatchdogVerdict::Continue
        );
        assert_eq!(c.record(Probe::Unreachable, true), WatchdogVerdict::Exit(2));
    }

    #[test]
    fn watchdog_arms_on_first_success_even_before_grace() {
        let mut c = WatchdogCounter::new(2);
        // First success arms the counter, so a later failure counts even though
        // the grace window has not elapsed.
        assert_eq!(c.record(Probe::Healthy, false), WatchdogVerdict::Continue);
        assert_eq!(
            c.record(Probe::Unreachable, false),
            WatchdogVerdict::Continue
        );
        assert_eq!(
            c.record(Probe::Unreachable, false),
            WatchdogVerdict::Exit(2)
        );
    }

    #[test]
    fn watchdog_disabled_never_exits() {
        // max_failures == 0 is the disabled sentinel; record must never exit.
        let mut c = WatchdogCounter::new(0);
        for _ in 0..100 {
            assert_eq!(
                c.record(Probe::Unreachable, true),
                WatchdogVerdict::Continue
            );
        }
    }

    #[test]
    fn watchdog_answered_degraded_never_exits() {
        // Review MUST-FIX (#936): a SERVED non-2xx (e.g. 503 while the DB is
        // degraded) proves the HTTP surface is alive. It must reset the counter
        // exactly like a 2xx, so a daemon that answers 503 forever is never
        // killed by the watchdog.
        let mut c = WatchdogCounter::new(2);
        for _ in 0..100 {
            assert_eq!(c.record(Probe::Degraded, true), WatchdogVerdict::Continue);
        }
        // A degraded answer also resets an in-progress transport-failure streak.
        assert_eq!(
            c.record(Probe::Unreachable, true),
            WatchdogVerdict::Continue
        );
        assert_eq!(c.record(Probe::Degraded, true), WatchdogVerdict::Continue);
        assert_eq!(
            c.record(Probe::Unreachable, true),
            WatchdogVerdict::Continue
        );
        assert_eq!(c.record(Probe::Unreachable, true), WatchdogVerdict::Exit(2));
    }

    #[test]
    fn watchdog_armed_by_default_wiring_guard() {
        // Wiring guard (#936): the liveness watchdog spawn in `serve_http_daemon`
        // is gated on `daemon_watchdog_config()` returning Some. If the default
        // flips to disabled, the watchdog silently un-wires — this pins the
        // default armed (threshold 4, interval clamped >= 5s). The fail-loud
        // select arm's contract is separately guarded by
        // `serve_exit_ok_without_shutdown_is_fatal`.
        let prev = std::env::var("TACHI_DAEMON_WATCHDOG_FAILS").ok();
        std::env::remove_var("TACHI_DAEMON_WATCHDOG_FAILS");
        let cfg = daemon_watchdog_config();
        match prev {
            Some(v) => std::env::set_var("TACHI_DAEMON_WATCHDOG_FAILS", v),
            None => std::env::remove_var("TACHI_DAEMON_WATCHDOG_FAILS"),
        }
        let (interval, fails) = cfg.expect("watchdog must be armed by default");
        assert_eq!(fails, 4, "default consecutive-failure threshold");
        assert!(interval.as_secs() >= 5, "interval clamped to >= 5s");
    }

    // ---- #936 integration: real socket self-probe drives the decision ----

    #[tokio::test]
    async fn watchdog_probe_through_real_socket_then_fatal_when_dead() {
        use tokio_util::sync::CancellationToken;

        // Bind a real server on an ephemeral port with a /health route, the
        // same shape the daemon exposes, and drive it with a live reqwest
        // client — proving the self-probe goes through the socket, not the fn.
        let ct = CancellationToken::new();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind ephemeral");
        let addr = listener.local_addr().expect("addr");
        let router = axum::Router::new().route(
            "/health",
            axum::routing::get(|| async { axum::http::StatusCode::OK }),
        );
        let ct_serve = ct.clone();
        let serve = tokio::spawn(async move {
            let result = axum::serve(listener, router)
                .with_graceful_shutdown(async move { ct_serve.cancelled_owned().await })
                .await;
            // Assert the fail-loud contract on the real serve result: it was
            // shut down via the token, so this must read as Clean.
            serve_exit_decision(&result, true)
        });

        crate::ensure_tls_provider();
        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(5))
            .build()
            .expect("client");
        let health_url = format!("http://{addr}/health");
        let mut counter = WatchdogCounter::new(4);

        // Live socket answers → Healthy → counter stays Continue.
        let resp = client.get(&health_url).send().await.expect("probe live");
        assert!(resp.status().is_success());
        assert_eq!(
            counter.record(Probe::Healthy, true),
            WatchdogVerdict::Continue
        );

        // Shut the surface down (simulates the wedge/death) and drain it.
        ct.cancel();
        let decision = serve.await.expect("join serve");
        assert_eq!(decision, ExitDecision::Clean);

        // Now real probes fail through the (closed) socket → after 4 the
        // watchdog would exit(2). We assert the decision, never call exit.
        let mut last = WatchdogVerdict::Continue;
        for _ in 0..4 {
            let probe = match client.get(&health_url).send().await {
                Ok(r) if r.status().is_success() => Probe::Healthy,
                _ => Probe::Unreachable,
            };
            assert_eq!(
                probe,
                Probe::Unreachable,
                "surface must be dead post-cancel"
            );
            last = counter.record(probe, true);
        }
        assert_eq!(last, WatchdogVerdict::Exit(2));
    }

    // ---- #936 review follow-up: bind-failure discovery-pid cleanup -------

    #[tokio::test]
    async fn bind_failure_removes_stale_discovery_pid_and_returns_err() {
        // The reported bug: a bind failure (port conflict, etc.) used to
        // return before the discovery pid path even existed as a local
        // binding, so a STALE pid file from a PRIOR successful run survived —
        // and under `KeepAlive` the daemon retries every ~10s carrying that
        // stale discovery state forever. Simulate exactly that: seed a stale
        // discovery pid file, then occupy the port so this run's own bind
        // fails.
        let tmp = tempfile::tempdir().expect("tempdir");
        let app_home = tmp.path().join(".tachi");
        std::fs::create_dir_all(&app_home).expect("app_home");
        let global_db = tmp.path().join("global.db");
        let server = crate::MemoryServer::new(global_db.clone(), None).expect("server");

        let pid_path = crate::daemon_lock::scoped_daemon_pid_path(&app_home, &global_db);
        if let Some(parent) = pid_path.parent() {
            std::fs::create_dir_all(parent).expect("pid parent");
        }
        std::fs::write(&pid_path, br#"{"pid": 999999}"#).expect("seed stale discovery pid");
        assert!(pid_path.exists(), "precondition: stale pid file seeded");

        // Occupy the port first so this run's own bind attempt fails.
        let blocker = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind blocker");
        let port = blocker.local_addr().expect("blocker addr").port();

        let result = serve_http_daemon(server, app_home, global_db, None, port).await;

        assert!(
            result.is_err(),
            "binding onto an already-occupied port must return Err"
        );
        assert!(
            !pid_path.exists(),
            "a bind failure must not leave a stale discovery pid file behind"
        );

        drop(blocker);
    }
}
