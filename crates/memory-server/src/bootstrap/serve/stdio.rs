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
mod tests {
    use super::*;
    use std::ffi::OsString;
    use tokio_util::sync::CancellationToken;

    fn daemon(global: Option<&Path>, project: Option<&Path>) -> crate::cli_client::DaemonInfo {
        crate::cli_client::DaemonInfo {
            url: "http://127.0.0.1:6919/mcp".to_string(),
            global_db: global.map(|path| path.display().to_string()),
            project_db: project.map(|path| path.display().to_string()),
            version: Some(env!("CARGO_PKG_VERSION").to_string()),
            pid: Some(std::process::id() as i64),
        }
    }

    fn with_tachi_home<T>(home: &Path, f: impl FnOnce() -> T) -> T {
        let _guard = crate::utils::global_test_lock()
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let saved_home = std::env::var_os("TACHI_HOME");
        let saved_sigil = std::env::var_os("SIGIL_HOME");
        let saved_app = std::env::var_os("TACHI_APP_HOME");
        std::env::set_var("TACHI_HOME", home);
        std::env::remove_var("SIGIL_HOME");
        std::env::remove_var("TACHI_APP_HOME");
        let out = f();
        restore_env("TACHI_HOME", saved_home);
        restore_env("SIGIL_HOME", saved_sigil);
        restore_env("TACHI_APP_HOME", saved_app);
        out
    }

    fn restore_env(name: &str, value: Option<OsString>) {
        if let Some(value) = value {
            std::env::set_var(name, value);
        } else {
            std::env::remove_var(name);
        }
    }

    fn test_runtime() -> tokio::runtime::Runtime {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("test runtime")
    }

    async fn spawn_test_http_daemon(
        server: crate::MemoryServer,
        global_db_path: &Path,
    ) -> (
        crate::cli_client::DaemonInfo,
        CancellationToken,
        tokio::task::JoinHandle<()>,
    ) {
        use rmcp::transport::streamable_http_server::{
            session::local::LocalSessionManager, StreamableHttpServerConfig, StreamableHttpService,
        };

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind daemon listener");
        let local_addr = listener.local_addr().expect("local addr");
        let ct = CancellationToken::new();
        let ct_shutdown = ct.clone();

        let mut http_config = StreamableHttpServerConfig::default();
        http_config.stateful_mode = true;
        http_config.cancellation_token = ct.child_token();

        let service = StreamableHttpService::new(
            move || Ok(server.clone()),
            std::sync::Arc::new(LocalSessionManager::default()),
            http_config,
        );
        let router = axum::Router::new().nest_service("/mcp", service);
        let handle = tokio::spawn(async move {
            let _ = axum::serve(listener, router)
                .with_graceful_shutdown(async move { ct_shutdown.cancelled_owned().await })
                .await;
        });

        (
            crate::cli_client::DaemonInfo {
                url: format!("http://{local_addr}/mcp"),
                global_db: Some(global_db_path.display().to_string()),
                project_db: None,
                version: Some(env!("CARGO_PKG_VERSION").to_string()),
                pid: Some(std::process::id() as i64),
            },
            ct,
            handle,
        )
    }

    async fn call_tool_via_stdio_proxy(
        proxy: StdioProxyServer,
        tool_name: &str,
        arguments: serde_json::Map<String, serde_json::Value>,
    ) -> Result<rmcp::model::CallToolResult, rmcp::ErrorData> {
        let mut params = rmcp::model::CallToolRequestParams::new(tool_name.to_string());
        if !arguments.is_empty() {
            params = params.with_arguments(arguments);
        }
        let request =
            rmcp::model::ClientRequest::CallToolRequest(rmcp::model::CallToolRequest::new(params));
        let (transport, mut receiver) = rmcp::transport::OneshotTransport::<
            rmcp::service::RoleServer,
        >::new(rmcp::model::ClientJsonRpcMessage::request(
            request,
            rmcp::model::RequestId::Number(1),
        ));
        let service = rmcp::service::serve_directly(proxy, transport, None);

        let message = tokio::time::timeout(std::time::Duration::from_secs(5), receiver.recv())
            .await
            .expect("proxied tool call timed out")
            .expect("proxied tool call should yield one response");

        let quit_reason = service.waiting().await.expect("wait for proxy service");
        assert!(
            matches!(quit_reason, rmcp::service::QuitReason::Closed),
            "proxy oneshot service should close cleanly after one tool call"
        );

        match message {
            rmcp::model::ServerJsonRpcMessage::Response(response) => match response.result {
                rmcp::model::ServerResult::CallToolResult(result) => Ok(result),
                other => panic!("expected CallToolResult, got {other:?}"),
            },
            rmcp::model::ServerJsonRpcMessage::Error(error) => Err(error.error),
            other => panic!("expected tool response or error, got {other:?}"),
        }
    }

    fn memory_text_count(db_path: &Path, text: &str) -> i64 {
        rusqlite::Connection::open(db_path)
            .expect("open db")
            .query_row(
                "SELECT COUNT(*) FROM memories WHERE text = ?1",
                [text],
                |row| row.get(0),
            )
            .expect("count memories")
    }

    fn memory_id_count(db_path: &Path, id: &str) -> i64 {
        rusqlite::Connection::open(db_path)
            .expect("open db")
            .query_row("SELECT COUNT(*) FROM memories WHERE id = ?1", [id], |row| {
                row.get(0)
            })
            .expect("count memory id")
    }

    fn memory_archived_value(db_path: &Path, id: &str) -> i64 {
        rusqlite::Connection::open(db_path)
            .expect("open db")
            .query_row("SELECT archived FROM memories WHERE id = ?1", [id], |row| {
                row.get(0)
            })
            .expect("archived value")
    }

