use super::*;
use std::ffi::OsString;
use std::future::Future;
use std::path::Path;
use tokio::io::{stdin, stdout};

pub(super) async fn ensure_stdio_proxy_daemon(
    app_home: &Path,
    global_db_path: &Path,
    project_db_path: Option<&Path>,
    client_project: Option<&str>,
) -> Option<crate::cli_client::DaemonInfo> {
    let auto_daemon_disabled = auto_daemon_disabled();
    if stdio_proxy_disabled() {
        eprintln!("[stdio-proxy] disabled by TACHI_DISABLE_STDIO_PROXY");
        return None;
    }

    if !auto_daemon_disabled {
        replace_stale_daemon_if_needed(app_home, global_db_path).await;
    }

    match compatible_daemon(app_home, global_db_path, project_db_path, client_project).await {
        DaemonCompatibility::Compatible(info) => return Some(info),
        DaemonCompatibility::Incompatible => return None,
        DaemonCompatibility::Missing => {}
    }

    if auto_daemon_disabled {
        eprintln!("[auto-daemon] disabled by TACHI_DISABLE_AUTO_DAEMON");
        return None;
    }

    spawn_stdio_daemon(app_home, global_db_path, project_db_path, client_project).await;
    match compatible_daemon(app_home, global_db_path, project_db_path, client_project).await {
        DaemonCompatibility::Compatible(info) => Some(info),
        DaemonCompatibility::Missing | DaemonCompatibility::Incompatible => None,
    }
}

pub(super) fn proxy_can_preserve_project_context(
    info: &crate::cli_client::DaemonInfo,
    global_db_path: &Path,
    project_db_path: Option<&Path>,
    client_project: Option<&str>,
) -> bool {
    if crate::cli_client::daemon_matches_requested_dbs(info, global_db_path, project_db_path) {
        if let (Some(project_db_path), Some(_)) = (project_db_path, client_project) {
            return valid_proxy_project_binding(project_db_path, client_project);
        }
        return true;
    }
    if !crate::cli_client::daemon_global_db_matches(info, global_db_path) {
        return false;
    }
    match project_db_path {
        None => false,
        Some(project_db_path) => valid_proxy_project_binding(project_db_path, client_project),
    }
}

fn valid_proxy_project_binding(project_db_path: &Path, client_project: Option<&str>) -> bool {
    let Some(project) = client_project else {
        return false;
    };
    let Ok(bound_path) = crate::MemoryServer::resolve_named_project_db_path(project) else {
        return false;
    };
    paths_match(project_db_path, &bound_path)
}

fn paths_match(left: &Path, right: &Path) -> bool {
    if left.as_os_str() == right.as_os_str() {
        return true;
    }
    std::fs::canonicalize(left)
        .ok()
        .zip(std::fs::canonicalize(right).ok())
        .map(|(left, right)| left == right)
        .unwrap_or(false)
}

pub(super) async fn serve_stdio_proxy(
    info: crate::cli_client::DaemonInfo,
    app_home: PathBuf,
    global_db_path: PathBuf,
    project_db_path: Option<PathBuf>,
    client_project: Option<String>,
) -> Result<(), Box<dyn std::error::Error>> {
    let proxy = StdioProxyServer {
        adapter_started_at: chrono::Utc::now(),
        daemon: std::sync::Arc::new(std::sync::RwLock::new(info)),
        app_home,
        global_db_path,
        project_db_path,
        client_project,
    };
    let transport = (stdin(), stdout());
    let running = rmcp::service::serve_server(proxy, transport).await?;
    wait_for_stdio_shutdown(running, None).await;
    // #1273 Gap 1: a proxy adapter holds no DB state to flush, so it is safe
    // (and required — see `stdio_hard_exit`'s doc comment) to force-exit
    // immediately once `wait_for_stdio_shutdown` decides it is time to stop.
    stdio_hard_exit();
}

pub(super) async fn serve_stdio(server: MemoryServer) -> Result<(), Box<dyn std::error::Error>> {
    let idle_token = spawn_stdio_idle_reaper(&server);
    let transport = (stdin(), stdout());
    let running = rmcp::service::serve_server(server, transport).await?;
    wait_for_stdio_shutdown(running, idle_token).await;
    Ok(())
}

