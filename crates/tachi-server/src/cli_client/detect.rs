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

/// Why a daemon probe returned nothing. Used by the stdio `Missing` path so the
/// refusal can name the failed check (pid file vs TCP) instead of staying mute.
/// Routine in-process forward fallbacks keep using [`Option`] and stay quiet.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum DaemonProbeFailure {
    /// No usable pid file at the probed path (absent, unreadable, or malformed).
    PidFileAbsent { path: PathBuf },
    /// Pid file parsed, but the recorded port refused the TCP probe.
    TcpProbeRefused { path: PathBuf, port: u16 },
}

/// Single stderr line for the stdio-proxy `DaemonCompatibility::Missing` arm.
/// Kept pure so unit tests can assert the text without capturing process stderr.
pub(crate) fn daemon_missing_stderr_line(failure: &DaemonProbeFailure) -> String {
    match failure {
        DaemonProbeFailure::PidFileAbsent { path } => {
            format!(
                "[stdio-proxy] no daemon found: pid file absent ({})",
                path.display()
            )
        }
        DaemonProbeFailure::TcpProbeRefused { path, port } => {
            format!(
                "[stdio-proxy] no daemon found: TCP probe refused at 127.0.0.1:{port} (pid file {})",
                path.display()
            )
        }
    }
}

/// Emit the Missing-branch line. Callers that treat absence as a quiet
/// fallback must not call this — only the stdio compatibility `Missing` arm.
pub(crate) fn emit_daemon_missing_reason(failure: &DaemonProbeFailure) {
    eprintln!("{}", daemon_missing_stderr_line(failure));
}

/// Look up `~/.tachi/daemon.pid` and verify the daemon is actually listening.
/// Returns `None` if the file is missing, malformed, or the port is closed.
pub(crate) async fn detect_daemon(app_home: &Path) -> Option<DaemonInfo> {
    detect_daemon_from_pid_path(&crate::daemon_lock::legacy_daemon_pid_path(app_home))
        .await
        .ok()
}

/// Detect the daemon for a specific global DB. Newer Tachi runtimes write a
/// scoped discovery file so multiple embedded runtimes sharing one app_home do
/// not steal each other's CLI/MCP writes. Legacy daemon.pid remains a fallback
/// only when it matches the requested global DB.
pub(crate) async fn detect_daemon_for_global_db(
    app_home: &Path,
    global_db_path: &Path,
) -> Option<DaemonInfo> {
    detect_daemon_for_global_db_result(app_home, global_db_path)
        .await
        .ok()
}

/// Same discovery as [`detect_daemon_for_global_db`], but preserves the probe
/// failure so the stdio `Missing` branch can name which check failed.
pub(crate) async fn detect_daemon_for_global_db_result(
    app_home: &Path,
    global_db_path: &Path,
) -> Result<DaemonInfo, DaemonProbeFailure> {
    let scoped_pid = crate::daemon_lock::scoped_daemon_pid_path(app_home, global_db_path);
    let scoped_failure = match detect_daemon_from_pid_path(&scoped_pid).await {
        Ok(info) if daemon_global_db_matches(&info, global_db_path) => return Ok(info),
        Ok(_) => {
            // Scoped pid pointed at a live daemon for a different global DB —
            // fall through to legacy. Treat as absent for this scope.
            None
        }
        // Scoped file exists but port is dead; still try legacy before
        // surfacing failure (legacy may own a matching live daemon). Keep the
        // refusal so composition can prefer it over a bare PidFileAbsent.
        Err(failure) => Some(failure),
    };

    let legacy_pid = crate::daemon_lock::legacy_daemon_pid_path(app_home);
    let legacy_failure = match detect_daemon_from_pid_path(&legacy_pid).await {
        Ok(info) if daemon_global_db_matches(&info, global_db_path) => return Ok(info),
        Ok(_) => DaemonProbeFailure::PidFileAbsent { path: legacy_pid },
        Err(failure) => failure,
    };
    Err(prefer_probe_failure(scoped_failure, legacy_failure))
}

/// Prefer the most informative probe failure when composing scoped + legacy.
/// Any [`DaemonProbeFailure::TcpProbeRefused`] outranks [`DaemonProbeFailure::PidFileAbsent`].
fn prefer_probe_failure(
    scoped: Option<DaemonProbeFailure>,
    legacy: DaemonProbeFailure,
) -> DaemonProbeFailure {
    match scoped {
        // Scoped TCP-refused must survive legacy fallback when legacy has no
        // better match (including legacy PidFileAbsent).
        Some(scoped @ DaemonProbeFailure::TcpProbeRefused { .. }) => scoped,
        // Scoped absent / mismatched / PidFileAbsent: take legacy as-is
        // (legacy TcpProbeRefused already outranks a bare absent).
        _ => legacy,
    }
}

