//! CLI client glue: forward CLI subcommand invocations to a running Tachi
//! daemon when one is detected, otherwise fall back to a transient in-process
//! `MemoryServer` so we use the same code path as the MCP tool handlers.
//!
//! Detection strategy:
//!   1. Read `~/.tachi/daemon.pid` (written by `tachi --daemon` on startup).
//!   2. Confirm liveness with a quick TCP connect to the recorded port.
//!   3. If both succeed, forward via MCP `tools/call` over streamable HTTP.
//!   4. If no compatible daemon is reached before dispatch, build an in-process
//!      `MemoryServer` and call the handler directly. Once a compatible daemon
//!      dispatch is attempted, fail closed instead of repeating writes locally.
//!
//! All write paths (remember, wiki_write, extract_facts) use this so they go
//! through capture gate, provenance, auto-link, and enrichment exactly the
//! same way as the MCP tools.

use std::path::{Path, PathBuf};
use std::time::Duration;

use rmcp::model::{CallToolRequestParams, RawContent};
use rmcp::transport::streamable_http_client::{
    StreamableHttpClientTransport, StreamableHttpClientTransportConfig,
};
use rmcp::ServiceExt;
use serde_json::Value;

use crate::MemoryServer;

const DAEMON_PROBE_TIMEOUT: Duration = Duration::from_millis(300);
const DAEMON_CALL_TIMEOUT: Duration = Duration::from_secs(60);

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum DaemonCallError {
    BeforeDispatch(String),
    AfterDispatch(String),
}

impl DaemonCallError {
    fn message(&self) -> &str {
        match self {
            Self::BeforeDispatch(message) | Self::AfterDispatch(message) => message,
        }
    }

    pub(crate) fn allows_in_process_fallback(&self) -> bool {
        matches!(self, Self::BeforeDispatch(_))
    }
}

impl std::fmt::Display for DaemonCallError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.message())
    }
}

impl std::error::Error for DaemonCallError {}

/// Discovery info for a running Tachi daemon.
#[derive(Debug, Clone)]
pub(crate) struct DaemonInfo {
    pub url: String,
    pub global_db: Option<String>,
    pub project_db: Option<String>,
    pub version: Option<String>,
}

/// Look up `~/.tachi/daemon.pid` and verify the daemon is actually listening.
/// Returns `None` if the file is missing, malformed, or the port is closed.
pub(crate) async fn detect_daemon(app_home: &Path) -> Option<DaemonInfo> {
    let pid_path = app_home.join("daemon.pid");
    let raw = tokio::fs::read_to_string(&pid_path).await.ok()?;
    let parsed: Value = serde_json::from_str(&raw).ok()?;

    parsed.get("pid").and_then(|v| v.as_u64())?;
    let port = u16::try_from(parsed.get("port").and_then(|v| v.as_u64())?).ok()?;
    let url = daemon_url_from_pid(&parsed, port)?;

    // Quick TCP probe so we don't hang the CLI on a stale pid file.
    let addr = format!("127.0.0.1:{port}");
    let probe =
        tokio::time::timeout(DAEMON_PROBE_TIMEOUT, tokio::net::TcpStream::connect(&addr)).await;

    match probe {
        Ok(Ok(_)) => Some(DaemonInfo {
            url,
            global_db: parsed
                .get("global_db")
                .and_then(|value| value.as_str())
                .map(str::to_string),
            project_db: parsed
                .get("project_db")
                .and_then(|value| value.as_str())
                .map(str::to_string),
            version: parsed
                .get("version")
                .and_then(|value| value.as_str())
                .map(str::to_string),
        }),
        _ => None,
    }
}

fn daemon_url_from_pid(parsed: &Value, port: u16) -> Option<String> {
    let Some(raw_url) = parsed.get("url").and_then(|v| v.as_str()) else {
        return Some(format!("http://127.0.0.1:{port}/mcp"));
    };
    let url = reqwest::Url::parse(raw_url).ok()?;
    if url.scheme() != "http"
        || !url.username().is_empty()
        || url.password().is_some()
        || url.path() != "/mcp"
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return None;
    }
    let host = url.host_str()?;
    if host != "127.0.0.1" && !host.eq_ignore_ascii_case("localhost") {
        return None;
    }
    if url.port_or_known_default()? != port {
        return None;
    }
    Some(format!("http://127.0.0.1:{port}/mcp"))
}

