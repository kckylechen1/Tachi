//! Daemon discovery, DB-scope matching, and version compatibility checks.

use std::path::{Path, PathBuf};
use std::time::Duration;

use serde_json::Value;

const DAEMON_PROBE_TIMEOUT: Duration = Duration::from_millis(300);

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
pub(super) fn semver_triple(v: &str) -> Option<(u64, u64, u64)> {
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