async fn detect_daemon_from_pid_path(pid_path: &Path) -> Result<DaemonInfo, DaemonProbeFailure> {
    let raw = match tokio::fs::read_to_string(pid_path).await {
        Ok(raw) => raw,
        Err(_) => {
            return Err(DaemonProbeFailure::PidFileAbsent {
                path: pid_path.to_path_buf(),
            });
        }
    };
    let parsed: Value = match serde_json::from_str(&raw) {
        Ok(parsed) => parsed,
        Err(_) => {
            return Err(DaemonProbeFailure::PidFileAbsent {
                path: pid_path.to_path_buf(),
            });
        }
    };

    let Some(pid) = parsed.get("pid").and_then(|v| v.as_u64()) else {
        return Err(DaemonProbeFailure::PidFileAbsent {
            path: pid_path.to_path_buf(),
        });
    };
    let Some(port_u64) = parsed.get("port").and_then(|v| v.as_u64()) else {
        return Err(DaemonProbeFailure::PidFileAbsent {
            path: pid_path.to_path_buf(),
        });
    };
    let Ok(port) = u16::try_from(port_u64) else {
        return Err(DaemonProbeFailure::PidFileAbsent {
            path: pid_path.to_path_buf(),
        });
    };
    let Some(url) = daemon_url_from_pid(&parsed, port) else {
        return Err(DaemonProbeFailure::PidFileAbsent {
            path: pid_path.to_path_buf(),
        });
    };

    // Quick TCP probe so we don't hang the CLI on a stale pid file.
    let addr = format!("127.0.0.1:{port}");
    let probe =
        tokio::time::timeout(DAEMON_PROBE_TIMEOUT, tokio::net::TcpStream::connect(&addr)).await;

    match probe {
        Ok(Ok(_)) => Ok(DaemonInfo {
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
        _ => Err(DaemonProbeFailure::TcpProbeRefused {
            path: pid_path.to_path_buf(),
            port,
        }),
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

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use tempfile::TempDir;

    fn info(version: Option<&str>, global: Option<&str>, project: Option<&str>) -> DaemonInfo {
        DaemonInfo {
            url: "http://127.0.0.1:6919/mcp".into(),
            global_db: global.map(str::to_string),
            project_db: project.map(str::to_string),
            version: version.map(str::to_string),
            pid: Some(1),
        }
    }

    #[test]
    fn semver_triple_positive_parses_core_and_strips_suffix() {
        assert_eq!(semver_triple("1.9.0"), Some((1, 9, 0)));
        assert_eq!(semver_triple("1.9.0-rc.1"), Some((1, 9, 0)));
        assert_eq!(semver_triple("1.9.0+build.9"), Some((1, 9, 0)));
    }

    #[test]
    fn semver_triple_negative_rejects_incomplete_or_garbage() {
        assert_eq!(semver_triple("1.9"), None);
        assert_eq!(semver_triple("garbage"), None);
    }

    #[test]
    fn daemon_version_matches_positive_exact_crate_version() {
        let current = env!("CARGO_PKG_VERSION");
        assert!(daemon_version_matches(&info(Some(current), None, None)));
    }

    #[test]
    fn daemon_version_matches_negative_mismatch_or_missing() {
        assert!(!daemon_version_matches(&info(Some("0.0.1"), None, None)));
        assert!(!daemon_version_matches(&info(None, None, None)));
    }

    #[test]
    fn daemon_global_db_matches_positive_same_path_or_unset() {
        let global = Path::new("/tmp/tachi-detect/global/memory.db");
        assert!(daemon_global_db_matches(
            &info(None, Some("/tmp/tachi-detect/global/memory.db"), None),
            global
        ));
        // Unset / empty daemon global is treated as compatible (legacy pid).
        assert!(daemon_global_db_matches(&info(None, None, None), global));
        assert!(daemon_global_db_matches(
            &info(None, Some(""), None),
            global
        ));
    }

    #[test]
    fn daemon_global_db_matches_negative_different_path() {
        let global = Path::new("/tmp/tachi-detect/global/memory.db");
        assert!(!daemon_global_db_matches(
            &info(None, Some("/tmp/other/global/memory.db"), None),
            global
        ));
    }

    #[test]
    fn daemon_project_db_matches_positive_both_none_or_same() {
        let project = Path::new("/tmp/tachi-detect/projects/sigil/memory.db");
        assert!(daemon_project_db_matches(&info(None, None, None), None));
        assert!(daemon_project_db_matches(
            &info(
                None,
                None,
                Some("/tmp/tachi-detect/projects/sigil/memory.db")
            ),
            Some(project)
        ));
    }

    #[test]
    fn daemon_project_db_matches_negative_mismatch_or_asymmetric() {
        let requested = Path::new("/tmp/tachi-detect/projects/quant/memory.db");
        assert!(!daemon_project_db_matches(
            &info(
                None,
                None,
                Some("/tmp/tachi-detect/projects/sigil/memory.db")
            ),
            Some(requested)
        ));
        assert!(!daemon_project_db_matches(
            &info(
                None,
                None,
                Some("/tmp/tachi-detect/projects/sigil/memory.db")
            ),
            None
        ));
        assert!(!daemon_project_db_matches(
            &info(None, None, None),
            Some(requested)
        ));
    }

    #[test]
    fn daemon_matches_requested_dbs_positive_both_scopes() {
        let global = Path::new("/tmp/tachi-detect/global/memory.db");
        let project = Path::new("/tmp/tachi-detect/projects/sigil/memory.db");
        let d = info(
            None,
            Some("/tmp/tachi-detect/global/memory.db"),
            Some("/tmp/tachi-detect/projects/sigil/memory.db"),
        );
        assert!(daemon_matches_requested_dbs(&d, global, Some(project)));
    }

    #[test]
    fn daemon_matches_requested_dbs_negative_project_mismatch() {
        let global = Path::new("/tmp/tachi-detect/global/memory.db");
        let requested = Path::new("/tmp/tachi-detect/projects/quant/memory.db");
        let d = info(
            None,
            Some("/tmp/tachi-detect/global/memory.db"),
            Some("/tmp/tachi-detect/projects/sigil/memory.db"),
        );
        assert!(!daemon_matches_requested_dbs(&d, global, Some(requested)));
    }

    #[test]
    fn daemon_missing_stderr_line_names_pid_file_absent() {
        let path = PathBuf::from("/tmp/tachi/daemon.pid");
        let line =
            daemon_missing_stderr_line(&DaemonProbeFailure::PidFileAbsent { path: path.clone() });
        assert_eq!(
            line,
            "[stdio-proxy] no daemon found: pid file absent (/tmp/tachi/daemon.pid)"
        );
        // Discrimination: if this line were dropped/emptied, the assertion above fails.
        assert!(line.contains("pid file absent"));
        assert!(!line.contains("TCP probe refused"));
    }

    #[test]
    fn daemon_missing_stderr_line_names_tcp_probe_refused() {
        let path = PathBuf::from("/tmp/tachi/daemon.pid");
        let line = daemon_missing_stderr_line(&DaemonProbeFailure::TcpProbeRefused {
            path: path.clone(),
            port: 59999,
        });
        assert_eq!(
            line,
            "[stdio-proxy] no daemon found: TCP probe refused at 127.0.0.1:59999 (pid file /tmp/tachi/daemon.pid)"
        );
        // Discrimination: removing the TCP clause fails this assertion.
        assert!(line.contains("TCP probe refused"));
        assert!(line.contains("59999"));
        assert!(!line.contains("pid file absent"));
    }

    #[tokio::test]
    async fn detect_from_pid_path_reports_pid_file_absent() {
        let dir = TempDir::new().expect("tempdir");
        let missing = dir.path().join("daemon.pid");
        let err = detect_daemon_from_pid_path(&missing)
            .await
            .expect_err("absent pid file");
        assert_eq!(err, DaemonProbeFailure::PidFileAbsent { path: missing });
    }

    #[tokio::test]
    async fn detect_from_pid_path_reports_tcp_probe_refused() {
        let dir = TempDir::new().expect("tempdir");
        let pid_path = dir.path().join("daemon.pid");
        // Bind nothing on this port — OS-assigned free port via bind+drop, then
        // reuse the number so connect must refuse (or time out → same Err arm).
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
        let port = listener.local_addr().expect("addr").port();
        drop(listener);

        std::fs::write(
            &pid_path,
            serde_json::to_string(&json!({
                "pid": 1,
                "port": port,
            }))
            .expect("json"),
        )
        .expect("write pid");

        let err = detect_daemon_from_pid_path(&pid_path)
            .await
            .expect_err("tcp refuse");
        assert_eq!(
            err,
            DaemonProbeFailure::TcpProbeRefused {
                path: pid_path,
                port,
            }
        );
    }

    /// Composition: scoped pid TCP-refused + legacy absent must report
    /// `TcpProbeRefused` (scoped path/port), not degrade to legacy `PidFileAbsent`.
    #[tokio::test]
    async fn detect_for_global_db_preserves_scoped_tcp_refused_over_legacy_absent() {
        let dir = TempDir::new().expect("tempdir");
        let app_home = dir.path();
        let global_db = app_home.join("global/memory.db");
        std::fs::create_dir_all(global_db.parent().expect("global parent")).expect("mkdir");
        std::fs::write(&global_db, b"").expect("touch global db");

        let scoped_pid = crate::daemon_lock::scoped_daemon_pid_path(app_home, &global_db);
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
        let port = listener.local_addr().expect("addr").port();
        drop(listener);

        std::fs::write(
            &scoped_pid,
            serde_json::to_string(&json!({
                "pid": 1,
                "port": port,
                "global_db": global_db.display().to_string(),
            }))
            .expect("json"),
        )
        .expect("write scoped pid");
        // Legacy pid intentionally absent.

        let err = detect_daemon_for_global_db_result(app_home, &global_db)
            .await
            .expect_err("composed probe should fail");
        assert_eq!(
            err,
            DaemonProbeFailure::TcpProbeRefused {
                path: scoped_pid,
                port,
            }
        );
    }
}
