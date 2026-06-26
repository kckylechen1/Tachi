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
//!      dispatch is attempted, write paths fail closed instead of repeating
//!      writes locally; read/result paths may fall back because they do not
//!      commit user-authored content.
//!
//! Write paths (remember, wiki_write, extract_facts) use this so they go through
//! capture gate, provenance, auto-link, and enrichment exactly the same way as
//! the MCP tools. Read/result paths use it so stdio MCP adapters stay
//! consistent with the authoritative daemon's ranking and DB routing logic.

use std::path::{Path, PathBuf};
use std::time::Duration;

use rmcp::model::{CallToolRequestParams, ListToolsResult, RawContent};
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ForwardFallback {
    /// Writes must not be replayed locally after the daemon might have handled
    /// them, or agents can create duplicate memories/jobs.
    Write,
    /// Read/result tools do not create user-authored content, so the stdio
    /// adapter can fall back to its local handler when daemon transport fails.
    /// Search may duplicate access accounting in this rare path; that is
    /// preferable to failing recall outright.
    Read,
}

impl ForwardFallback {
    fn allows_in_process_fallback(self, error: &DaemonCallError) -> bool {
        match self {
            Self::Write => error.allows_in_process_fallback(),
            Self::Read => true,
        }
    }

    fn fallback_label(self) -> &'static str {
        match self {
            Self::Write => "write",
            Self::Read => "read",
        }
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
    pub pid: Option<i64>,
}

/// Look up `~/.tachi/daemon.pid` and verify the daemon is actually listening.
/// Returns `None` if the file is missing, malformed, or the port is closed.
pub(crate) async fn detect_daemon(app_home: &Path) -> Option<DaemonInfo> {
    detect_daemon_from_pid_path(&crate::daemon_lock::legacy_daemon_pid_path(app_home)).await
}

/// Detect the daemon for a specific global DB. Newer Tachi runtimes write a
/// scoped discovery file so multiple embedded runtimes sharing one app_home do
/// not steal each other's CLI/MCP writes. Legacy daemon.pid remains a fallback
/// only when it matches the requested global DB.
pub(crate) async fn detect_daemon_for_global_db(
    app_home: &Path,
    global_db_path: &Path,
) -> Option<DaemonInfo> {
    let scoped_pid = crate::daemon_lock::scoped_daemon_pid_path(app_home, global_db_path);
    if let Some(info) = detect_daemon_from_pid_path(&scoped_pid).await {
        if daemon_global_db_matches(&info, global_db_path) {
            return Some(info);
        }
    }

    let info = detect_daemon(app_home).await?;
    daemon_global_db_matches(&info, global_db_path).then_some(info)
}

async fn detect_daemon_from_pid_path(pid_path: &Path) -> Option<DaemonInfo> {
    let raw = tokio::fs::read_to_string(pid_path).await.ok()?;
    let parsed: Value = serde_json::from_str(&raw).ok()?;

    let pid = parsed.get("pid").and_then(|v| v.as_u64())?;
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
            pid: i64::try_from(pid).ok(),
        }),
        _ => None,
    }
}

/// Parse a `MAJOR.MINOR.PATCH` prefix into a comparable tuple, ignoring any
/// `-pre`/`+build` suffix. Returns `None` if the three core numbers aren't all
/// present — callers treat that as "unknown, do not act".
fn semver_triple(v: &str) -> Option<(u64, u64, u64)> {
    let core = v.trim().split(['-', '+']).next().unwrap_or("");
    let mut it = core.split('.');
    let major = it.next()?.parse().ok()?;
    let minor = it.next()?.parse().ok()?;
    let patch = it.next()?.parse().ok()?;
    Some((major, minor, patch))
}