    fn first_text(result: &rmcp::model::CallToolResult) -> String {
        result
            .content
            .iter()
            .find_map(|content| match &content.raw {
                rmcp::model::RawContent::Text(text) => Some(text.text.clone()),
                _ => None,
            })
            .expect("text tool result")
    }

    fn first_text_json(result: &rmcp::model::CallToolResult) -> serde_json::Value {
        let text = first_text(result);
        serde_json::from_str(&text)
            .unwrap_or_else(|err| panic!("tool result json: {err}; text={text:?}"))
    }

    fn assert_search_section_rows_are_objects(parsed: &serde_json::Value) {
        let sections = parsed["sections"].as_array().unwrap_or_else(|| {
            panic!("search JSON should contain sections array: {parsed:#}");
        });
        for section in sections {
            let section_name = section["name"].as_str().unwrap_or("<unnamed>");
            let rows = section["rows"].as_array().unwrap_or_else(|| {
                panic!("section {section_name} rows should be an array: {section:#}");
            });
            for row in rows {
                assert!(
                    row.is_object(),
                    "section {section_name} contains non-object row: row={row:#} parsed={parsed:#}"
                );
            }
        }
    }

    fn assert_tool_ok(result: &rmcp::model::CallToolResult) {
        assert!(
            !result.is_error.unwrap_or(false),
            "tool call should not be an MCP error: {result:?}"
        );
    }

    fn seed_project_db(tachi_home: &Path, project_db_path: &Path) {
        let _seed = crate::MemoryServer::new(
            tachi_home.join(format!("seed-{}.db", uuid::Uuid::new_v4())),
            Some(project_db_path.to_path_buf()),
        )
        .expect("seed project db schema");
    }

    #[test]
    fn ensure_stdio_proxy_accepts_global_only_daemon_for_project_client() {
        let _guard = crate::utils::global_test_lock()
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let saved_home = std::env::var_os("TACHI_HOME");
        let saved_sigil = std::env::var_os("SIGIL_HOME");
        let saved_app = std::env::var_os("TACHI_APP_HOME");
        let saved_disable_proxy = std::env::var_os("TACHI_DISABLE_STDIO_PROXY");
        let saved_disable_auto = std::env::var_os("TACHI_DISABLE_AUTO_DAEMON");

        let temp = tempfile::tempdir().expect("tempdir");
        let tachi_home = temp.path().join("home");
        let global = tachi_home.join("global/memory.db");
        let project = tachi_home.join("projects/Sigil-test/memory.db");
        std::fs::create_dir_all(global.parent().expect("global parent")).expect("global parent");
        std::fs::create_dir_all(project.parent().expect("project parent")).expect("project parent");
        std::fs::write(&project, b"").expect("project db placeholder");
        std::env::set_var("TACHI_HOME", &tachi_home);
        std::env::remove_var("SIGIL_HOME");
        std::env::remove_var("TACHI_APP_HOME");
        std::env::remove_var("TACHI_DISABLE_STDIO_PROXY");
        std::env::set_var("TACHI_DISABLE_AUTO_DAEMON", "1");

        let rt = test_runtime();
        let listener = rt
            .block_on(tokio::net::TcpListener::bind("127.0.0.1:0"))
            .expect("listener");
        let port = listener.local_addr().expect("local addr").port();
        let pid_path = crate::daemon_lock::scoped_daemon_pid_path(&tachi_home, &global);
        std::fs::write(
            &pid_path,
            serde_json::json!({
                "pid": std::process::id(),
                "port": port,
                "url": format!("http://127.0.0.1:{port}/mcp"),
                "global_db": global.display().to_string(),
                "project_db": null,
                "version": env!("CARGO_PKG_VERSION"),
            })
            .to_string(),
        )
        .expect("pid file");

        let info = rt
            .block_on(ensure_stdio_proxy_daemon(
                &tachi_home,
                &global,
                Some(&project),
                Some("Sigil-test"),
            ))
            .expect("global-only daemon should accept bound project client");
        assert_eq!(info.project_db, None);
        assert_eq!(info.url, format!("http://127.0.0.1:{port}/mcp"));

        drop(listener);
        restore_env("TACHI_HOME", saved_home);
        restore_env("SIGIL_HOME", saved_sigil);
        restore_env("TACHI_APP_HOME", saved_app);
        restore_env("TACHI_DISABLE_STDIO_PROXY", saved_disable_proxy);
        restore_env("TACHI_DISABLE_AUTO_DAEMON", saved_disable_auto);
    }

