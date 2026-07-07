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
        daemon: std::sync::Arc::new(std::sync::RwLock::new(info)),
        app_home,
        global_db_path,
        project_db_path,
        client_project,
    };
    let transport = (stdin(), stdout());
    let running = rmcp::service::serve_server(proxy, transport).await?;
    wait_for_stdio_shutdown(running, None).await;
    Ok(())
}

pub(super) async fn serve_stdio(server: MemoryServer) -> Result<(), Box<dyn std::error::Error>> {
    let idle_token = spawn_stdio_idle_reaper(&server);
    let transport = (stdin(), stdout());
    let running = rmcp::service::serve_server(server, transport).await?;
    wait_for_stdio_shutdown(running, idle_token).await;
    Ok(())
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

    fn runtime_info_result(&self) -> rmcp::model::CallToolResult {
        let daemon = self.current_daemon();
        let body = serde_json::json!({
            "mode": "stdio_proxy",
            "process_role": "stdio_proxy",
            "db_handles": 0,
            "stdio_adapter": true,
            "authoritative_runtime": "daemon",
            "transport": {
                "inbound": "stdio",
                "outbound": "streamable_http",
                "target": daemon.url,
            },
            "daemon": {
                "pid": daemon.pid,
                "version": daemon.version,
                "global_db": daemon.global_db,
                "project_db": daemon.project_db,
            },
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
                return Ok(self.runtime_info_result());
            }
            let request = prepare_proxy_tool_call(request, self.client_project.as_deref())?;
            let current = self.current_daemon();
            match crate::cli_client::call_daemon_tool_raw(&current, request.clone()).await {
                Ok(result) => Ok(result),
                // Only BeforeDispatch is safe to retry: the request never reached
                // the daemon, so a re-resolved retry cannot duplicate a write.
                // AfterDispatch (timeout / post-handshake failure) must surface
                // as-is to avoid replaying a possibly-applied write.
                Err(err) if err.allows_in_process_fallback() => {
                    match self.refresh_daemon(&current.url).await {
                        Some(fresh) => crate::cli_client::call_daemon_tool_raw(&fresh, request)
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
        enforce_client_project(request.name.as_ref(), &mut request.arguments, project)?;
    }
    Ok(request)
}

fn enforce_client_project(
    tool_name: &str,
    arguments: &mut Option<rmcp::model::JsonObject>,
    project: &str,
) -> Result<(), rmcp::ErrorData> {
    let args = arguments.get_or_insert_with(serde_json::Map::new);
    if let Some(explicit_project) = args.get("project") {
        if explicit_project.as_str() == Some(project) {
            return Ok(());
        }
        // #733 write isolation: reject routing into another project's DB. Cross-library
        // reads are safe (daemon opens read-only stores) and are allow-listed below (#737).
        if explicit_project.as_str().is_some()
            && explicit_project_can_cross_binding(tool_name, args)
        {
            return Ok(());
        }
        return Err(rmcp::ErrorData::invalid_params(
            format!(
                "stdio proxy project binding mismatch: session is bound to '{project}', but tool call requested project={explicit_project}"
            ),
            None,
        ));
    }
    if !project_defaults_to_bound_project(tool_name, args) {
        return Ok(());
    }
    if tool_name == "tachi_memory"
        && args
            .get("action")
            .and_then(|value| value.as_str())
            .map(|action| !tachi_memory_action_defaults_to_project(action))
            .unwrap_or(false)
    {
        return Ok(());
    }
    if args
        .get("scope")
        .and_then(|value| value.as_str())
        .is_some_and(|scope| scope.eq_ignore_ascii_case("global"))
    {
        return Ok(());
    }
    args.insert("project".to_string(), serde_json::json!(project));
    Ok(())
}

/// Returns true when an explicit `project` param may differ from the stdio
/// session binding. Protects #733 write isolation only: daemon-side reads use
/// read-only opens and do not threaten single-writer discipline (#520).
fn explicit_project_can_cross_binding(tool_name: &str, args: &rmcp::model::JsonObject) -> bool {
    match tool_name {
        "search_memory"
        | "find_similar_memory"
        | "get_memory"
        | "list_memories"
        | "memory_graph"
        | "get_edges"
        | "tachi_search" => true,
        "tachi_memory" => args
            .get("action")
            .and_then(|value| value.as_str())
            .is_some_and(tachi_memory_action_allows_cross_project_read),
        "tachi_wiki" => args
            .get("action")
            .and_then(|value| value.as_str())
            .is_some_and(tachi_wiki_action_allows_cross_project_read),
        "tachi_event" => args
            .get("action")
            .and_then(|value| value.as_str())
            .is_some_and(tachi_event_action_allows_cross_project_read),
        _ => false,
    }
}

fn project_defaults_to_bound_project(tool_name: &str, args: &rmcp::model::JsonObject) -> bool {
    if tool_name == "tachi_memory" {
        return args
            .get("action")
            .and_then(|value| value.as_str())
            .is_none_or(tachi_memory_action_defaults_to_project);
    }
    matches!(
        tool_name,
        "search_memory"
            | "find_similar_memory"
            | "get_memory"
            | "list_memories"
            | "delete_memory"
            | "archive_memory"
            | "save_memory"
            | "remember"
            | "ingest"
            | "ingest_event"
            | "ingest_source"
            | "extract_facts"
            | "tachi_search"
            | "tachi_save"
            | "tachi_event"
            | "tachi_domain_adapter"
            | "tachi_task"
            | "tachi_verify"
            | "tachi_gh"
            | "tachi_wiki"
            | "wiki_write"
            | "tachi_wiki_write"
    ) || tool_name == "tachi_memory"
}

fn tachi_memory_action_defaults_to_project(action: &str) -> bool {
    matches!(
        action.to_ascii_lowercase().as_str(),
        "alerts"
            | "ask"
            | "briefing"
            | "checkpoint"
            | "consolidate"
            | "extract_facts"
            | "get"
            | "apply_recall_proposals"
            | "pattern_feedback"
            | "progress"
            | "readiness"
            | "recall_proposals"
            | "recall_simulate"
            | "review_recall_proposal"
            | "save"
            | "search"
    )
}

/// Read-only or dry-run `tachi_memory` actions. `consolidate` returns dry_run
/// candidates; `recall_simulate` replays search without persisting writes.
fn tachi_memory_action_allows_cross_project_read(action: &str) -> bool {
    matches!(
        action.to_ascii_lowercase().as_str(),
        "alerts"
            | "ask"
            | "briefing"
            | "consolidate"
            | "get"
            | "readiness"
            | "recall_simulate"
            | "search"
    )
}

fn tachi_event_action_allows_cross_project_read(action: &str) -> bool {
    matches!(action.to_ascii_lowercase().as_str(), "metrics" | "query")
}

fn tachi_wiki_action_allows_cross_project_read(action: &str) -> bool {
    matches!(
        action.to_ascii_lowercase().as_str(),
        "browse" | "read" | "search"
    )
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
