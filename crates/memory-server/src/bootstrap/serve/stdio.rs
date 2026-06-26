use super::*;
use std::ffi::OsString;
use std::future::Future;
use std::path::Path;
use tokio::io::{stdin, stdout};

pub(super) async fn ensure_stdio_proxy_daemon(
    app_home: &Path,
    global_db_path: &Path,
    project_db_path: Option<&Path>,
) -> Option<crate::cli_client::DaemonInfo> {
    let auto_daemon_disabled = auto_daemon_disabled();

    if !auto_daemon_disabled {
        replace_stale_daemon_if_needed(app_home, global_db_path).await;
    }

    if let Some(info) = compatible_daemon(app_home, global_db_path).await {
        return Some(info);
    }

    if auto_daemon_disabled {
        eprintln!("[auto-daemon] disabled by TACHI_DISABLE_AUTO_DAEMON");
        return None;
    }

    spawn_stdio_daemon(app_home, global_db_path, project_db_path).await;
    compatible_daemon(app_home, global_db_path).await
}

pub(super) fn proxy_can_preserve_project_context(
    info: &crate::cli_client::DaemonInfo,
    global_db_path: &Path,
    project_db_path: Option<&Path>,
    client_project: Option<&str>,
) -> bool {
    project_db_path.is_none()
        || client_project.is_some()
        || crate::cli_client::daemon_matches_requested_dbs(info, global_db_path, project_db_path)
}

pub(super) async fn serve_stdio_proxy(
    info: crate::cli_client::DaemonInfo,
    global_db_path: PathBuf,
    project_db_path: Option<PathBuf>,
    client_project: Option<String>,
) -> Result<(), Box<dyn std::error::Error>> {
    let proxy = StdioProxyServer {
        daemon: info,
        global_db_path,
        project_db_path,
        client_project,
    };
    let transport = (stdin(), stdout());
    let running = rmcp::service::serve_server(proxy, transport).await?;
    wait_for_stdio_shutdown(running).await;
    Ok(())
}

pub(super) async fn serve_stdio(server: MemoryServer) -> Result<(), Box<dyn std::error::Error>> {
    let transport = (stdin(), stdout());
    let running = rmcp::service::serve_server(server, transport).await?;
    wait_for_stdio_shutdown(running).await;
    Ok(())
}

async fn wait_for_stdio_shutdown(
    running: rmcp::service::RunningService<
        rmcp::service::RoleServer,
        impl rmcp::Service<rmcp::service::RoleServer>,
    >,
) {
    // Graceful shutdown: MCP quit (stdin EOF / client disconnect), SIGINT,
    // or parent-death. A well-behaved host closes stdin on disconnect so
    // `running.waiting()` resolves; the parent-death branch is the backstop
    // for hosts that leak the child (it gets reparented to init/launchd and
    // would otherwise linger forever).
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
    }
}

fn auto_daemon_disabled() -> bool {
    std::env::var("TACHI_DISABLE_AUTO_DAEMON")
        .map(|value| {
            let value = value.trim();
            value == "1" || value.eq_ignore_ascii_case("true") || value.eq_ignore_ascii_case("yes")
        })
        .unwrap_or(false)
}

async fn compatible_daemon(
    app_home: &Path,
    global_db_path: &Path,
) -> Option<crate::cli_client::DaemonInfo> {
    let info = crate::cli_client::detect_daemon_for_global_db(app_home, global_db_path).await?;
    if crate::cli_client::daemon_version_matches(&info) {
        Some(info)
    } else {
        eprintln!(
            "[stdio-proxy] daemon version mismatch (daemon {:?}, binary {}); using local fallback",
            info.version,
            env!("CARGO_PKG_VERSION")
        );
        None
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
                    wait_for_daemon_ready(app_home, global_db_path).await;
                }
                Err(e) => eprintln!("[auto-daemon] failed to spawn daemon: {e}"),
            }
        }
        Err(e) => eprintln!("[auto-daemon] cannot determine binary path: {e}"),
    }
}