    #[test]
    fn stdio_proxy_call_writes_bound_project_via_global_only_daemon() {
        let _guard = crate::utils::global_test_lock()
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let saved_home = std::env::var_os("TACHI_HOME");
        let saved_sigil = std::env::var_os("SIGIL_HOME");
        let saved_app = std::env::var_os("TACHI_APP_HOME");

        let temp = tempfile::tempdir().expect("tempdir");
        let tachi_home = temp.path().join("home");
        let global = tachi_home.join("global/memory.db");
        let project_name = "Sigil-proxy-e2e";
        let project = tachi_home
            .join("projects")
            .join(project_name)
            .join("memory.db");
        std::fs::create_dir_all(global.parent().expect("global parent")).expect("global parent");
        std::env::set_var("TACHI_HOME", &tachi_home);
        std::env::remove_var("SIGIL_HOME");
        std::env::remove_var("TACHI_APP_HOME");
        seed_project_db(&tachi_home, &project);

        let rt = test_runtime();
        let (ct, daemon_task) = rt.block_on(async {
            let server = crate::MemoryServer::new(global.clone(), None).expect("daemon server");
            let (daemon, ct, daemon_task) = spawn_test_http_daemon(server, &global).await;
            let proxy = StdioProxyServer {
                daemon: std::sync::Arc::new(std::sync::RwLock::new(daemon)),
                app_home: tachi_home.clone(),
                global_db_path: global.clone(),
                project_db_path: Some(project.clone()),
                client_project: Some(project_name.to_string()),
            };

            let saved_text = "stdio proxy e2e writes only the bound project db";
            let result = call_tool_via_stdio_proxy(
                proxy,
                "tachi_memory",
                serde_json::Map::from_iter([
                    ("action".to_string(), serde_json::json!("save")),
                    ("text".to_string(), serde_json::json!(saved_text)),
                    ("summary".to_string(), serde_json::json!("stdio proxy e2e")),
                    (
                        "path".to_string(),
                        serde_json::json!("/tests/stdio-proxy-e2e"),
                    ),
                    ("category".to_string(), serde_json::json!("fact")),
                    ("scope".to_string(), serde_json::json!("project")),
                    ("force".to_string(), serde_json::json!(true)),
                ]),
            )
            .await
            .expect("proxied save_memory should succeed");

            assert_tool_ok(&result);
            (ct, daemon_task)
        });

        let saved_text = "stdio proxy e2e writes only the bound project db";
        assert_eq!(memory_text_count(&project, saved_text), 1);
        assert_eq!(memory_text_count(&global, saved_text), 0);

        ct.cancel();
        rt.block_on(daemon_task).expect("daemon task");
        restore_env("TACHI_HOME", saved_home);
        restore_env("SIGIL_HOME", saved_sigil);
        restore_env("TACHI_APP_HOME", saved_app);
    }

    #[test]
    fn stdio_proxy_call_rejects_cross_project_override_before_daemon_write() {
        let _guard = crate::utils::global_test_lock()
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let saved_home = std::env::var_os("TACHI_HOME");
        let saved_sigil = std::env::var_os("SIGIL_HOME");
        let saved_app = std::env::var_os("TACHI_APP_HOME");

        let temp = tempfile::tempdir().expect("tempdir");
        let tachi_home = temp.path().join("home");
        let global = tachi_home.join("global/memory.db");
        let bound_project_name = "Sigil-proxy-e2e";
        let other_project_name = "Quant-proxy-e2e";
        let bound_project = tachi_home
            .join("projects")
            .join(bound_project_name)
            .join("memory.db");
        let other_project = tachi_home
            .join("projects")
            .join(other_project_name)
            .join("memory.db");
        std::fs::create_dir_all(global.parent().expect("global parent")).expect("global parent");
        std::env::set_var("TACHI_HOME", &tachi_home);
        std::env::remove_var("SIGIL_HOME");
        std::env::remove_var("TACHI_APP_HOME");
        seed_project_db(&tachi_home, &bound_project);
        seed_project_db(&tachi_home, &other_project);

        let rt = test_runtime();
        let (ct, daemon_task) = rt.block_on(async {
            let server = crate::MemoryServer::new(global.clone(), None).expect("daemon server");
            let (daemon, ct, daemon_task) = spawn_test_http_daemon(server, &global).await;
            let proxy = StdioProxyServer {
                daemon: std::sync::Arc::new(std::sync::RwLock::new(daemon)),
                app_home: tachi_home.clone(),
                global_db_path: global.clone(),
                project_db_path: Some(bound_project.clone()),
                client_project: Some(bound_project_name.to_string()),
            };

            let rejected_text = "stdio proxy e2e rejects cross-project override";
            let err = call_tool_via_stdio_proxy(
                proxy,
                "tachi_memory",
                serde_json::Map::from_iter([
                    ("action".to_string(), serde_json::json!("save")),
                    ("project".to_string(), serde_json::json!(other_project_name)),
                    ("text".to_string(), serde_json::json!(rejected_text)),
                    (
                        "summary".to_string(),
                        serde_json::json!("stdio proxy e2e reject"),
                    ),
                    (
                        "path".to_string(),
                        serde_json::json!("/tests/stdio-proxy-e2e-reject"),
                    ),
                    ("category".to_string(), serde_json::json!("fact")),
                    ("scope".to_string(), serde_json::json!("project")),
                    ("force".to_string(), serde_json::json!(true)),
                ]),
            )
            .await
            .expect_err("cross-project override should be rejected before dispatch");

            assert!(
                err.message.contains("stdio proxy project binding mismatch"),
                "unexpected error: {err:?}"
            );
            (ct, daemon_task)
        });

        let rejected_text = "stdio proxy e2e rejects cross-project override";
        assert_eq!(memory_text_count(&bound_project, rejected_text), 0);
        assert_eq!(memory_text_count(&other_project, rejected_text), 0);
        assert_eq!(memory_text_count(&global, rejected_text), 0);

        ct.cancel();
        rt.block_on(daemon_task).expect("daemon task");
        restore_env("TACHI_HOME", saved_home);
        restore_env("SIGIL_HOME", saved_sigil);
        restore_env("TACHI_APP_HOME", saved_app);
    }