/// #1273 Gap 1 — root cause of "SIGTERM ignored" zombies.
///
/// `wait_for_stdio_shutdown`'s `sigterm()`/`ctrl_c()`/`wait_for_parent_death()`
/// arms all fire correctly — the eprintln for each DOES run on a real signal.
/// The zombies survive anyway because of what happens *after* that: this
/// process's stdio transport is backed by `tokio::io::stdin()`, which — on
/// every platform, because a raw OS stdin fd cannot be portably driven by an
/// async reactor — services its reads via ONE persistent background thread
/// in tokio's blocking-task pool that performs a genuine synchronous
/// `read(2)` in a loop. That thread has no cancellation hook: dropping the
/// async-side future that was awaiting its result does not interrupt the
/// blocking syscall underneath. In the exact leak mode #1273 measured (a
/// live MCP host that finished a sub-session but never closed this specific
/// stdio pipe), that `read(2)` never returns — no more data, no EOF, forever.
///
/// `#[tokio::main]` builds its `Runtime` inline and drops it when the
/// wrapped async fn returns; `Runtime::drop` performs a BLOCKING shutdown
/// that waits for every outstanding task — including that un-cancellable
/// blocking-pool thread — before the process is allowed to exit. So a plain
/// `return Ok(())` here is a correct in-application decision that the OS
/// process never gets to act on: `main()` never returns, so the process
/// never calls `exit()`, and `kill -TERM` looks identical to "ignored" from
/// outside even though the signal was received and handled.
///
/// The only way to guarantee termination within a bounded time (the
/// contract #1273 asks for) is to bypass `Runtime::drop` entirely with an
/// explicit process exit once we have already decided to stop and (for the
/// direct, DB-holding path) already flushed/joined the background tasks that
/// matter — see the `!cli.daemon` tail of `start_server_transport` in
/// `serve.rs`, which calls this same function after that join completes.
pub(super) fn stdio_hard_exit() -> ! {
    std::process::exit(0)
}

async fn wait_for_stdio_shutdown(
    running: rmcp::service::RunningService<
        rmcp::service::RoleServer,
        impl rmcp::Service<rmcp::service::RoleServer>,
    >,
    idle_token: Option<tokio_util::sync::CancellationToken>,
) {
    tokio::select! {
        quit_reason = running.waiting() => {
            eprintln!("Memory MCP Server stopped: {:?}", quit_reason);
        }
        _ = tokio::signal::ctrl_c() => {
            eprintln!("Received SIGINT, shutting down gracefully...");
        }
        _ = sigterm() => {
            eprintln!("Received SIGTERM, shutting down gracefully...");
        }
        _ = wait_for_parent_death() => {
            eprintln!("[parent-death] host process exited; shutting down orphaned stdio server");
        }
        _ = async {
            match idle_token {
                Some(token) => token.cancelled().await,
                None => std::future::pending::<()>().await,
            }
        } => {
            eprintln!("[stdio-idle] idle timeout reached; shutting down abandoned stdio server");
        }
    }
}

fn spawn_stdio_idle_reaper(server: &MemoryServer) -> Option<tokio_util::sync::CancellationToken> {
    let timeout = stdio_idle_timeout()?;
    let token = tokio_util::sync::CancellationToken::new();
    let clock = server.activity_clock();
    let cancel = token.clone();
    let tick = Duration::from_secs(timeout.as_secs().clamp(60, 300));
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(tick);
        interval.tick().await;
        loop {
            interval.tick().await;
            let last = clock.load(std::sync::atomic::Ordering::Relaxed);
            let idle_ms = chrono::Utc::now().timestamp_millis() - last;
            if idle_ms >= timeout.as_millis() as i64 {
                eprintln!(
                    "[stdio-idle] no MCP activity for {}s (limit {}s); shutting down",
                    idle_ms / 1000,
                    timeout.as_secs()
                );
                cancel.cancel();
                return;
            }
        }
    });
    Some(token)
}

