//! CLI client glue: forward CLI subcommand invocations to a running Tachi
//! daemon when one is detected, otherwise fall back to a transient in-process
//! `MemoryServer` so we use the same code path as the MCP tool handlers.
//!
//! Detection strategy:
//!   1. Read `~/.tachi/daemon.pid` (written by `tachi --daemon` on startup).
//!   2. Confirm liveness with a quick TCP connect to the recorded port.
//!   3. If both succeed, forward via MCP `tools/call` over streamable HTTP.
//!   4. Otherwise, build an in-process `MemoryServer` and call the handler
//!      directly.
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

/// Discovery info for a running Tachi daemon.
#[derive(Debug, Clone)]
pub(crate) struct DaemonInfo {
    pub url: String,
    pub global_db: Option<String>,
    pub version: Option<String>,
}

/// Look up `~/.tachi/daemon.pid` and verify the daemon is actually listening.
/// Returns `None` if the file is missing, malformed, or the port is closed.
pub(crate) async fn detect_daemon(app_home: &Path) -> Option<DaemonInfo> {
    let pid_path = app_home.join("daemon.pid");
    let raw = tokio::fs::read_to_string(&pid_path).await.ok()?;
    let parsed: Value = serde_json::from_str(&raw).ok()?;

    parsed.get("pid").and_then(|v| v.as_u64())?;
    let port = parsed.get("port").and_then(|v| v.as_u64())? as u16;
    let url = parsed
        .get("url")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
        .unwrap_or_else(|| format!("http://127.0.0.1:{port}/mcp"));

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
            version: parsed
                .get("version")
                .and_then(|value| value.as_str())
                .map(str::to_string),
        }),
        _ => None,
    }
}

/// Call a tool over MCP-streamable-HTTP against a known daemon URL.
/// Returns the first text content block from the tool result.
pub(crate) async fn call_daemon_tool(
    info: &DaemonInfo,
    tool_name: &str,
    arguments: serde_json::Map<String, Value>,
) -> Result<String, String> {
    let (daemon_tool, daemon_args) = remap_daemon_tool(tool_name, arguments);
    let transport_config = StreamableHttpClientTransportConfig::with_uri(info.url.clone());
    let transport = StreamableHttpClientTransport::from_config(transport_config);
    let client = ServiceExt::serve((), transport)
        .await
        .map_err(|e| format!("daemon handshake failed at {}: {e}", info.url))?;

    let mut params = CallToolRequestParams::new(daemon_tool.clone());
    if !daemon_args.is_empty() {
        params = params.with_arguments(daemon_args);
    }

    let peer = client.peer().clone();
    let result = tokio::time::timeout(DAEMON_CALL_TIMEOUT, peer.call_tool(params))
        .await
        .map_err(|_| {
            format!(
                "daemon call '{daemon_tool}' timed out after {:?}",
                DAEMON_CALL_TIMEOUT
            )
        })?
        .map_err(|e| format!("daemon call '{daemon_tool}' failed: {e}"))?;

    // Dropping the client is enough to close the short-lived CLI HTTP session.
    // Calling `cancel()` here has caused daemon-side lifecycle confusion with
    // rmcp streamable HTTP: a successful forwarded write could be followed by
    // the daemon exiting and leaving a stale pid file.
    drop(client);

    if result.is_error.unwrap_or(false) {
        let err_text =
            first_text_block(&result.content).unwrap_or_else(|| "<no error text>".to_string());
        return Err(format!(
            "daemon tool '{daemon_tool}' returned error: {err_text}"
        ));
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
/// Returns `Some(body)` on success. Returns `None` when already in daemon
/// mode, no daemon is listening, or forward failed (caller continues in-process).
pub(crate) async fn maybe_forward_write<T: serde::Serialize>(
    global_db_path: &Path,
    tool_name: &str,
    params: &T,
) -> Option<String> {
    if is_daemon_process() {
        return None;
    }
    let args = serde_json::to_value(params)
        .ok()
        .and_then(|value| value.as_object().cloned())?;
    let app_home = app_home_from_global_db(global_db_path);
    let info = detect_daemon(&app_home).await?;
    if !daemon_version_matches(&info) {
        eprintln!(
            "[mcp] daemon version mismatch (daemon {:?}, binary {}); executing in-process",
            info.version,
            env!("CARGO_PKG_VERSION")
        );
        return None;
    }
    if !daemon_global_db_matches(&info, global_db_path) {
        return None;
    }
    match call_daemon_tool(&info, tool_name, args).await {
        Ok(body) => Some(body),
        Err(error) => {
            eprintln!("[mcp] daemon forward '{tool_name}' failed ({error}); executing in-process");
            None
        }
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