    #[test]
    fn stdio_proxy_tachi_search_returns_global_and_bound_project_rows() {
        let _guard = crate::utils::global_test_lock()
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let saved_home = std::env::var_os("TACHI_HOME");
        let saved_sigil = std::env::var_os("SIGIL_HOME");
        let saved_app = std::env::var_os("TACHI_APP_HOME");

        let temp = tempfile::tempdir().expect("tempdir");
        let tachi_home = temp.path().join("home");
        let global = tachi_home.join("global/memory.db");
        let project_name = "Sigil-proxy-search-e2e";
        let project = tachi_home
            .join("projects")
            .join(project_name)
            .join("memory.db");
        std::fs::create_dir_all(global.parent().expect("global parent")).expect("global parent");
        std::env::set_var("TACHI_HOME", &tachi_home);
        std::env::remove_var("SIGIL_HOME");
        std::env::remove_var("TACHI_APP_HOME");
        seed_project_db(&tachi_home, &project);

        let rt = test_runtime();
        let (ct, daemon_task) = rt.block_on(async {
            let server = crate::MemoryServer::new(global.clone(), None).expect("daemon server");
            let (daemon, ct, daemon_task) = spawn_test_http_daemon(server, &global).await;
            let proxy = StdioProxyServer {
                daemon: std::sync::Arc::new(std::sync::RwLock::new(daemon)),
                app_home: tachi_home.clone(),
                global_db_path: global.clone(),
                project_db_path: Some(project.clone()),
                client_project: Some(project_name.to_string()),
            };

            for (id, scope, summary) in [
                (
                    "global-proxy-search-e2e",
                    "global",
                    "global PROXYREADMERGE row",
                ),
                (
                    "project-proxy-search-e2e",
                    "project",
                    "project PROXYREADMERGE row",
                ),
            ] {
                let result = call_tool_via_stdio_proxy(
                    proxy.clone(),
                    "tachi_memory",
                    serde_json::Map::from_iter([
                        ("action".to_string(), serde_json::json!("save")),
                        ("id".to_string(), serde_json::json!(id)),
                        (
                            "text".to_string(),
                            serde_json::json!(format!("{summary} lossless text")),
                        ),
                        ("summary".to_string(), serde_json::json!(summary)),
                        (
                            "path".to_string(),
                            serde_json::json!("/tests/stdio-proxy-search-e2e"),
                        ),
                        ("category".to_string(), serde_json::json!("fact")),
                        ("scope".to_string(), serde_json::json!(scope)),
                        ("force".to_string(), serde_json::json!(true)),
                    ]),
                )
                .await
                .unwrap_or_else(|err| panic!("seed {id}: {err}"));
                assert_tool_ok(&result);
            }

            let result = call_tool_via_stdio_proxy(
                proxy,
                "tachi_memory",
                serde_json::Map::from_iter([
                    ("action".to_string(), serde_json::json!("search")),
                    ("query".to_string(), serde_json::json!("PROXYREADMERGE")),
                    ("scope".to_string(), serde_json::json!("memory")),
                    ("top_k".to_string(), serde_json::json!(10)),
                    ("format".to_string(), serde_json::json!("json")),
                ]),
            )
            .await
            .expect("proxied tachi_memory search should succeed");
            assert_tool_ok(&result);
            let text = first_text(&result);
            assert!(
                text.contains("global-proxy-search-e2e"),
                "proxied search lost global row: {text}"
            );
            assert!(
                text.contains("project-proxy-search-e2e"),
                "proxied search lost bound project row: {text}"
            );

            (ct, daemon_task)
        });

        ct.cancel();
        rt.block_on(daemon_task).expect("daemon task");
        restore_env("TACHI_HOME", saved_home);
        restore_env("SIGIL_HOME", saved_sigil);
        restore_env("TACHI_APP_HOME", saved_app);
    }

    #[test]
    fn stdio_proxy_allows_explicit_cross_project_read() {
        let _guard = crate::utils::global_test_lock()
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let saved_home = std::env::var_os("TACHI_HOME");
        let saved_sigil = std::env::var_os("SIGIL_HOME");
        let saved_app = std::env::var_os("TACHI_APP_HOME");

        let temp = tempfile::tempdir().expect("tempdir");
        let tachi_home = temp.path().join("home");
        let global = tachi_home.join("global/memory.db");
        let bound_project_name = "Sigil-proxy-cross-read-e2e";
        let other_project_name = "Quant-proxy-cross-read-e2e";
        let bound_project = tachi_home
            .join("projects")
            .join(bound_project_name)
            .join("memory.db");
        let other_project = tachi_home
            .join("projects")
            .join(other_project_name)
            .join("memory.db");
        std::fs::create_dir_all(global.parent().expect("global parent")).expect("global parent");
        std::env::set_var("TACHI_HOME", &tachi_home);
        std::env::remove_var("SIGIL_HOME");
        std::env::remove_var("TACHI_APP_HOME");
        seed_project_db(&tachi_home, &bound_project);
        seed_project_db(&tachi_home, &other_project);

        let rt = test_runtime();
        let (ct, daemon_task) = rt.block_on(async {
            let server = crate::MemoryServer::new(global.clone(), None).expect("daemon server");
            let (daemon, ct, daemon_task) = spawn_test_http_daemon(server, &global).await;
            let daemon = std::sync::Arc::new(std::sync::RwLock::new(daemon));
            let bound_proxy = StdioProxyServer {
                daemon: daemon.clone(),
                app_home: tachi_home.clone(),
                global_db_path: global.clone(),
                project_db_path: Some(bound_project.clone()),
                client_project: Some(bound_project_name.to_string()),
            };
            let other_proxy = StdioProxyServer {
                daemon,
                app_home: tachi_home.clone(),
                global_db_path: global.clone(),
                project_db_path: Some(other_project.clone()),
                client_project: Some(other_project_name.to_string()),
            };

            let result = call_tool_via_stdio_proxy(
                other_proxy,
                "tachi_memory",
                serde_json::Map::from_iter([
                    ("action".to_string(), serde_json::json!("save")),
                    (
                        "id".to_string(),
                        serde_json::json!("other-proxy-cross-read-e2e"),
                    ),
                    (
                        "text".to_string(),
                        serde_json::json!("other PROXYCROSSREAD row"),
                    ),
                    (
                        "summary".to_string(),
                        serde_json::json!("other PROXYCROSSREAD row"),
                    ),
                    (
                        "path".to_string(),
                        serde_json::json!("/tests/stdio-proxy-cross-read-e2e"),
                    ),
                    ("category".to_string(), serde_json::json!("fact")),
                    ("scope".to_string(), serde_json::json!("project")),
                    ("force".to_string(), serde_json::json!(true)),
                ]),
            )
            .await
            .expect("seed other project through proxy");
            assert_tool_ok(&result);

            let result = call_tool_via_stdio_proxy(
                bound_proxy,
                "tachi_memory",
                serde_json::Map::from_iter([
                    ("action".to_string(), serde_json::json!("search")),
                    ("project".to_string(), serde_json::json!(other_project_name)),
                    ("query".to_string(), serde_json::json!("PROXYCROSSREAD")),
                    ("scope".to_string(), serde_json::json!("memory")),
                    ("top_k".to_string(), serde_json::json!(10)),
                    ("format".to_string(), serde_json::json!("json")),
                ]),
            )
            .await
            .expect("explicit cross-project read should be forwarded");
            assert_tool_ok(&result);
            let parsed = first_text_json(&result);
            assert_search_section_rows_are_objects(&parsed);
            let text = parsed.to_string();
            assert!(
                text.contains("other-proxy-cross-read-e2e"),
                "proxied explicit project read lost other project row: {parsed:#}"
            );

            (ct, daemon_task)
        });

        ct.cancel();
        rt.block_on(daemon_task).expect("daemon task");
        restore_env("TACHI_HOME", saved_home);
        restore_env("SIGIL_HOME", saved_sigil);
        restore_env("TACHI_APP_HOME", saved_app);
    }