fn auto_daemon_disabled() -> bool {
    std::env::var("TACHI_DISABLE_AUTO_DAEMON")
        .map(|value| env_truthy(&value))
        .unwrap_or(false)
}

pub(super) fn stdio_proxy_disabled() -> bool {
    std::env::var("TACHI_DISABLE_STDIO_PROXY")
        .map(|value| env_truthy(&value))
        .unwrap_or(false)
}

fn env_truthy(value: &str) -> bool {
    let value = value.trim();
    value == "1" || value.eq_ignore_ascii_case("true") || value.eq_ignore_ascii_case("yes")
}

enum DaemonCompatibility {
    Compatible(crate::cli_client::DaemonInfo),
    Missing,
    Incompatible,
}

async fn compatible_daemon(
    app_home: &Path,
    global_db_path: &Path,
    project_db_path: Option<&Path>,
    client_project: Option<&str>,
) -> DaemonCompatibility {
    let Some(info) = crate::cli_client::detect_daemon_for_global_db(app_home, global_db_path).await
    else {
        return DaemonCompatibility::Missing;
    };
    if crate::cli_client::daemon_version_matches(&info) {
        if proxy_can_preserve_project_context(
            &info,
            global_db_path,
            project_db_path,
            client_project,
        ) {
            DaemonCompatibility::Compatible(info)
        } else {
            eprintln!(
                "[stdio-proxy] daemon DB scope mismatch (daemon global={}, project={}; requested global={}, project={}); using local fallback",
                info.global_db.as_deref().unwrap_or("<unknown>"),
                info.project_db.as_deref().unwrap_or("<none>"),
                global_db_path.display(),
                project_db_path
                    .map(|path| path.display().to_string())
                    .unwrap_or_else(|| "<none>".to_string())
            );
            DaemonCompatibility::Incompatible
        }
    } else {
        eprintln!(
            "[stdio-proxy] daemon version mismatch (daemon {:?}, binary {}); using local fallback",
            info.version,
            env!("CARGO_PKG_VERSION")
        );
        DaemonCompatibility::Incompatible
    }
}