/// True only when the running daemon's version is STRICTLY OLDER than this
/// binary. Used to decide whether to replace a stale daemon on stdio startup.
/// Conservative by design: an equal, newer, or unparseable version returns
/// false so we never kill a current or ahead-of-us daemon (e.g. mid-rollout).
pub(crate) fn daemon_is_older_than_current(info: &DaemonInfo) -> bool {
    let current = match semver_triple(env!("CARGO_PKG_VERSION")) {
        Some(c) => c,
        None => return false,
    };
    match info.version.as_deref().and_then(semver_triple) {
        Some(running) => running < current,
        None => false,
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
    let mut params = CallToolRequestParams::new(daemon_tool.clone());
    if !daemon_args.is_empty() {
        params = params.with_arguments(daemon_args);
    }
    let result = call_daemon_tool_raw(info, params).await?;
    if result.is_error.unwrap_or(false) {
        let err_text =
            first_text_block(&result.content).unwrap_or_else(|| "<no error text>".to_string());
        return Err(DaemonCallError::AfterDispatch(format!(
            "daemon tool '{daemon_tool}' returned error: {err_text}"
        )));
    }

    Ok(first_text_block(&result.content).unwrap_or_else(|| "{}".to_string()))
}

/// Call a daemon tool over Streamable HTTP without remapping the name or
/// collapsing the result to text. stdio proxy mode uses this to stay a pure
/// transport adapter while the daemon remains the semantic owner.
pub(crate) async fn call_daemon_tool_raw(
    info: &DaemonInfo,
    params: CallToolRequestParams,
) -> Result<rmcp::model::CallToolResult, DaemonCallError> {
    let tool_name = params.name.as_ref().to_string();
    let transport_config = StreamableHttpClientTransportConfig::with_uri(info.url.clone());
    let transport = StreamableHttpClientTransport::from_config(transport_config);
    let client = ServiceExt::serve((), transport).await.map_err(|e| {
        DaemonCallError::BeforeDispatch(format!("daemon handshake failed at {}: {e}", info.url))
    })?;

    let peer = client.peer().clone();
    let result = tokio::time::timeout(DAEMON_CALL_TIMEOUT, peer.call_tool(params))
        .await
        .map_err(|_| {
            DaemonCallError::AfterDispatch(format!(
                "daemon call '{tool_name}' timed out after {:?}",
                DAEMON_CALL_TIMEOUT
            ))
        })?
        .map_err(|e| {
            DaemonCallError::AfterDispatch(format!("daemon call '{tool_name}' failed: {e}"))
        })?;

    // Dropping the client is enough to close the short-lived CLI HTTP session.
    // Calling `cancel()` here has caused daemon-side lifecycle confusion with
    // rmcp streamable HTTP: a successful forwarded write could be followed by
    // the daemon exiting and leaving a stale pid file.
    drop(client);

    Ok(result)
}

/// List daemon tools over Streamable HTTP. stdio proxy mode uses daemon-side
/// discovery so it does not need to construct a local MemoryServer or open DBs.
pub(crate) async fn list_daemon_tools(
    info: &DaemonInfo,
    params: Option<rmcp::model::PaginatedRequestParams>,
) -> Result<ListToolsResult, DaemonCallError> {
    let transport_config = StreamableHttpClientTransportConfig::with_uri(info.url.clone());
    let transport = StreamableHttpClientTransport::from_config(transport_config);
    let client = ServiceExt::serve((), transport).await.map_err(|e| {
        DaemonCallError::BeforeDispatch(format!("daemon handshake failed at {}: {e}", info.url))
    })?;

    let peer = client.peer().clone();
    let result = tokio::time::timeout(DAEMON_CALL_TIMEOUT, peer.list_tools(params))
        .await
        .map_err(|_| {
            DaemonCallError::AfterDispatch(format!(
                "daemon tools/list timed out after {:?}",
                DAEMON_CALL_TIMEOUT
            ))
        })?
        .map_err(|e| DaemonCallError::AfterDispatch(format!("daemon tools/list failed: {e}")))?;

    drop(client);
    Ok(result)
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

pub(crate) fn daemon_version_matches(info: &DaemonInfo) -> bool {
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

pub(crate) fn daemon_global_db_matches(info: &DaemonInfo, global_db_path: &Path) -> bool {
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
    maybe_forward_tool(
        server.global_db_path.as_path(),
        project_db_path.as_deref(),
        tool_name,
        params,
        ForwardFallback::Write,
    )
    .await
}

#[cfg(test)]
pub(crate) async fn maybe_forward_write<T: serde::Serialize>(
    global_db_path: &Path,
    project_db_path: Option<&Path>,
    tool_name: &str,
    params: &T,
) -> Result<Option<String>, String> {
    maybe_forward_tool(
        global_db_path,
        project_db_path,
        tool_name,
        params,
        ForwardFallback::Write,
    )
    .await
}

/// Forward a read-only tool to the running daemon when available.
///
/// Unlike writes, reads can safely fall back to the in-process handler if the
/// daemon call fails after dispatch. This keeps old stdio adapters aligned with
/// daemon-side search/routing fixes while preserving local resilience.
pub(crate) async fn maybe_forward_server_read<T: serde::Serialize>(
    server: &MemoryServer,
    tool_name: &str,
    params: &T,
) -> Result<Option<String>, String> {
    let project_db_path = server.project_db_path_buf();
    maybe_forward_read(
        server.global_db_path.as_path(),
        project_db_path.as_deref(),
        tool_name,
        params,
    )
    .await
}

pub(crate) async fn maybe_forward_read<T: serde::Serialize>(
    global_db_path: &Path,
    project_db_path: Option<&Path>,
    tool_name: &str,
    params: &T,
) -> Result<Option<String>, String> {
    maybe_forward_tool(
        global_db_path,
        project_db_path,
        tool_name,
        params,
        ForwardFallback::Read,
    )
    .await
}

async fn maybe_forward_tool<T: serde::Serialize>(
    global_db_path: &Path,
    project_db_path: Option<&Path>,
    tool_name: &str,
    params: &T,
    fallback: ForwardFallback,
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
    let Some(info) = detect_daemon_for_global_db(&app_home, global_db_path).await else {
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
        Err(error) if fallback.allows_in_process_fallback(&error) => {
            eprintln!(
                "[mcp] daemon {} forward '{tool_name}' failed ({}); executing in-process",
                fallback.fallback_label(),
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
    crate::provider_config::bootstrap_provider_runtime(&server);
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
            pid: Some(std::process::id() as i64),
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

    fn daemon_versioned(version: Option<&str>) -> DaemonInfo {
        DaemonInfo {
            url: "http://127.0.0.1:6919/mcp".to_string(),
            global_db: None,
            project_db: None,
            version: version.map(str::to_string),
            pid: Some(1234),
        }
    }

    #[test]
    fn semver_triple_parses_core_and_ignores_suffix() {
        assert_eq!(semver_triple("1.5.6"), Some((1, 5, 6)));
        assert_eq!(semver_triple("1.5.6-rc.1"), Some((1, 5, 6)));
        assert_eq!(semver_triple("1.5.6+build.9"), Some((1, 5, 6)));
        assert_eq!(semver_triple("1.5"), None); // incomplete → unknown
        assert_eq!(semver_triple("garbage"), None);
    }

    #[test]
    fn daemon_is_older_only_when_strictly_behind_current() {
        let current = env!("CARGO_PKG_VERSION");
        // The running daemon reporting our exact version is NOT older.
        assert!(!daemon_is_older_than_current(&daemon_versioned(Some(
            current
        ))));
        // A clearly ancient version IS older.
        assert!(daemon_is_older_than_current(&daemon_versioned(Some(
            "0.0.1"
        ))));
        // A clearly future version is NOT older (never replace ahead-of-us).
        assert!(!daemon_is_older_than_current(&daemon_versioned(Some(
            "999.0.0"
        ))));
        // Unknown / unparseable / missing → never treated as older (safe).
        assert!(!daemon_is_older_than_current(&daemon_versioned(Some(
            "weird"
        ))));
        assert!(!daemon_is_older_than_current(&daemon_versioned(None)));
    }
}