    #[test]
    fn stdio_proxy_tachi_memory_search_rows_stay_objects_under_parallel_forwarding() {
        let _guard = crate::utils::global_test_lock()
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let saved_home = std::env::var_os("TACHI_HOME");
        let saved_sigil = std::env::var_os("SIGIL_HOME");
        let saved_app = std::env::var_os("TACHI_APP_HOME");

        let temp = tempfile::tempdir().expect("tempdir");
        let tachi_home = temp.path().join("home");
        let global = tachi_home.join("global/memory.db");
        let project_name = "Sigil-proxy-row-shape-e2e";
        let project = tachi_home
            .join("projects")
            .join(project_name)
            .join("memory.db");
        std::fs::create_dir_all(global.parent().expect("global parent")).expect("global parent");
        std::env::set_var("TACHI_HOME", &tachi_home);
        std::env::remove_var("SIGIL_HOME");
        std::env::remove_var("TACHI_APP_HOME");
        seed_project_db(&tachi_home, &project);

        let rt = test_runtime();
        let (ct, daemon_task) = rt.block_on(async {
            let server = crate::MemoryServer::new(global.clone(), None).expect("daemon server");
            let (daemon, ct, daemon_task) = spawn_test_http_daemon(server, &global).await;
            let proxy = StdioProxyServer {
                daemon: std::sync::Arc::new(std::sync::RwLock::new(daemon)),
                app_home: tachi_home.clone(),
                global_db_path: global.clone(),
                project_db_path: Some(project.clone()),
                client_project: Some(project_name.to_string()),
            };

            for (id, scope, summary) in [
                (
                    "global-proxy-row-shape-e2e",
                    "global",
                    "global PROXYROWSHAPE row",
                ),
                (
                    "project-proxy-row-shape-e2e",
                    "project",
                    "project PROXYROWSHAPE row",
                ),
            ] {
                let result = call_tool_via_stdio_proxy(
                    proxy.clone(),
                    "tachi_memory",
                    serde_json::Map::from_iter([
                        ("action".to_string(), serde_json::json!("save")),
                        ("id".to_string(), serde_json::json!(id)),
                        (
                            "text".to_string(),
                            serde_json::json!(format!("{summary} lossless text")),
                        ),
                        ("summary".to_string(), serde_json::json!(summary)),
                        (
                            "path".to_string(),
                            serde_json::json!("/tests/stdio-proxy-row-shape-e2e"),
                        ),
                        ("category".to_string(), serde_json::json!("fact")),
                        ("scope".to_string(), serde_json::json!(scope)),
                        ("force".to_string(), serde_json::json!(true)),
                    ]),
                )
                .await
                .unwrap_or_else(|err| panic!("seed {id}: {err}"));
                assert_tool_ok(&result);
            }

            let search_args = |i: usize| {
                serde_json::Map::from_iter([
                    ("action".to_string(), serde_json::json!("search")),
                    ("query".to_string(), serde_json::json!("PROXYROWSHAPE")),
                    ("scope".to_string(), serde_json::json!("memory")),
                    ("top_k".to_string(), serde_json::json!(10 + i)),
                    ("format".to_string(), serde_json::json!("json")),
                ])
            };
            let calls = (0..32)
                .map(|i| call_tool_via_stdio_proxy(proxy.clone(), "tachi_memory", search_args(i)));
            let results = futures::future::join_all(calls).await;

            for result in results {
                let result = result.expect("proxied tachi_memory search should succeed");
                assert_tool_ok(&result);
                let parsed = first_text_json(&result);
                assert_search_section_rows_are_objects(&parsed);
                let text = parsed.to_string();
                assert!(
                    text.contains("global-proxy-row-shape-e2e"),
                    "proxied search lost global row: {parsed:#}"
                );
                assert!(
                    text.contains("project-proxy-row-shape-e2e"),
                    "proxied search lost bound project row: {parsed:#}"
                );
            }

            let runtime = call_tool_via_stdio_proxy(proxy, "runtime_info", serde_json::Map::new())
                .await
                .expect("runtime_info should succeed");
            assert_tool_ok(&runtime);

            (ct, daemon_task)
        });

        ct.cancel();
        rt.block_on(daemon_task).expect("daemon task");
        restore_env("TACHI_HOME", saved_home);
        restore_env("SIGIL_HOME", saved_sigil);
        restore_env("TACHI_APP_HOME", saved_app);
    }