async fn replace_stale_daemon_if_needed(app_home: &Path, global_db_path: &Path) {
    // Version-skew replace: if a daemon is already running but STRICTLY
    // OLDER than this binary, a new-binary child cannot proxy to it
    // safely. Replacing it keeps a single current-version writer.
    let Some(info) = crate::cli_client::detect_daemon_for_global_db(app_home, global_db_path).await
    else {
        return;
    };
    if !crate::cli_client::daemon_is_older_than_current(&info) {
        return;
    }
    let Some(pid) = info.pid else {
        return;
    };
    if trading_hours_kill_guard_enabled() && is_within_trading_hours() {
        eprintln!(
            "[auto-daemon] stale daemon pid={pid} v{} detected but trading hours guard active; \
             deferring replacement to off-hours (after 15:30 Asia/Shanghai)",
            info.version.as_deref().unwrap_or("?"),
        );
        return;
    }
    eprintln!(
        "[auto-daemon] replacing stale daemon pid={pid} v{} (< v{})",
        info.version.as_deref().unwrap_or("?"),
        env!("CARGO_PKG_VERSION")
    );
    #[cfg(unix)]
    {
        // SAFETY: `kill(pid, SIGTERM)` only sends a signal to an OS pid; it
        // passes no pointers across the FFI boundary and aliases no Rust
        // memory. `pid` is an integer read from the stale daemon lock above;
        // an invalid/stale pid yields ESRCH, which the loop below tolerates.
        unsafe { libc::kill(pid as libc::pid_t, libc::SIGTERM) };
        for _ in 0..30 {
            if !crate::daemon_lock::process_alive(pid as i32) {
                break;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    }
}

async fn spawn_stdio_daemon(
    app_home: &Path,
    global_db_path: &Path,
    project_db_path: Option<&Path>,
    client_project: Option<&str>,
) {
    match std::env::current_exe() {
        Ok(exe) => {
            let port_str = "0".to_string();
            let daemon_args = auto_daemon_command_args(global_db_path, project_db_path, &port_str);
            match std::process::Command::new(&exe)
                .args(daemon_args)
                .stdin(std::process::Stdio::null())
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .spawn()
            {
                Ok(mut child) => {
                    eprintln!("[auto-daemon] spawned tachi daemon (pid={})", child.id());
                    tokio::spawn(async move {
                        let _ = child.wait();
                    });
                    wait_for_daemon_ready(
                        app_home,
                        global_db_path,
                        project_db_path,
                        client_project,
                    )
                    .await;
                }
                Err(e) => eprintln!("[auto-daemon] failed to spawn daemon: {e}"),
            }
        }
        Err(e) => eprintln!("[auto-daemon] cannot determine binary path: {e}"),
    }
}

async fn wait_for_daemon_ready(
    app_home: &Path,
    global_db_path: &Path,
    project_db_path: Option<&Path>,
    client_project: Option<&str>,
) {
    let ready = tokio::time::timeout(Duration::from_secs(5), async {
        for _ in 0..25 {
            tokio::time::sleep(Duration::from_millis(200)).await;
            match compatible_daemon(app_home, global_db_path, project_db_path, client_project).await
            {
                DaemonCompatibility::Compatible(_) => return true,
                DaemonCompatibility::Incompatible => return false,
                DaemonCompatibility::Missing => {}
            }
        }
        false
    })
    .await
    .unwrap_or(false);
    if !ready {
        tracing::warn!("[auto-daemon] daemon did not become ready within 5s");
    }
}

#[derive(Clone)]
struct StdioProxyServer {
    // Stamped once at adapter-process construction (`serve_stdio_proxy`); this
    // is the adapter's own lifetime anchor, reported verbatim in
    // `runtime_info` as `adapter_started_at` — distinct from
    // `daemon_identity_as_of` (tachi#1222), which is re-derived on every call.
    adapter_started_at: chrono::DateTime<chrono::Utc>,
    // Shared + refreshable so a daemon restart (new ephemeral port) or death is
    // self-healed at call time instead of stranding the adapter on a dead URL.
    daemon: std::sync::Arc<std::sync::RwLock<crate::cli_client::DaemonInfo>>,
    app_home: PathBuf,
    global_db_path: PathBuf,
    project_db_path: Option<PathBuf>,
    client_project: Option<String>,
}

impl StdioProxyServer {
    /// Snapshot the currently-targeted daemon. Cheap clone out of the lock so the
    /// guard is never held across an await.
    fn current_daemon(&self) -> crate::cli_client::DaemonInfo {
        self.daemon
            .read()
            .expect("stdio proxy daemon lock poisoned")
            .clone()
    }

    /// Re-resolve the daemon after a BeforeDispatch (transport) failure. The
    /// request never reached the daemon, so it may have restarted on a new
    /// ephemeral port or died. Reuse the EXACT startup path
    /// (`ensure_stdio_proxy_daemon`: version-compatible discovery +
    /// stale-replace + auto-spawn) so a self-healed daemon is never one startup
    /// would have rejected, then apply startup's project-context gate so a
    /// project-scoped request is never rerouted to a daemon that can't preserve
    /// this project. Persist the fresh endpoint so later calls skip the dead URL.
    /// Returns None when nothing compatible is reachable, so the caller surfaces
    /// the original error.
    async fn refresh_daemon(&self, stale_url: &str) -> Option<crate::cli_client::DaemonInfo> {
        let fresh = ensure_stdio_proxy_daemon(
            &self.app_home,
            &self.global_db_path,
            self.project_db_path.as_deref(),
            self.client_project.as_deref(),
        )
        .await?;
        if !proxy_can_preserve_project_context(
            &fresh,
            &self.global_db_path,
            self.project_db_path.as_deref(),
            self.client_project.as_deref(),
        ) {
            // A same-global daemon that can't preserve this project would
            // misroute writes; refuse it and surface the original error.
            return None;
        }
        if fresh.url == stale_url {
            // Same endpoint resolved again; retrying it would fail identically.
            return None;
        }
        let mut guard = self.daemon.write().unwrap_or_else(|e| e.into_inner());
        *guard = fresh.clone();
        eprintln!(
            "[stdio-proxy] self-healed daemon endpoint: {stale_url} -> {}",
            fresh.url
        );
        Some(fresh)
    }

    /// Re-derive the daemon identity block fresh on every call (tachi#1222):
    /// no cached-snapshot read, so a daemon restart or death between calls is
    /// reflected immediately instead of echoing adapter-startup state.
    /// Reuses the exact discovery primitive (`detect_daemon_for_global_db`:
    /// pid/lock-file re-read + TCP probe) that `compatible_daemon` /
    /// `refresh_daemon` already use to decide routing, so "what runtime_info
    /// reports" and "what the proxy would route to" can never silently
    /// diverge onto two different detection mechanisms.
    ///
    /// When no daemon answers for this global DB, this reports `reachable:
    /// false` and null identity fields rather than falling back to the last
    /// cached snapshot — a stale-but-plausible-looking identity is worse than
    /// an explicit "don't know" (dispatch-lifecycle "拒必有声").
    async fn runtime_info_result(&self) -> rmcp::model::CallToolResult {
        let adapter_started_at = self.adapter_started_at.to_rfc3339();
        let fresh =
            crate::cli_client::detect_daemon_for_global_db(&self.app_home, &self.global_db_path)
                .await;
        let daemon_identity_as_of = chrono::Utc::now().to_rfc3339();

        let (transport_target, daemon_block) = match fresh {
            Some(daemon) => (
                serde_json::Value::String(daemon.url),
                serde_json::json!({
                    "reachable": true,
                    "pid": daemon.pid,
                    "version": daemon.version,
                    "global_db": daemon.global_db,
                    "project_db": daemon.project_db,
                }),
            ),
            None => (
                serde_json::Value::Null,
                serde_json::json!({
                    "reachable": false,
                    "pid": null,
                    "version": null,
                    "global_db": null,
                    "project_db": null,
                }),
            ),
        };

        let body = serde_json::json!({
            "mode": "stdio_proxy",
            "process_role": "stdio_proxy",
            "db_handles": 0,
            "stdio_adapter": true,
            "authoritative_runtime": "daemon",
            "adapter_started_at": adapter_started_at,
            "daemon_identity_as_of": daemon_identity_as_of,
            "transport": {
                "inbound": "stdio",
                "outbound": "streamable_http",
                "target": transport_target,
            },
            "daemon": daemon_block,
            "client": {
                "global_db": self.global_db_path.display().to_string(),
                "project_db": self.project_db_path.as_ref().map(|path| path.display().to_string()),
                "project": self.client_project,
            }
        });
        rmcp::model::CallToolResult::success(vec![rmcp::model::Content::text(
            serde_json::to_string(&body).unwrap_or_else(|_| "{}".to_string()),
        )])
    }
}

impl rmcp::ServerHandler for StdioProxyServer {
    fn get_info(&self) -> rmcp::model::ServerInfo {
        rmcp::model::ServerInfo::new(
            rmcp::model::ServerCapabilities::builder()
                .enable_tools()
                .build(),
        )
        .with_instructions(crate::server_instructions::mcp_server_instructions())
    }

    fn list_tools(
        &self,
        request: Option<rmcp::model::PaginatedRequestParams>,
        _context: rmcp::service::RequestContext<rmcp::service::RoleServer>,
    ) -> impl Future<Output = Result<rmcp::model::ListToolsResult, rmcp::ErrorData>> + Send + '_
    {
        async move {
            let current = self.current_daemon();
            match crate::cli_client::list_daemon_tools(&current, request.clone()).await {
                Ok(result) => Ok(result),
                // BeforeDispatch = the request never reached the daemon; safe to
                // re-resolve and retry (list_tools is read-only regardless).
                Err(err) if err.allows_in_process_fallback() => {
                    match self.refresh_daemon(&current.url).await {
                        Some(fresh) => crate::cli_client::list_daemon_tools(&fresh, request)
                            .await
                            .map_err(daemon_error_data),
                        None => Err(daemon_error_data(err)),
                    }
                }
                Err(err) => Err(daemon_error_data(err)),
            }
        }
    }

    fn call_tool(
        &self,
        request: rmcp::model::CallToolRequestParams,
        _context: rmcp::service::RequestContext<rmcp::service::RoleServer>,
    ) -> impl Future<Output = Result<rmcp::model::CallToolResult, rmcp::ErrorData>> + Send + '_
    {
        async move {
            if request.name.as_ref() == "runtime_info" {
                return Ok(self.runtime_info_result().await);
            }
            let request = prepare_proxy_tool_call(request, self.client_project.as_deref())?;
            let current = self.current_daemon();
            match crate::cli_client::call_daemon_tool_raw(
                &current,
                request.clone(),
                self.client_project.as_deref(),
            )
            .await
            {
                Ok(result) => Ok(result),
                // Only BeforeDispatch is safe to retry: the request never reached
                // the daemon, so a re-resolved retry cannot duplicate a write.
                // AfterDispatch (timeout / post-handshake failure) must surface
                // as-is to avoid replaying a possibly-applied write.
                Err(err) if err.allows_in_process_fallback() => {
                    match self.refresh_daemon(&current.url).await {
                        Some(fresh) => crate::cli_client::call_daemon_tool_raw(
                            &fresh,
                            request,
                            self.client_project.as_deref(),
                        )
                        .await
                        .map_err(daemon_error_data),
                        None => Err(daemon_error_data(err)),
                    }
                }
                Err(err) => Err(daemon_error_data(err)),
            }
        }
    }
}

fn daemon_error_data(error: crate::cli_client::DaemonCallError) -> rmcp::ErrorData {
    rmcp::ErrorData::internal_error(format!("stdio proxy daemon call failed: {error}"), None)
}

fn prepare_proxy_tool_call(
    mut request: rmcp::model::CallToolRequestParams,
    client_project: Option<&str>,
) -> Result<rmcp::model::CallToolRequestParams, rmcp::ErrorData> {
    if request.name.as_ref() == "tachi_briefing" {
        let mut args = serde_json::Map::new();
        args.insert("action".to_string(), serde_json::json!("briefing"));
        args.insert("format".to_string(), serde_json::json!("markdown"));
        args.insert("compact".to_string(), serde_json::json!(true));
        if let Some(project) = client_project {
            args.insert("project".to_string(), serde_json::json!(project));
            args.insert(
                "query".to_string(),
                serde_json::json!(format!(
                    "{project} current task recent decisions blockers next steps"
                )),
            );
        }
        request.name = "tachi_memory".into();
        request.arguments = Some(args);
        return Ok(request);
    }

    if let Some(project) = client_project {
        crate::session_identity::enforce_session_project(
            request.name.as_ref(),
            &mut request.arguments,
            project,
            "stdio proxy",
            // #1041 B1: this is a Preflight hop only — it validates/rejects
            // early (before the HTTP round-trip to the daemon) but never
            // injects a default `project=`. The daemon's own call_tool
            // (Authoritative) is the sole place that happens, so there is
            // never a wire-forgeable "was this hop's default" signal to
            // preserve across the stdio-proxy -> daemon-HTTP hop.
            crate::session_identity::EnforcementRole::Preflight,
        )?;
    }
    Ok(request)
}

// HTTP direct-connect parity (#732): keep any future direct-connect guard aligned
// with this stdio read/write split; reads may select readable projects, mutations
// must remain bound unless their specific invariant proves otherwise.

fn auto_daemon_command_args(
    global_db_path: &Path,
    project_db_path: Option<&Path>,
    port: &str,
) -> Vec<OsString> {
    let mut args = vec![
        OsString::from("--daemon"),
        OsString::from("--port"),
        OsString::from(port),
        OsString::from("--global-db"),
        global_db_path.as_os_str().to_owned(),
    ];
    match project_db_path {
        Some(project_db_path) => {
            args.push(OsString::from("--project-db"));
            args.push(project_db_path.as_os_str().to_owned());
        }
        None => args.push(OsString::from("--no-project-db")),
    }
    args
}

#[cfg(test)]
mod tests;