/// Call a tool over MCP-streamable-HTTP against a known daemon URL.
/// Returns the first text content block from the tool result.
pub(crate) async fn call_daemon_tool(
    info: &DaemonInfo,
    tool_name: &str,
    arguments: serde_json::Map<String, Value>,
) -> Result<String, DaemonCallError> {
    let (daemon_tool, daemon_args) = remap_daemon_tool(tool_name, arguments);
    let transport_config = StreamableHttpClientTransportConfig::with_uri(info.url.clone());
    let transport = StreamableHttpClientTransport::from_config(transport_config);
    let client = ServiceExt::serve((), transport).await.map_err(|e| {
        DaemonCallError::BeforeDispatch(format!("daemon handshake failed at {}: {e}", info.url))
    })?;

    let mut params = CallToolRequestParams::new(daemon_tool.clone());
    if !daemon_args.is_empty() {
        params = params.with_arguments(daemon_args);
    }

    let peer = client.peer().clone();
    let result = tokio::time::timeout(DAEMON_CALL_TIMEOUT, peer.call_tool(params))
        .await
        .map_err(|_| {
            DaemonCallError::AfterDispatch(format!(
                "daemon call '{daemon_tool}' timed out after {:?}",
                DAEMON_CALL_TIMEOUT
            ))
        })?
        .map_err(|e| {
            DaemonCallError::AfterDispatch(format!("daemon call '{daemon_tool}' failed: {e}"))
        })?;

    // Dropping the client is enough to close the short-lived CLI HTTP session.
    // Calling `cancel()` here has caused daemon-side lifecycle confusion with
    // rmcp streamable HTTP: a successful forwarded write could be followed by
    // the daemon exiting and leaving a stale pid file.
    drop(client);

    if result.is_error.unwrap_or(false) {
        let err_text =
            first_text_block(&result.content).unwrap_or_else(|| "<no error text>".to_string());
        return Err(DaemonCallError::AfterDispatch(format!(
            "daemon tool '{daemon_tool}' returned error: {err_text}"
        )));
    }

    Ok(first_text_block(&result.content).unwrap_or_else(|| "{}".to_string()))
}

fn first_text_block(blocks: &[rmcp::model::Annotated<RawContent>]) -> Option<String> {
    blocks.iter().find_map(|c| match &c.raw {
        RawContent::Text(t) => Some(t.text.clone()),
        _ => None,
    })
}

/// True when this process is the long-lived HTTP daemon (not stdio MCP / CLI).
pub(crate) fn is_daemon_process() -> bool {
    std::env::var("TACHI_DAEMON")
        .map(|value| {
            let value = value.trim();
            value == "1" || value.eq_ignore_ascii_case("true") || value.eq_ignore_ascii_case("yes")
        })
        .unwrap_or(false)
}

/// Resolve `~/.tachi` (or `TACHI_HOME`) from the canonical global DB path.
pub(crate) fn app_home_from_global_db(global_db_path: &Path) -> PathBuf {
    if std::env::var("TACHI_HOME").is_ok()
        || std::env::var("SIGIL_HOME").is_ok()
        || std::env::var("TACHI_APP_HOME").is_ok()
    {
        return crate::path_utils::tachi_home();
    }
    if let Some(global_dir) = global_db_path.parent() {
        if global_dir.file_name().and_then(|name| name.to_str()) == Some("global") {
            if let Some(app_home) = global_dir.parent() {
                return app_home.to_path_buf();
            }
        }
    }
    crate::path_utils::tachi_home()
}

fn daemon_version_matches(info: &DaemonInfo) -> bool {
    match info.version.as_deref() {
        Some(v) => v == env!("CARGO_PKG_VERSION"),
        None => false,
    }
}