    #[test]
    fn stdio_proxy_delete_and_archive_global_rows_with_bound_project() {
        let _guard = crate::utils::global_test_lock()
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let saved_home = std::env::var_os("TACHI_HOME");
        let saved_sigil = std::env::var_os("SIGIL_HOME");
        let saved_app = std::env::var_os("TACHI_APP_HOME");

        let temp = tempfile::tempdir().expect("tempdir");
        let tachi_home = temp.path().join("home");
        let global = tachi_home.join("global/memory.db");
        let project_name = "Sigil-proxy-delete-e2e";
        let project = tachi_home
            .join("projects")
            .join(project_name)
            .join("memory.db");
        std::fs::create_dir_all(global.parent().expect("global parent")).expect("global parent");
        std::env::set_var("TACHI_HOME", &tachi_home);
        std::env::remove_var("SIGIL_HOME");
        std::env::remove_var("TACHI_APP_HOME");
        seed_project_db(&tachi_home, &project);

        let rt = test_runtime();
        let (ct, daemon_task) = rt.block_on(async {
            let server = crate::MemoryServer::new(global.clone(), None).expect("daemon server");
            server.set_tool_profile(Some(tachi_hub::ToolProfile::admin()));
            let (daemon, ct, daemon_task) = spawn_test_http_daemon(server, &global).await;
            let proxy = StdioProxyServer {
                daemon: std::sync::Arc::new(std::sync::RwLock::new(daemon)),
                app_home: tachi_home.clone(),
                global_db_path: global.clone(),
                project_db_path: Some(project.clone()),
                client_project: Some(project_name.to_string()),
            };

            for (id, summary) in [
                ("global-proxy-delete-e2e", "global delete fallback row"),
                ("global-proxy-archive-e2e", "global archive fallback row"),
            ] {
                let result = call_tool_via_stdio_proxy(
                    proxy.clone(),
                    "tachi_memory",
                    serde_json::Map::from_iter([
                        ("action".to_string(), serde_json::json!("save")),
                        ("id".to_string(), serde_json::json!(id)),
                        (
                            "text".to_string(),
                            serde_json::json!(format!("{summary} PROXYMUTATEGLOBAL")),
                        ),
                        ("summary".to_string(), serde_json::json!(summary)),
                        (
                            "path".to_string(),
                            serde_json::json!("/tests/stdio-proxy-mutate-global"),
                        ),
                        ("category".to_string(), serde_json::json!("fact")),
                        ("scope".to_string(), serde_json::json!("global")),
                        ("force".to_string(), serde_json::json!(true)),
                    ]),
                )
                .await
                .unwrap_or_else(|err| panic!("seed {id}: {err}"));
                assert_tool_ok(&result);
            }

            let deleted = call_tool_via_stdio_proxy(
                proxy.clone(),
                "delete_memory",
                serde_json::Map::from_iter([(
                    "id".to_string(),
                    serde_json::json!("global-proxy-delete-e2e"),
                )]),
            )
            .await
            .expect("proxied delete should succeed");
            assert_tool_ok(&deleted);
            let deleted = first_text_json(&deleted);
            assert_eq!(deleted["deleted"], serde_json::json!(true));
            assert_eq!(deleted["db"], serde_json::json!("global"));

            let archived = call_tool_via_stdio_proxy(
                proxy,
                "archive_memory",
                serde_json::Map::from_iter([(
                    "id".to_string(),
                    serde_json::json!("global-proxy-archive-e2e"),
                )]),
            )
            .await
            .expect("proxied archive should succeed");
            assert_tool_ok(&archived);
            let archived = first_text_json(&archived);
            assert_eq!(archived["archived"], serde_json::json!(true));
            assert_eq!(archived["db"], serde_json::json!("global"));

            (ct, daemon_task)
        });

        assert_eq!(memory_id_count(&global, "global-proxy-delete-e2e"), 0);
        assert_eq!(
            memory_archived_value(&global, "global-proxy-archive-e2e"),
            1
        );
        assert_eq!(memory_id_count(&project, "global-proxy-delete-e2e"), 0);
        assert_eq!(memory_id_count(&project, "global-proxy-archive-e2e"), 0);

        ct.cancel();
        rt.block_on(daemon_task).expect("daemon task");
        restore_env("TACHI_HOME", saved_home);
        restore_env("SIGIL_HOME", saved_sigil);
        restore_env("TACHI_APP_HOME", saved_app);
    }

    #[test]
    fn auto_daemon_args_preserve_global_only_scope() {
        let args = auto_daemon_command_args(Path::new("/tmp/agent.db"), None, "0");
        let rendered: Vec<String> = args
            .iter()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect();

        assert_eq!(
            rendered,
            vec![
                "--daemon",
                "--port",
                "0",
                "--global-db",
                "/tmp/agent.db",
                "--no-project-db"
            ]
        );
    }

    #[test]
    fn auto_daemon_args_preserve_project_scope() {
        let args = auto_daemon_command_args(
            Path::new("/tmp/global.db"),
            Some(Path::new("/tmp/project.db")),
            "1234",
        );
        let rendered: Vec<String> = args
            .iter()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect();

        assert_eq!(
            rendered,
            vec![
                "--daemon",
                "--port",
                "1234",
                "--global-db",
                "/tmp/global.db",
                "--project-db",
                "/tmp/project.db"
            ]
        );
    }

    #[test]
    fn stdio_proxy_accepts_matching_project_daemon() {
        let global = Path::new("/tmp/tachi/global/memory.db");
        let project = Path::new("/tmp/tachi/sigil/memory.db");
        let info = daemon(Some(global), Some(project));

        assert!(proxy_can_preserve_project_context(
            &info,
            global,
            Some(project),
            None
        ));
    }

