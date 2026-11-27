use super::*;
use std::sync::Arc;

use super::malformed_json_middleware::normalize_malformed_mcp_json_response;

pub(super) fn acquire_startup_daemon_lock(
    app_home: &Path,
    global_db_path: &Path,
) -> Result<crate::daemon_lock::DaemonLock, Box<dyn std::error::Error>> {
    let path = crate::daemon_lock::scoped_daemon_lock_path(app_home, global_db_path);
    crate::daemon_lock::DaemonLock::acquire(&path)
        .map_err(|error| format!("tachi daemon singleton lock unavailable: {error}").into())
}

pub(super) async fn serve_http_daemon(
    server: MemoryServer,
    app_home: PathBuf,
    global_db_path: PathBuf,
    project_db_path: Option<PathBuf>,
    port: u16,
    _daemon_lock: &crate::daemon_lock::DaemonLock,
) -> Result<(), Box<dyn std::error::Error>> {
    // The process was marked as a daemon (`TACHI_DAEMON`) before this runtime
    // was built, in `tokio_main`; it is not changed while workers exist.

    // HTTP daemon mode.
    // In daemon mode the cwd-derived repo-local project DB is RETAINED
    // (see the `cli.daemon && project_db_path.is_some()` block above): the
    // multi-DB FoundryScheduler gives every manifest DB equal coverage, so
    // we keep the auto-detected project and only emit a warning that the
    // daemon's bound project follows the launch cwd. Users can still pin a
    // single project explicitly via --project-db.

    // Startup acquired this scoped guard before initializing stores or workers.
    // Keep ownership through HTTP serving and orderly shutdown.
    let internal_proxy_token = uuid::Uuid::new_v4().simple().to_string();
    server.set_daemon_proxy_token(internal_proxy_token.clone());

    // Reconcile only while the startup singleton guard is held. Foreign
    // controller epochs are observed as orphaned, never relaunched or adopted.
    crate::managed_run_epoch::record_startup_reconciliation(&server);

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
        let scheduler =
            crate::vector_sweep::VectorSweepScheduler::start(manifest_path.clone(), server.clone());
        eprintln!(
            "[daemon] vector sweep scheduled (manifest={})",
            manifest_path.display()
        );
        scheduler
    } else {
        let scheduler = crate::vector_sweep::VectorSweepScheduler::start_own_dbs(
            manifest_path.clone(),
            server.clone(),
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

    use tokio_util::sync::CancellationToken;

    let ct = CancellationToken::new();

    if manifest_background {
        let daily_server = server.clone();
        spawn_background_loop(
            "daily_pipeline",
            ct.clone(),
            LOOP_RESTART_PAUSE,
            move || {
                let daily_server = daily_server.clone();
                Box::pin(async move {
                    loop {
                        let next_run = crate::daily_pipeline::next_daily_run_time();
                        tokio::time::sleep_until(next_run).await;
                        let app_home = daily_server.tachi_home_dir();
                        let global_db_path = daily_server.global_db_path_buf();
                        if let Err(e) =
                            crate::status_ops::status_health::refresh_provider_probe_cache(
                                &app_home,
                                &global_db_path,
                            )
                            .await
                        {
                            eprintln!(
                                "[daily_pipeline] provider key probe cache refresh skipped: {e}"
                            );
                        }
                        match crate::daily_pipeline::run_daily_pipeline(&daily_server).await {
                            Ok(report) => {
                                eprintln!("[daily-pipeline] completed: {}", report.summary())
                            }
                            Err(e) => eprintln!("[daily-pipeline] failed: {e}"),
                        }
                    }
                })
            },
        );
        eprintln!("[daemon] daily pipeline scheduled for 04:00 Asia/Shanghai");
    } else {
        eprintln!("[daemon] daily pipeline disabled for scoped daemon");
    }

    // Discovery pid path + its owner-scoped RAII cleanup guard, computed/armed
    // only after the scoped daemon lock above was acquired, but BEFORE the
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
    server.set_published_daemon_mcp_url(format!("http://{bind_addr}/mcp"));

    // Warm only the admitted daemon's existing provider pool, never a CLI
    // one-shot or a failed bind. Startup and the first query do not await it.
    // Cancellation ends the optional work when the daemon shuts down.
    let voyage_prewarm = spawn_voyage_prewarm(server.llm.clone(), ct.clone());

    // Idle shutdown is an explicit operator choice. Native HTTP clients need
    // the resident service to remain available without a stdio process.
    let idle_shutdown = tokio_util::sync::CancellationToken::new();
    if let Some(idle_timeout) = daemon_idle_timeout() {
        let clock = server.activity_clock();
        let idle_target = idle_shutdown.clone();
        let tick = std::time::Duration::from_secs(idle_timeout.as_secs().clamp(5, 60));
        spawn_background_loop("idle_reaper", ct.clone(), LOOP_RESTART_PAUSE, move || {
            let idle_target = idle_target.clone();
            let clock = clock.clone();
            Box::pin(async move {
                loop {
                    tokio::time::sleep(tick).await;
                    let last = clock.load(std::sync::atomic::Ordering::Relaxed);
                    let idle_ms = jiff::Timestamp::now().as_nanosecond().div_euclid(1_000_000)
                        - i128::from(last);
                    if idle_ms >= idle_timeout.as_millis() as i128 {
                        eprintln!(
                        "[idle-reaper] daemon idle {}s (limit {}s); shutting down; a supervisor or operator must restart it for native HTTP clients",
                        idle_ms / 1000,
                        idle_timeout.as_secs()
                    );
                        idle_target.cancel();
                        break;
                    }
                }
            })
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
        spawn_background_loop(
            "liveness_watchdog",
            ct.clone(),
            LOOP_RESTART_PAUSE,
            move || {
                let health_url = health_url.clone();
                let pid_path_watchdog = pid_path_watchdog.clone();
                let ct_watchdog = ct_watchdog.clone();
                Box::pin(async move {
                    // reqwest is built with `rustls-no-provider`; install the crypto
                    // provider before building any client or `.build()` panics (even
                    // for loopback http, the builder resolves the provider eagerly).
                    crate::ensure_tls_provider();
                    // Loopback only: an environment or system proxy must
                    // neither receive the probe nor turn into false
                    // "surface dead" exits.
                    let client = match reqwest::Client::builder()
                        .no_proxy()
                        .connect_timeout(std::time::Duration::from_secs(2))
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
                                // A shutdown that arrived while the file was
                                // being removed owns the exit from here.
                                if ct_watchdog.is_cancelled() {
                                    break;
                                }
                                std::process::exit(code);
                            }
                        }
                    }
                })
            },
        );
    }

    let mcp_ct = tokio_util::sync::CancellationToken::new();
    let router = daemon_http_router(server, mcp_ct.clone());
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
        "started_at": memcore::time_compat::rfc3339_compat(jiff::Timestamp::now()),
        "version": crate::build_info::PKG_VERSION,
        "git_sha": crate::build_info::GIT_SHA,
        "internal_proxy_token": internal_proxy_token,
    });
    if let Some(parent) = pid_path.parent() {
        let _ = tokio::fs::create_dir_all(parent).await;
    }
    let pid_body = serde_json::to_string_pretty(&pid_payload).unwrap_or_default();
    match tokio::task::spawn_blocking({
        let pid_path = pid_path.clone();
        move || crate::utils::write_owner_only_file_atomic(&pid_path, pid_body.as_bytes())
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

    let signal = async {
        tokio::select! {
            _ = tokio::signal::ctrl_c() => "SIGINT",
            _ = sigterm() => "SIGTERM",
            _ = idle_shutdown.cancelled() => "idle timeout",
        }
    };
    match serve_until_shutdown(
        listener,
        router,
        ct.clone(),
        mcp_ct.clone(),
        SHUTDOWN_DRAIN_TIMEOUT,
        signal,
    )
    .await
    {
        Served::ByItself(result) => {
            // FAIL-LOUD serve contract (#936): the HTTP serve future must only
            // resolve because we asked it to (the idle-reaper cancels `ct`). Any
            // *self-initiated* completion — accept loop ending on its own (`Ok`)
            // or an `axum::serve` error (`Err`) — means the MCP/HTTP surface is
            // dead while the process is otherwise alive. Before this fix both
            // were swallowed and the fn returned `Ok(())` unconditionally, so
            // the process kept running deaf and launchd saw `state=running`.
            // Now we exit non-zero so `KeepAlive` respawns a daemon that can
            // actually serve.
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
        Served::Signalled { signal, drained } => {
            if drained {
                eprintln!("Received {signal}; HTTP surface drained");
            } else {
                eprintln!(
                    "Received {signal}; in-flight requests did not finish within {}s, continuing shutdown",
                    SHUTDOWN_DRAIN_TIMEOUT.as_secs()
                );
            }
        }
    }

    if let Some(task) = voyage_prewarm {
        task.abort();
        let _ = task.await;
    }
    // Best-effort cleanup of daemon discovery file
    let _ = tokio::fs::remove_file(&pid_path_cleanup).await;
    // This call site already did its own (async, logged-on-failure) removal
    // above, so disarm the guard rather than let its synchronous Drop attempt
    // a redundant no-op removal.
    pid_guard.disarm();
    Ok(())
}

fn spawn_voyage_prewarm(
    llm: Arc<tachi_llm::LlmClient>,
    cancellation: tokio_util::sync::CancellationToken,
) -> Option<tokio::task::JoinHandle<()>> {
    if !crate::utils::parse_env_bool("TACHI_VOYAGE_PREWARM").unwrap_or(true)
        || crate::utils::parse_env_bool("TACHI_SEARCH_DISABLE_QUERY_EMBEDDING").unwrap_or(false)
    {
        return None;
    }
    Some(tokio::spawn(async move {
        tokio::select! {
            biased;
            _ = cancellation.cancelled() => {}
            _ = llm.prewarm_voyage_connection() => {}
        }
    }))
}

#[cfg(test)]
type HealthReadHook = Arc<dyn Fn() + Send + Sync>;

#[cfg(test)]
static HEALTH_READ_HOOKS: std::sync::LazyLock<
    std::sync::Mutex<std::collections::HashMap<PathBuf, HealthReadHook>>,
> = std::sync::LazyLock::new(|| std::sync::Mutex::new(std::collections::HashMap::new()));

#[cfg(test)]
pub(crate) struct HealthReadHookGuard {
    path: PathBuf,
    callback: HealthReadHook,
}

#[cfg(test)]
impl Drop for HealthReadHookGuard {
    fn drop(&mut self) {
        let mut hooks = HEALTH_READ_HOOKS.lock().unwrap_or_else(|e| e.into_inner());
        if hooks
            .get(&self.path)
            .is_some_and(|hook| Arc::ptr_eq(hook, &self.callback))
        {
            hooks.remove(&self.path);
        }
    }
}

#[cfg(test)]
pub(crate) fn observe_daemon_health_reads_for_test(
    path: PathBuf,
    callback: impl Fn() + Send + Sync + 'static,
) -> HealthReadHookGuard {
    let callback: HealthReadHook = Arc::new(callback);
    let mut hooks = HEALTH_READ_HOOKS.lock().unwrap_or_else(|e| e.into_inner());
    assert!(
        !hooks.contains_key(&path),
        "health hook already installed for fixture"
    );
    hooks.insert(path.clone(), callback.clone());
    HealthReadHookGuard { path, callback }
}

#[cfg(test)]
fn notify_health_read_for_test(path: &Path) {
    let callback = HEALTH_READ_HOOKS
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .get(path)
        .cloned();
    if let Some(callback) = callback {
        callback();
    }
}

/// Construct the same HTTP routes for deployment and isolated runtime tests.
pub(crate) fn daemon_http_router(
    server: MemoryServer,
    ct: tokio_util::sync::CancellationToken,
) -> axum::Router {
    use rmcp::transport::streamable_http_server::{
        session::local::LocalSessionManager, StreamableHttpServerConfig, StreamableHttpService,
    };

    let vault_router = super::super::vault_cli::daemon::vault_admin_router(server.clone());
    let health_server = server.clone();
    static HEALTH_SEATS: std::sync::OnceLock<Arc<tokio::sync::Semaphore>> =
        std::sync::OnceLock::new();
    let health_seats = HEALTH_SEATS
        .get_or_init(|| Arc::new(tokio::sync::Semaphore::new(1)))
        .clone();

    let mut http_config = StreamableHttpServerConfig::default();
    http_config.legacy_session_mode = true;
    http_config.stateless_protocol_metadata_required = true;
    http_config.cancellation_token = ct.child_token();
    // No browser origin is registered. A page on another site must not drive
    // the daemon through a visitor's browser; clients that send no `Origin`
    // (CLI, MCP hosts) are unaffected.
    let http_config = http_config.enforce_origin_validation();

    let service = StreamableHttpService::new(
        move || Ok(server.clone_for_mcp_session()),
        Arc::new(LocalSessionManager::default()),
        http_config,
    );

    axum::Router::new()
        .merge(vault_router)
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
                let health_seats = health_seats.clone();
                async move {
                    let (db_ok, vec_available) =
                        crate::executor_offload::run_bounded_blocking(health_seats, move || {
                            #[cfg(test)]
                            notify_health_read_for_test(&health_server.global_db_path_buf());
                            let db_ok = health_server
                                .with_global_store_read(|store| {
                                    store.probe_readable().map_err(|e| e.to_string())
                                })
                                .is_ok();
                            let vec_available = health_server.global_vec_available();
                            (db_ok, vec_available)
                        })
                        .await
                        .unwrap_or((false, false));
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
        ))
        // Outermost: it runs before any route, including `/health`.
        .layer(axum::middleware::from_fn(require_loopback_host))
}

fn is_valid_host_port(port: &str) -> bool {
    !port.is_empty()
        && port.bytes().all(|byte| byte.is_ascii_digit())
        && port.parse::<u16>().is_ok()
}

/// Whether a `Host` header names the loopback daemon: `localhost`, `127.0.0.1`
/// or `[::1]`, each with an optional valid port. It mirrors the hosts the
/// `/mcp` service accepts on its own.
pub(super) fn is_loopback_host(value: &str) -> bool {
    let value = value.trim();
    if let Some(bracketed) = value.strip_prefix('[') {
        let Some((name, rest)) = bracketed.split_once(']') else {
            return false;
        };
        if name != "::1" {
            return false;
        }
        return rest.is_empty() || rest.strip_prefix(':').is_some_and(is_valid_host_port);
    }
    let (name, port) = match value.rsplit_once(':') {
        Some((name, port)) => (name, Some(port)),
        None => (value, None),
    };
    port.is_none_or(is_valid_host_port)
        && (name.eq_ignore_ascii_case("localhost") || name == "127.0.0.1")
}

/// Rejects requests whose `Host` does not name the loopback daemon.
///
/// The listener binds 127.0.0.1 only, but a page on another site can still
/// reach it through a name it controls (DNS rebinding) and would otherwise
/// read `/health` or present the owner capability to `/internal/vault`.
async fn require_loopback_host(
    request: axum::extract::Request,
    next: axum::middleware::Next,
) -> axum::response::Response {
    let named_loopback = request
        .headers()
        .get(axum::http::header::HOST)
        .and_then(|value| value.to_str().ok())
        .is_some_and(is_loopback_host);
    if !named_loopback {
        // The rejected value is attacker-chosen text and is not logged.
        tracing::warn!(
            target: "tachi::daemon",
            "rejected request whose Host is not the loopback daemon"
        );
        return axum::response::IntoResponse::into_response((
            axum::http::StatusCode::FORBIDDEN,
            axum::Json(serde_json::json!({ "error": "Host header is not allowed" })),
        ));
    }
    next.run(request).await
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
/// behavior after daemon restart.
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

/// How long a shutdown signal waits for in-flight requests to finish. A client
/// that keeps a stream open must not hold the daemon in shutdown: the rest of
/// the teardown (WAL drain, lock release) still has to run.
const SHUTDOWN_DRAIN_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);

/// What ended `serve_until_shutdown`.
#[derive(Debug)]
enum Served {
    /// The serve future stopped without a shutdown signal, or failed.
    ByItself(std::io::Result<()>),
    /// A shutdown signal arrived. `ct` was cancelled and the serve future was
    /// then awaited; `drained` says whether every in-flight request finished.
    Signalled { signal: &'static str, drained: bool },
}

/// Serves `router` until the surface stops by itself or `shutdown` resolves.
///
/// A shutdown signal cancels `ct` so `with_graceful_shutdown` stops accepting
/// new connections and drains in-flight requests, while keeping `mcp_ct` live
/// until the bounded drain finishes so in-flight `rmcp` SSE/JSON-RPC responses
/// complete cleanly before session teardown.
async fn serve_until_shutdown<S: std::future::Future<Output = &'static str>>(
    listener: tokio::net::TcpListener,
    router: axum::Router,
    ct: tokio_util::sync::CancellationToken,
    mcp_ct: tokio_util::sync::CancellationToken,
    drain_timeout: std::time::Duration,
    shutdown: S,
) -> Served {
    let graceful = ct.clone();
    // `axum::serve(..)` and `with_graceful_shutdown` build an `IntoFuture`;
    // polling it by reference in `select!` needs the future itself.
    let serve = async move {
        axum::serve(listener, router)
            .with_graceful_shutdown(async move { graceful.cancelled_owned().await })
            .await
    };
    tokio::pin!(serve);
    tokio::pin!(shutdown);
    let signal;
    tokio::select! {
        result = &mut serve => {
            mcp_ct.cancel();
            return Served::ByItself(result);
        }
        received = &mut shutdown => signal = received,
    }
    ct.cancel();
    let drained = matches!(
        tokio::time::timeout(drain_timeout, &mut serve).await,
        Ok(Ok(()))
    );
    mcp_ct.cancel();
    Served::Signalled { signal, drained }
}

/// First pause before a failed background loop is restarted, and its ceiling.
const LOOP_RESTART_PAUSE: std::time::Duration = std::time::Duration::from_secs(1);
const LOOP_RESTART_PAUSE_MAX: std::time::Duration = std::time::Duration::from_secs(300);
/// A loop that ran this long before ending was healthy; its next restart waits
/// only the first pause again.
const LOOP_STEADY_RUN: std::time::Duration = std::time::Duration::from_secs(60);

/// Keeps one of the daemon's background loops alive and its failures visible.
///
/// The loops used to be spawned and forgotten: a panic inside one ended it
/// silently until the next process restart. Each run is now its own task. When
/// it ends before the daemon shuts down, the end is logged and the loop starts
/// again after a pause, so one failing run does not disable it for the life of
/// the process.
fn spawn_background_loop(
    name: &'static str,
    shutdown: tokio_util::sync::CancellationToken,
    restart_pause: std::time::Duration,
    start: impl Fn() -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send>> + Send + 'static,
) {
    tokio::spawn(async move {
        let mut pause = restart_pause;
        loop {
            let started = std::time::Instant::now();
            let outcome = tokio::spawn(start()).await;
            if shutdown.is_cancelled() {
                break;
            }
            match outcome {
                Err(error) if error.is_panic() => {
                    let payload = error.into_panic();
                    let message = payload
                        .downcast_ref::<&str>()
                        .copied()
                        .or_else(|| payload.downcast_ref::<String>().map(String::as_str))
                        .unwrap_or("non-string panic payload");
                    tracing::error!(
                        target: "tachi::daemon",
                        loop_name = name,
                        panic = message,
                        "background loop panicked, restarting"
                    );
                }
                _ => tracing::error!(
                    target: "tachi::daemon",
                    loop_name = name,
                    "background loop ended on its own, restarting"
                ),
            }
            if started.elapsed() >= LOOP_STEADY_RUN {
                pause = restart_pause;
            }
            tokio::select! {
                _ = shutdown.cancelled() => break,
                _ = tokio::time::sleep(pause) => {}
            }
            pause = pause.saturating_mul(2).min(LOOP_RESTART_PAUSE_MAX);
        }
    });
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
/// Unlike [`crate::daemon_lock::DaemonLock`], which keeps its stable lock path
/// after releasing `flock`, this guard owns a discovery *receipt* whose stale
/// contents must be removed on an early owner return.
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
        // Drop cannot be async; a small receipt-file removal is an acceptable
        // synchronous fs op here. Best-effort: a bind failure with no prior
        // discovery file (e.g. the very first run) is a harmless no-op.
        let _ = std::fs::remove_file(&self.path);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// One route whose handler reports that it is running, waits for the daemon
    /// token to be cancelled and then for `release`: a request that sees the
    /// shutdown and still finishes its work.
    fn draining_router(
        started: Arc<tokio::sync::Notify>,
        release: Arc<tokio::sync::Notify>,
        ct: tokio_util::sync::CancellationToken,
    ) -> axum::Router {
        axum::Router::new().route(
            "/slow",
            axum::routing::get(move || {
                let started = started.clone();
                let release = release.clone();
                async move {
                    started.notify_one();
                    ct.cancelled().await;
                    release.notified().await;
                    "drained"
                }
            }),
        )
    }

    async fn get(url: &str) -> Result<String, Box<dyn std::error::Error + Send + Sync>> {
        crate::ensure_tls_provider();
        let client = reqwest::Client::builder()
            .no_proxy()
            .timeout(std::time::Duration::from_secs(5))
            .build()?;
        Ok(client
            .get(url)
            .send()
            .await?
            .error_for_status()?
            .text()
            .await?)
    }

    #[tokio::test]
    async fn a_shutdown_signal_drains_in_flight_requests_instead_of_dropping_them() {
        let ct = tokio_util::sync::CancellationToken::new();
        let mcp_ct = tokio_util::sync::CancellationToken::new();
        let started = Arc::new(tokio::sync::Notify::new());
        let release = Arc::new(tokio::sync::Notify::new());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind ephemeral");
        let url = format!("http://{}/slow", listener.local_addr().expect("addr"));
        let (signal, signalled) = tokio::sync::oneshot::channel::<()>();
        let serving = tokio::spawn(serve_until_shutdown(
            listener,
            draining_router(started.clone(), release.clone(), ct.clone()),
            ct.clone(),
            mcp_ct.clone(),
            std::time::Duration::from_secs(5),
            async {
                let _ = signalled.await;
                "SIGTERM"
            },
        ));
        let response = tokio::spawn(async move { get(&url).await });
        // The request is being handled before the signal arrives.
        started.notified().await;
        signal.send(()).expect("signal the server task");
        // Shutdown has begun, and the request is still working on purpose.
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        assert!(ct.is_cancelled(), "the signal cancels the daemon token");
        assert!(
            !mcp_ct.is_cancelled(),
            "mcp_ct must remain uncancelled while in-flight requests drain"
        );
        assert!(
            !serving.is_finished(),
            "shutdown must wait while a request is still in flight"
        );
        // The request finishes; only then does shutdown continue.
        release.notify_one();
        assert_eq!(
            response.await.unwrap().unwrap(),
            "drained",
            "an in-flight request must finish after the shutdown signal"
        );
        assert!(
            matches!(
                serving.await.unwrap(),
                Served::Signalled {
                    signal: "SIGTERM",
                    drained: true
                }
            ),
            "the serve future is awaited, not dropped with the signal"
        );
        assert!(
            mcp_ct.is_cancelled(),
            "mcp_ct is cancelled after drain completes"
        );
    }

    #[tokio::test]
    async fn a_request_that_ignores_shutdown_cannot_hold_the_daemon_open() {
        let ct = tokio_util::sync::CancellationToken::new();
        let mcp_ct = tokio_util::sync::CancellationToken::new();
        let started = Arc::new(tokio::sync::Notify::new());
        let handler_started = started.clone();
        let router = axum::Router::new().route(
            "/stuck",
            axum::routing::get(move || {
                let started = handler_started.clone();
                async move {
                    started.notify_one();
                    std::future::pending::<&'static str>().await
                }
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind ephemeral");
        let url = format!("http://{}/stuck", listener.local_addr().expect("addr"));
        let (signal, signalled) = tokio::sync::oneshot::channel::<()>();
        let serving = tokio::spawn(serve_until_shutdown(
            listener,
            router,
            ct.clone(),
            mcp_ct.clone(),
            std::time::Duration::from_millis(200),
            async {
                let _ = signalled.await;
                "SIGINT"
            },
        ));
        let response = tokio::spawn(async move { get(&url).await });
        started.notified().await;
        let signalled_at = std::time::Instant::now();
        signal.send(()).expect("signal the server task");
        // hyper's graceful shutdown waits for in-flight requests, so this one
        // never finishes; the process ends it when the daemon exits.
        let served = tokio::time::timeout(std::time::Duration::from_secs(2), serving)
            .await
            .expect("shutdown must not wait for a request that ignores it")
            .unwrap();
        assert!(
            matches!(
                served,
                Served::Signalled {
                    signal: "SIGINT",
                    drained: false
                }
            ),
            "shutdown continues once the drain bound passes"
        );
        assert!(
            signalled_at.elapsed() >= std::time::Duration::from_millis(200),
            "the drain bound is honoured before shutdown continues"
        );
        response.abort();
        assert!(ct.is_cancelled());
        assert!(
            mcp_ct.is_cancelled(),
            "mcp_ct is cancelled once the bounded drain finishes"
        );
    }

    #[tokio::test]
    async fn a_shutdown_signal_drains_in_flight_mcp_tool_call_over_sse() {
        crate::ensure_tls_provider();
        let tmp = tempfile::tempdir().expect("tempdir");
        let global_db = tmp.path().join("global/memory.db");
        std::fs::create_dir_all(global_db.parent().unwrap()).expect("global dir");
        let server = crate::MemoryServer::new(global_db, None).expect("server");

        let ct = tokio_util::sync::CancellationToken::new();
        let mcp_ct = tokio_util::sync::CancellationToken::new();
        let router = daemon_http_router(server, mcp_ct.clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind ephemeral");
        let url = format!("http://{}/mcp", listener.local_addr().expect("addr"));
        let (signal, signalled) = tokio::sync::oneshot::channel::<()>();
        let serving = tokio::spawn(serve_until_shutdown(
            listener,
            router,
            ct.clone(),
            mcp_ct.clone(),
            std::time::Duration::from_secs(5),
            async {
                let _ = signalled.await;
                "SIGTERM"
            },
        ));

        let client = reqwest::Client::builder()
            .no_proxy()
            .timeout(std::time::Duration::from_secs(5))
            .build()
            .expect("client");

        let init_resp = client
            .post(&url)
            .header("accept", "application/json, text/event-stream")
            .json(&serde_json::json!({
                "jsonrpc": "2.0",
                "id": 1,
                "method": "initialize",
                "params": {
                    "protocolVersion": "2024-11-05",
                    "capabilities": {},
                    "clientInfo": { "name": "drain-test", "version": "1.0" }
                }
            }))
            .send()
            .await
            .expect("initialize request");
        assert!(
            init_resp.status().is_success(),
            "initialize failed: {}",
            init_resp.status()
        );
        let session_id = init_resp
            .headers()
            .get("mcp-session-id")
            .expect("mcp-session-id")
            .to_str()
            .expect("session id UTF-8")
            .to_string();

        let initialized_resp = client
            .post(&url)
            .header("accept", "application/json, text/event-stream")
            .header("mcp-session-id", &session_id)
            .json(&serde_json::json!({
                "jsonrpc": "2.0",
                "method": "notifications/initialized"
            }))
            .send()
            .await
            .expect("initialized notification");
        assert!(
            initialized_resp.status().is_success(),
            "initialized failed: {}",
            initialized_resp.status()
        );

        let started = Arc::new(tokio::sync::Notify::new());
        let release = Arc::new(tokio::sync::Notify::new());
        let pause_token = uuid::Uuid::new_v4().simple().to_string();
        let _gate = crate::server_handler::arm_call_tool_gate(
            pause_token.clone(),
            started.clone(),
            release.clone(),
        );

        let call_client = client.clone();
        let call_url = url.clone();
        let call_session = session_id.clone();
        let response = tokio::spawn(async move {
            call_client
                .post(&call_url)
                .header("accept", "application/json, text/event-stream")
                .header("mcp-session-id", &call_session)
                .header("x-tachi-test-pause", &pause_token)
                .json(&serde_json::json!({
                    "jsonrpc": "2.0",
                    "id": 2,
                    "method": "tools/call",
                    "params": {
                        "name": "tachi_memory",
                        "arguments": { "action": "status" }
                    }
                }))
                .send()
                .await
        });

        started.notified().await;
        signal.send(()).expect("signal the server task");
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        assert!(ct.is_cancelled(), "the signal cancels the drain token");
        assert!(
            !mcp_ct.is_cancelled(),
            "mcp_ct must remain uncancelled while an in-flight MCP tool call drains"
        );
        assert!(
            !serving.is_finished(),
            "shutdown must wait while /mcp tools/call is in flight"
        );

        release.notify_one();
        let tool_resp = response
            .await
            .expect("tool task join")
            .expect("HTTP response after signal");
        assert_eq!(tool_resp.status(), reqwest::StatusCode::OK);
        let body = tool_resp.text().await.expect("MCP response body");
        let payload = serde_json::from_str::<serde_json::Value>(&body)
            .ok()
            .or_else(|| {
                body.lines()
                    .filter_map(|line| line.strip_prefix("data:"))
                    .map(str::trim)
                    .filter(|data| !data.is_empty())
                    .filter_map(|data| serde_json::from_str::<serde_json::Value>(data).ok())
                    .find(|msg| msg.get("id") == Some(&serde_json::json!(2)))
            })
            .unwrap_or_else(|| panic!("expected JSON-RPC id=2 response in body, got: {body:?}"));
        assert_eq!(payload["id"], serde_json::json!(2), "{payload:#}");
        assert!(
            payload.get("result").is_some(),
            "expected complete JSON-RPC result after drain, got: {payload:#}"
        );
        assert!(
            payload.get("error").is_none(),
            "unexpected JSON-RPC error during drain: {payload:#}"
        );

        assert!(
            matches!(
                serving.await.unwrap(),
                Served::Signalled {
                    signal: "SIGTERM",
                    drained: true
                }
            ),
            "in-flight /mcp call must drain cleanly on SIGTERM"
        );
        assert!(
            mcp_ct.is_cancelled(),
            "mcp_ct must cancel after drain finishes"
        );
    }

    #[tokio::test]
    async fn a_background_loop_that_panics_is_restarted() {
        use std::sync::atomic::{AtomicUsize, Ordering};

        let shutdown = tokio_util::sync::CancellationToken::new();
        let runs = Arc::new(AtomicUsize::new(0));
        let counted = runs.clone();
        spawn_background_loop(
            "test_loop",
            shutdown.clone(),
            std::time::Duration::from_millis(10),
            move || {
                let runs = counted.clone();
                Box::pin(async move {
                    if runs.fetch_add(1, Ordering::SeqCst) == 0 {
                        panic!("the first run fails");
                    }
                    std::future::pending::<()>().await
                })
            },
        );
        tokio::time::timeout(std::time::Duration::from_secs(5), async {
            while runs.load(Ordering::SeqCst) < 2 {
                tokio::time::sleep(std::time::Duration::from_millis(5)).await;
            }
        })
        .await
        .expect("the loop runs again after a panic");
    }

    #[tokio::test]
    async fn a_background_loop_is_not_restarted_after_shutdown() {
        use std::sync::atomic::{AtomicUsize, Ordering};

        let shutdown = tokio_util::sync::CancellationToken::new();
        let runs = Arc::new(AtomicUsize::new(0));
        let counted = runs.clone();
        spawn_background_loop(
            "test_loop",
            shutdown.clone(),
            std::time::Duration::from_millis(10),
            move || {
                let runs = counted.clone();
                // Each run ends at once, so the loop is restarted until the
                // daemon shuts down.
                Box::pin(async move {
                    runs.fetch_add(1, Ordering::SeqCst);
                })
            },
        );
        tokio::time::timeout(std::time::Duration::from_secs(5), async {
            while runs.load(Ordering::SeqCst) < 3 {
                tokio::time::sleep(std::time::Duration::from_millis(5)).await;
            }
        })
        .await
        .expect("the loop is restarted while the daemon runs");
        shutdown.cancel();
        let at_shutdown = runs.load(Ordering::SeqCst);
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
        let after = runs.load(Ordering::SeqCst);
        assert!(
            after <= at_shutdown + 1,
            "no restart after shutdown: {at_shutdown} at shutdown, {after} after"
        );
        let settled = after;
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        assert_eq!(runs.load(Ordering::SeqCst), settled);
    }

    #[tokio::test]
    async fn voyage_prewarm_disable_flags_do_not_schedule_a_task() {
        use crate::test_support::EnvRestore;
        let _lock = crate::utils::global_test_lock().lock().unwrap();
        let llm = Arc::new(tachi_llm::LlmClient::new().unwrap());
        for (warmup, query_disabled) in [("0", "0"), ("1", "1")] {
            let _warmup = EnvRestore::set("TACHI_VOYAGE_PREWARM", warmup);
            let _disabled = EnvRestore::set("TACHI_SEARCH_DISABLE_QUERY_EMBEDDING", query_disabled);
            assert!(
                spawn_voyage_prewarm(llm.clone(), tokio_util::sync::CancellationToken::new(),)
                    .is_none()
            );
        }
    }

    #[tokio::test]
    #[allow(clippy::await_holding_lock)]
    async fn voyage_prewarm_cancelled_start_does_not_contact_provider() {
        use crate::test_support::EnvRestore;
        let _lock = crate::utils::global_test_lock().lock().unwrap();
        let _warmup = EnvRestore::remove("TACHI_VOYAGE_PREWARM");
        let _disabled = EnvRestore::remove("TACHI_SEARCH_DISABLE_QUERY_EMBEDDING");
        let _key = EnvRestore::set("VOYAGE_API_KEY", "test-prewarm-cancelled-key");
        let _model = EnvRestore::remove("TACHI_EMBEDDING_MODEL");
        let _dim = EnvRestore::remove("TACHI_EMBEDDING_DIM");
        let provider = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        provider.set_nonblocking(true).unwrap();
        let _base = EnvRestore::set(
            "VOYAGE_BASE_URL",
            &format!("http://{}", provider.local_addr().unwrap()),
        );
        let llm = Arc::new(tachi_llm::LlmClient::new().unwrap());
        let cancellation = tokio_util::sync::CancellationToken::new();
        cancellation.cancel();
        spawn_voyage_prewarm(llm, cancellation)
            .unwrap()
            .await
            .unwrap();
        assert_eq!(
            provider.accept().unwrap_err().kind(),
            std::io::ErrorKind::WouldBlock
        );
    }

    #[tokio::test]
    #[allow(clippy::await_holding_lock)]
    async fn voyage_prewarm_background_task_yields_and_cancels_during_stalled_head() {
        use crate::test_support::EnvRestore;
        use tokio::io::AsyncReadExt;
        let _lock = crate::utils::global_test_lock().lock().unwrap();
        let _warmup = EnvRestore::remove("TACHI_VOYAGE_PREWARM");
        let _disabled = EnvRestore::remove("TACHI_SEARCH_DISABLE_QUERY_EMBEDDING");
        let _key = EnvRestore::set("VOYAGE_API_KEY", "test-prewarm-startup-key");
        let _model = EnvRestore::remove("TACHI_EMBEDDING_MODEL");
        let _dim = EnvRestore::remove("TACHI_EMBEDDING_DIM");
        let provider = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let _base = EnvRestore::set(
            "VOYAGE_BASE_URL",
            &format!("http://{}", provider.local_addr().unwrap()),
        );
        let llm = Arc::new(tachi_llm::LlmClient::new().unwrap());
        let ct = tokio_util::sync::CancellationToken::new();
        let task = spawn_voyage_prewarm(llm, ct.clone()).expect("default schedules warmup");
        // Accepting the request on this single-thread runtime proves startup
        // can keep running while warmup waits for the provider's response.
        let (mut socket, _) =
            tokio::time::timeout(std::time::Duration::from_secs(15), provider.accept())
                .await
                .unwrap()
                .unwrap();
        let mut request = [0; 1024];
        let headers = tokio::time::timeout(std::time::Duration::from_secs(15), async {
            let mut headers = Vec::new();
            while !headers.ends_with(b"\r\n\r\n") {
                let n = socket.read(&mut request).await.unwrap();
                assert!(n > 0, "provider must receive complete HEAD headers");
                headers.extend_from_slice(&request[..n]);
                assert!(headers.len() <= 8192, "bounded warmup headers");
            }
            headers
        })
        .await
        .unwrap();
        assert!(headers.starts_with(b"HEAD /v1/embeddings "));
        ct.cancel();
        tokio::time::timeout(std::time::Duration::from_secs(15), task)
            .await
            .unwrap()
            .expect("daemon cancellation joins optional work");
        // Dropping the HEAD future closes this in-flight HTTP/1 connection.
        let n = tokio::time::timeout(
            std::time::Duration::from_secs(15),
            socket.read(&mut request),
        )
        .await
        .unwrap()
        .unwrap();
        assert_eq!(n, 0);
    }

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
        assert!(payload["reconnect"].get("stdio_unaffected").is_none());
        assert_eq!(payload["status"], "ok");
        assert_eq!(payload["transport"], "http");
        assert_eq!(payload["mcp"], "streamable-http");
        assert_eq!(payload["bind"], "127.0.0.1");
        assert_eq!(payload["auth_posture"], "loopback-trust-v1");
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

        let owner = acquire_startup_daemon_lock(&app_home, &global_db).unwrap();
        let result = serve_http_daemon(server, app_home, global_db, None, port, &owner).await;

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