pub(crate) fn daemon_matches_requested_dbs(
    info: &DaemonInfo,
    global_db_path: &Path,
    project_db_path: Option<&Path>,
) -> bool {
    daemon_global_db_matches(info, global_db_path)
        && daemon_project_db_matches(info, project_db_path)
}

fn daemon_global_db_matches(info: &DaemonInfo, global_db_path: &Path) -> bool {
    let Some(daemon_global) = info.global_db.as_deref() else {
        return true;
    };
    if daemon_global.is_empty() {
        return true;
    }
    if global_db_path.as_os_str() == daemon_global {
        return true;
    }
    std::fs::canonicalize(global_db_path)
        .ok()
        .zip(std::fs::canonicalize(daemon_global).ok())
        .map(|(left, right)| left == right)
        .unwrap_or(false)
}

fn daemon_project_db_matches(info: &DaemonInfo, project_db_path: Option<&Path>) -> bool {
    match (info.project_db.as_deref(), project_db_path) {
        (None, None) => true,
        (Some(daemon_project), Some(requested_project)) if !daemon_project.is_empty() => {
            paths_match(requested_project, Path::new(daemon_project))
        }
        (Some(daemon_project), None) => daemon_project.is_empty(),
        _ => false,
    }
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

fn remap_daemon_tool(
    tool_name: &str,
    mut args: serde_json::Map<String, serde_json::Value>,
) -> (String, serde_json::Map<String, serde_json::Value>) {
    match tool_name {
        "extract_facts" => {
            args.insert("action".into(), serde_json::json!("extract_facts"));
            ("tachi_memory".into(), args)
        }
        "save_memory" | "remember" => {
            args.insert("action".into(), serde_json::json!("save"));
            ("tachi_memory".into(), args)
        }
        "search_memory" => {
            args.insert("action".into(), serde_json::json!("search"));
            ("tachi_memory".into(), args)
        }
        "get_memory" => {
            args.insert("action".into(), serde_json::json!("get"));
            ("tachi_memory".into(), args)
        }
        "tachi_wiki_search" | "wiki_search" => {
            args.insert("action".into(), serde_json::json!("search"));
            ("tachi_wiki".into(), args)
        }
        "tachi_wiki_write" | "wiki_write" => {
            args.insert("action".into(), serde_json::json!("write"));
            ("tachi_wiki".into(), args)
        }
        other => (other.to_string(), args),
    }
}

/// Forward a write tool to the running daemon when available.
/// Returns `Ok(Some(body))` on success and `Ok(None)` when no compatible daemon
/// was reached before dispatch. If a compatible daemon was reached and the
/// request outcome is unknown, returns `Err` so callers do not repeat writes
/// in-process after a possibly successful daemon-side commit.
pub(crate) async fn maybe_forward_server_write<T: serde::Serialize>(
    server: &MemoryServer,
    tool_name: &str,
    params: &T,
) -> Result<Option<String>, String> {
    let project_db_path = server.project_db_path_buf();
    maybe_forward_write(
        server.global_db_path.as_path(),
        project_db_path.as_deref(),
        tool_name,
        params,
    )
    .await
}

pub(crate) async fn maybe_forward_write<T: serde::Serialize>(
    global_db_path: &Path,
    project_db_path: Option<&Path>,
    tool_name: &str,
    params: &T,
) -> Result<Option<String>, String> {
    if is_daemon_process() {
        return Ok(None);
    }
    let Some(args) = serde_json::to_value(params)
        .ok()
        .and_then(|value| value.as_object().cloned())
    else {
        eprintln!(
            "[mcp] failed to serialize daemon forward args for '{tool_name}'; executing in-process"
        );
        return Ok(None);
    };
    let app_home = app_home_from_global_db(global_db_path);
    let Some(info) = detect_daemon(&app_home).await else {
        return Ok(None);
    };
    if !daemon_version_matches(&info) {
        eprintln!(
            "[mcp] daemon version mismatch (daemon {:?}, binary {}); executing in-process",
            info.version,
            env!("CARGO_PKG_VERSION")
        );
        return Ok(None);
    }
    if !daemon_matches_requested_dbs(&info, global_db_path, project_db_path) {
        return Ok(None);
    }
    match call_daemon_tool(&info, tool_name, args).await {
        Ok(body) => Ok(Some(body)),
        Err(error) if error.allows_in_process_fallback() => {
            eprintln!(
                "[mcp] daemon forward '{tool_name}' failed before dispatch ({}); executing in-process",
                error.message()
            );
            Ok(None)
        }
        Err(error) => Err(format!(
            "daemon forward '{tool_name}' failed after dispatch; refusing in-process fallback to avoid duplicate writes: {}",
            error.message()
        )),
    }
}

/// Build a transient in-process `MemoryServer` for one-shot CLI use.
/// Uses the same constructor as the daemon, so capture gate, enrichment,
/// auto-link, etc. all run identically.
pub(crate) fn build_in_process_server(
    global_db: &PathBuf,
    project_db: Option<&PathBuf>,
) -> Result<MemoryServer, Box<dyn std::error::Error>> {
    let server = MemoryServer::new(global_db.clone(), project_db.cloned())?;
    Ok(server)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn daemon(global: Option<&Path>, project: Option<&Path>) -> DaemonInfo {
        DaemonInfo {
            url: "http://127.0.0.1:6919/mcp".to_string(),
            global_db: global.map(|path| path.display().to_string()),
            project_db: project.map(|path| path.display().to_string()),
            version: Some(env!("CARGO_PKG_VERSION").to_string()),
        }
    }

    #[test]
    fn daemon_scope_matches_same_global_and_project() {
        let global = Path::new("/tmp/tachi/global/memory.db");
        let project = Path::new("/tmp/tachi/project/memory.db");
        let info = daemon(Some(global), Some(project));

        assert!(daemon_matches_requested_dbs(&info, global, Some(project)));
    }

    #[test]
    fn daemon_scope_rejects_different_project_db() {
        let global = Path::new("/tmp/tachi/global/memory.db");
        let daemon_project = Path::new("/tmp/tachi/sigil/memory.db");
        let requested_project = Path::new("/tmp/tachi/quant/memory.db");
        let info = daemon(Some(global), Some(daemon_project));

        assert!(!daemon_matches_requested_dbs(
            &info,
            global,
            Some(requested_project)
        ));
    }

    #[test]
    fn daemon_scope_rejects_project_daemon_for_no_project_request() {
        let global = Path::new("/tmp/tachi/global/memory.db");
        let project = Path::new("/tmp/tachi/sigil/memory.db");
        let info = daemon(Some(global), Some(project));

        assert!(!daemon_matches_requested_dbs(&info, global, None));
    }

    #[test]
    fn daemon_scope_accepts_global_only_when_both_have_no_project() {
        let global = Path::new("/tmp/tachi/global/memory.db");
        let info = daemon(Some(global), None);

        assert!(daemon_matches_requested_dbs(&info, global, None));
    }

    #[test]
    fn daemon_scope_rejects_missing_daemon_project_for_project_request() {
        let global = Path::new("/tmp/tachi/global/memory.db");
        let requested_project = Path::new("/tmp/tachi/quant/memory.db");
        let info = daemon(Some(global), None);

        assert!(!daemon_matches_requested_dbs(
            &info,
            global,
            Some(requested_project)
        ));
    }

    #[test]
    fn daemon_call_error_fallback_is_only_safe_before_dispatch() {
        assert!(
            DaemonCallError::BeforeDispatch("handshake failed".to_string())
                .allows_in_process_fallback()
        );
        assert!(!DaemonCallError::AfterDispatch("timeout".to_string()).allows_in_process_fallback());
    }

    #[tokio::test]
    async fn daemon_forward_non_object_args_fall_back_in_process() {
        let global = Path::new("/tmp/tachi/global/memory.db");
        let result = maybe_forward_write(global, None, "remember", &vec!["not", "an", "object"])
            .await
            .expect("non-object args should not fail the write path");

        assert!(result.is_none());
    }
}