    #[test]
    fn stdio_proxy_rejects_matching_project_daemon_with_wrong_named_binding() {
        let temp = tempfile::tempdir().expect("tempdir");
        let tachi_home = temp.path().join("home");
        let global = tachi_home.join("global/memory.db");
        let project = tachi_home.join("projects/Sigil-test/memory.db");
        let other = tachi_home.join("projects/Quant-test/memory.db");
        for path in [&global, &project, &other] {
            std::fs::create_dir_all(path.parent().expect("parent")).expect("parent");
            std::fs::write(path, b"").expect("db placeholder");
        }
        let info = daemon(Some(&global), Some(&project));

        with_tachi_home(&tachi_home, || {
            assert!(!proxy_can_preserve_project_context(
                &info,
                &global,
                Some(&project),
                Some("Quant-test")
            ));
        });
    }

    #[test]
    fn stdio_proxy_rejects_named_project_when_daemon_project_differs() {
        let global = Path::new("/tmp/tachi/global/memory.db");
        let daemon_project = Path::new("/tmp/tachi/quant/memory.db");
        let requested_project = Path::new("/tmp/tachi/sigil/memory.db");
        let info = daemon(Some(global), Some(daemon_project));

        assert!(!proxy_can_preserve_project_context(
            &info,
            global,
            Some(requested_project),
            Some("Sigil-test")
        ));
    }

    #[test]
    fn stdio_proxy_rejects_project_daemon_for_no_project_client() {
        let global = Path::new("/tmp/tachi/global/memory.db");
        let project = Path::new("/tmp/tachi/quant/memory.db");
        let info = daemon(Some(global), Some(project));

        assert!(!proxy_can_preserve_project_context(
            &info, global, None, None
        ));
    }

    #[test]
    fn stdio_proxy_accepts_global_only_daemon_for_no_project_client() {
        let global = Path::new("/tmp/tachi/global/memory.db");
        let info = daemon(Some(global), None);

        assert!(proxy_can_preserve_project_context(
            &info, global, None, None
        ));
    }

    #[test]
    fn stdio_proxy_accepts_global_only_daemon_for_bound_named_project_client() {
        let temp = tempfile::tempdir().expect("tempdir");
        let tachi_home = temp.path().join("home");
        let global = tachi_home.join("global/memory.db");
        let project = tachi_home.join("projects/Sigil-test/memory.db");
        std::fs::create_dir_all(global.parent().expect("global parent")).expect("global parent");
        std::fs::create_dir_all(project.parent().expect("project parent")).expect("project parent");
        std::fs::write(&project, b"").expect("project db placeholder");
        let info = daemon(Some(&global), None);

        with_tachi_home(&tachi_home, || {
            assert!(proxy_can_preserve_project_context(
                &info,
                &global,
                Some(&project),
                Some("Sigil-test")
            ));
        });
    }

    #[test]
    fn stdio_proxy_rejects_unverified_named_project_binding() {
        let temp = tempfile::tempdir().expect("tempdir");
        let tachi_home = temp.path().join("home");
        let global = tachi_home.join("global/memory.db");
        let project = temp.path().join("repo/.tachi/memory.db");
        std::fs::create_dir_all(global.parent().expect("global parent")).expect("global parent");
        std::fs::create_dir_all(project.parent().expect("project parent")).expect("project parent");
        std::fs::write(&project, b"").expect("project db placeholder");
        let info = daemon(Some(&global), None);

        with_tachi_home(&tachi_home, || {
            assert!(!proxy_can_preserve_project_context(
                &info,
                &global,
                Some(&project),
                Some("Sigil-test")
            ));
        });
    }

    #[test]
    fn stdio_proxy_env_gate_accepts_common_truthy_values() {
        assert!(env_truthy("1"));
        assert!(env_truthy("true"));
        assert!(env_truthy(" yes "));
        assert!(!env_truthy("0"));
        assert!(!env_truthy("false"));
        assert!(!env_truthy(""));
    }

    #[test]
    fn proxy_maps_zero_arg_briefing_to_project_memory_briefing() {
        let request = rmcp::model::CallToolRequestParams::new("tachi_briefing");

        let mapped = prepare_proxy_tool_call(request, Some("Sigil-abc123")).expect("mapped");

        assert_eq!(mapped.name.as_ref(), "tachi_memory");
        let args = mapped.arguments.expect("briefing args");
        assert_eq!(args["action"], serde_json::json!("briefing"));
        assert_eq!(args["format"], serde_json::json!("markdown"));
        assert_eq!(args["compact"], serde_json::json!(true));
        assert_eq!(args["project"], serde_json::json!("Sigil-abc123"));
        assert!(args["query"]
            .as_str()
            .expect("query")
            .contains("Sigil-abc123"));
    }

    #[test]
    fn proxy_injects_bound_project_for_tachi_memory_project_actions() {
        let save = rmcp::model::CallToolRequestParams::new("tachi_memory").with_arguments(
            serde_json::Map::from_iter([("action".to_string(), serde_json::json!("save"))]),
        );
        let save = prepare_proxy_tool_call(save, Some("Sigil-abc123")).expect("save mapped");
        assert_eq!(
            save.arguments.expect("save args")["project"],
            serde_json::json!("Sigil-abc123")
        );

        let search = rmcp::model::CallToolRequestParams::new("tachi_memory").with_arguments(
            serde_json::Map::from_iter([("action".to_string(), serde_json::json!("search"))]),
        );
        let search = prepare_proxy_tool_call(search, Some("Sigil-abc123")).expect("search mapped");
        assert_eq!(
            search.arguments.expect("search args")["project"],
            serde_json::json!("Sigil-abc123")
        );

        for action in [
            "consolidate",
            "pattern_feedback",
            "recall_proposals",
            "review_recall_proposal",
            "apply_recall_proposals",
        ] {
            let request = rmcp::model::CallToolRequestParams::new("tachi_memory").with_arguments(
                serde_json::Map::from_iter([("action".to_string(), serde_json::json!(action))]),
            );
            let mapped =
                prepare_proxy_tool_call(request, Some("Sigil-abc123")).expect("action mapped");
            assert_eq!(
                mapped.arguments.expect("args")["project"],
                serde_json::json!("Sigil-abc123"),
                "{action} should inherit the bound project"
            );
        }
    }