async fn wait_for_daemon_ready(app_home: &Path, global_db_path: &Path) {
    let ready = tokio::time::timeout(Duration::from_secs(5), async {
        for _ in 0..25 {
            tokio::time::sleep(Duration::from_millis(200)).await;
            if crate::cli_client::detect_daemon_for_global_db(app_home, global_db_path)
                .await
                .is_some()
            {
                return true;
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
    daemon: crate::cli_client::DaemonInfo,
    global_db_path: PathBuf,
    project_db_path: Option<PathBuf>,
    client_project: Option<String>,
}

impl StdioProxyServer {
    fn runtime_info_result(&self) -> rmcp::model::CallToolResult {
        let body = serde_json::json!({
            "mode": "stdio_proxy",
            "process_role": "stdio_proxy",
            "db_handles": 0,
            "stdio_adapter": true,
            "authoritative_runtime": "daemon",
            "transport": {
                "inbound": "stdio",
                "outbound": "streamable_http",
                "target": self.daemon.url,
            },
            "daemon": {
                "pid": self.daemon.pid,
                "version": self.daemon.version,
                "global_db": self.daemon.global_db,
                "project_db": self.daemon.project_db,
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
        .with_instructions(crate::bootstrap::mcp_server_instructions())
    }

    fn list_tools(
        &self,
        request: Option<rmcp::model::PaginatedRequestParams>,
        _context: rmcp::service::RequestContext<rmcp::service::RoleServer>,
    ) -> impl Future<Output = Result<rmcp::model::ListToolsResult, rmcp::ErrorData>> + Send + '_
    {
        async move {
            crate::cli_client::list_daemon_tools(&self.daemon, request)
                .await
                .map_err(daemon_error_data)
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
            let request = prepare_proxy_tool_call(request, self.client_project.as_deref());
            crate::cli_client::call_daemon_tool_raw(&self.daemon, request)
                .await
                .map_err(daemon_error_data)
        }
    }
}

fn daemon_error_data(error: crate::cli_client::DaemonCallError) -> rmcp::ErrorData {
    rmcp::ErrorData::internal_error(format!("stdio proxy daemon call failed: {error}"), None)
}

fn prepare_proxy_tool_call(
    mut request: rmcp::model::CallToolRequestParams,
    client_project: Option<&str>,
) -> rmcp::model::CallToolRequestParams {
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
        return request;
    }

    if let Some(project) = client_project {
        inject_client_project(request.name.as_ref(), &mut request.arguments, project);
    }
    request
}

fn inject_client_project(
    tool_name: &str,
    arguments: &mut Option<rmcp::model::JsonObject>,
    project: &str,
) {
    if !project_aware_default_project_tool(tool_name) {
        return;
    }
    let args = arguments.get_or_insert_with(serde_json::Map::new);
    if tool_name == "tachi_memory"
        && args
            .get("action")
            .and_then(|value| value.as_str())
            .map(|action| !tachi_memory_action_defaults_to_project(action))
            .unwrap_or(false)
    {
        return;
    }
    if args.contains_key("project") {
        return;
    }
    if args
        .get("scope")
        .and_then(|value| value.as_str())
        .is_some_and(|scope| scope.eq_ignore_ascii_case("global"))
    {
        return;
    }
    args.insert("project".to_string(), serde_json::json!(project));
}

fn project_aware_default_project_tool(tool_name: &str) -> bool {
    matches!(
        tool_name,
        "save_memory"
            | "remember"
            | "extract_facts"
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
        "briefing" | "save" | "checkpoint" | "extract_facts" | "progress"
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
    fn proxy_maps_zero_arg_briefing_to_project_memory_briefing() {
        let request = rmcp::model::CallToolRequestParams::new("tachi_briefing");

        let mapped = prepare_proxy_tool_call(request, Some("Sigil-abc123"));

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
    fn proxy_injects_project_for_tachi_memory_writes_only() {
        let save = rmcp::model::CallToolRequestParams::new("tachi_memory").with_arguments(
            serde_json::Map::from_iter([("action".to_string(), serde_json::json!("save"))]),
        );
        let save = prepare_proxy_tool_call(save, Some("Sigil-abc123"));
        assert_eq!(
            save.arguments.expect("save args")["project"],
            serde_json::json!("Sigil-abc123")
        );

        let search = rmcp::model::CallToolRequestParams::new("tachi_memory").with_arguments(
            serde_json::Map::from_iter([("action".to_string(), serde_json::json!("search"))]),
        );
        let search = prepare_proxy_tool_call(search, Some("Sigil-abc123"));
        assert!(
            !search
                .arguments
                .expect("search args")
                .contains_key("project"),
            "search should not be narrowed to a named project implicitly"
        );
    }

    #[test]
    fn proxy_does_not_override_explicit_global_scope() {
        let request = rmcp::model::CallToolRequestParams::new("tachi_save").with_arguments(
            serde_json::Map::from_iter([("scope".to_string(), serde_json::json!("global"))]),
        );

        let mapped = prepare_proxy_tool_call(request, Some("Sigil-abc123"));

        assert!(
            !mapped.arguments.expect("args").contains_key("project"),
            "explicit global writes must remain global"
        );
    }
}