    #[test]
    fn proxy_injects_bound_project_for_raw_project_tools() {
        for tool in [
            "search_memory",
            "find_similar_memory",
            "get_memory",
            "list_memories",
            "delete_memory",
            "archive_memory",
            "ingest",
            "ingest_event",
            "ingest_source",
            "tachi_search",
        ] {
            let mapped = prepare_proxy_tool_call(
                rmcp::model::CallToolRequestParams::new(tool),
                Some("Sigil-abc123"),
            )
            .unwrap_or_else(|err| panic!("{tool} should map: {err}"));
            assert_eq!(
                mapped.arguments.expect("args")["project"],
                serde_json::json!("Sigil-abc123"),
                "{tool} should inherit the bound project"
            );
        }
    }

    #[test]
    fn proxy_does_not_override_explicit_global_scope() {
        let request = rmcp::model::CallToolRequestParams::new("tachi_save").with_arguments(
            serde_json::Map::from_iter([("scope".to_string(), serde_json::json!("global"))]),
        );

        let mapped = prepare_proxy_tool_call(request, Some("Sigil-abc123")).expect("mapped");

        assert!(
            !mapped.arguments.expect("args").contains_key("project"),
            "explicit global writes must remain global"
        );
    }

    #[test]
    fn proxy_rejects_explicit_cross_project_write_override() {
        let request = rmcp::model::CallToolRequestParams::new("tachi_save").with_arguments(
            serde_json::Map::from_iter([("project".to_string(), serde_json::json!("Quant-test"))]),
        );

        let err = prepare_proxy_tool_call(request, Some("Sigil-test"))
            .expect_err("cross-project override should be rejected");

        assert!(
            err.to_string().contains("project binding mismatch"),
            "unexpected error: {err}"
        );

        let request = rmcp::model::CallToolRequestParams::new("tachi_wiki").with_arguments(
            serde_json::Map::from_iter([
                ("action".to_string(), serde_json::json!("write")),
                ("project".to_string(), serde_json::json!("Quant-test")),
            ]),
        );

        let err = prepare_proxy_tool_call(request, Some("Sigil-test"))
            .expect_err("cross-project wiki write should be rejected");

        assert!(
            err.to_string().contains("project binding mismatch"),
            "unexpected error: {err}"
        );

        for (tool, action) in [
            ("tachi_memory", Some("save")),
            ("tachi_memory", Some("extract_facts")),
            ("tachi_memory", Some("checkpoint")),
            ("delete_memory", None),
            ("save_memory", None),
            ("tachi_event", Some("emit")),
        ] {
            let mut args = serde_json::Map::from_iter([(
                "project".to_string(),
                serde_json::json!("Quant-test"),
            )]);
            if let Some(action) = action {
                args.insert("action".to_string(), serde_json::json!(action));
            }
            let request = rmcp::model::CallToolRequestParams::new(tool).with_arguments(args);

            let err = prepare_proxy_tool_call(request, Some("Sigil-test"))
                .expect_err("{tool}/{action:?} cross-project write must be rejected");

            assert!(
                err.to_string().contains("project binding mismatch"),
                "{tool}/{action:?} unexpected error: {err}"
            );
        }
    }

    #[test]
    fn proxy_allows_explicit_cross_project_read_override() {
        for (tool, action) in [
            ("tachi_memory", Some("search")),
            ("tachi_memory", Some("get")),
            ("tachi_memory", Some("briefing")),
            ("tachi_memory", Some("consolidate")),
            ("tachi_memory", Some("recall_simulate")),
            ("tachi_memory", Some("readiness")),
            ("tachi_search", None),
            ("search_memory", None),
            ("find_similar_memory", None),
            ("get_memory", None),
            ("list_memories", None),
            ("tachi_wiki", Some("search")),
            ("tachi_wiki", Some("browse")),
            ("tachi_wiki", Some("read")),
            ("tachi_event", Some("query")),
            ("tachi_event", Some("metrics")),
        ] {
            let mut args = serde_json::Map::from_iter([(
                "project".to_string(),
                serde_json::json!("Quant-test"),
            )]);
            if let Some(action) = action {
                args.insert("action".to_string(), serde_json::json!(action));
            }
            let request = rmcp::model::CallToolRequestParams::new(tool).with_arguments(args);

            let mapped = prepare_proxy_tool_call(request, Some("Sigil-test"))
                .unwrap_or_else(|err| panic!("{tool}/{action:?} should be forwarded: {err}"));

            assert_eq!(
                mapped.arguments.expect("args")["project"],
                serde_json::json!("Quant-test"),
                "{tool}/{action:?} should preserve explicit project"
            );
        }
    }

    #[test]
    fn proxy_rejects_cross_project_override_even_with_global_scope() {
        let request = rmcp::model::CallToolRequestParams::new("tachi_save").with_arguments(
            serde_json::Map::from_iter([
                ("scope".to_string(), serde_json::json!("global")),
                ("project".to_string(), serde_json::json!("Quant-test")),
            ]),
        );

        let err = prepare_proxy_tool_call(request, Some("Sigil-test"))
            .expect_err("cross-project override should be rejected");

        assert!(
            err.to_string().contains("project binding mismatch"),
            "unexpected error: {err}"
        );
    }
}
