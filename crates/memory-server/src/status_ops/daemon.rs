//! Daemon singleton detection for `tachi status`: read the PID file + flock
//! peer, and decide whether a running daemon matches this binary and global DB.
//! Extracted from `status_ops::mod` (no behavior change).

use super::{DaemonPidInfo, DaemonStatus};
use crate::daemon_lock::{process_alive, read_pid_file};
use std::path::Path;

pub(crate) fn collect_daemon_status(app_home: &Path, global_db_path: &Path) -> DaemonStatus {
    let scoped_lock = crate::daemon_lock::scoped_daemon_lock_path(app_home, global_db_path);
    let scoped_pid = crate::daemon_lock::scoped_daemon_pid_path(app_home, global_db_path);
    let scoped_status = collect_daemon_status_from_paths(&scoped_lock, &scoped_pid, global_db_path);
    if let Some(status) = scoped_status.as_ref() {
        if matches!(status, DaemonStatus::Running { .. }) {
            return status.clone();
        }
    }

    let lock_path = crate::daemon_lock::legacy_daemon_lock_path(app_home);
    let pid_path = crate::daemon_lock::legacy_daemon_pid_path(app_home);
    collect_daemon_status_from_paths(&lock_path, &pid_path, global_db_path)
        .or(scoped_status)
        .unwrap_or(DaemonStatus::None)
}

fn collect_daemon_status_from_paths(
    lock_path: &Path,
    pid_path: &Path,
    global_db_path: &Path,
) -> Option<DaemonStatus> {
    let pid_info = read_daemon_pid_info_from_path(pid_path);
    match read_pid_file(lock_path) {
        Some(pid) if process_alive(pid) => {
            if let Some(reason) = daemon_mismatch_reason(pid, pid_info.as_ref(), global_db_path) {
                Some(DaemonStatus::Foreign {
                    pid,
                    lock_path: lock_path.to_path_buf(),
                    reason,
                    version: pid_info.as_ref().and_then(|info| info.version.clone()),
                    port: pid_info.as_ref().and_then(|info| info.port),
                    global_db: pid_info.as_ref().and_then(|info| info.global_db.clone()),
                })
            } else {
                Some(DaemonStatus::Running {
                    pid,
                    lock_path: lock_path.to_path_buf(),
                })
            }
        }
        Some(pid) => Some(DaemonStatus::StalePid {
            pid,
            lock_path: lock_path.to_path_buf(),
        }),
        None => None,
    }
}

pub(crate) fn read_daemon_pid_info_from_path(pid_path: &Path) -> Option<DaemonPidInfo> {
    let raw = std::fs::read_to_string(pid_path).ok()?;
    let parsed: serde_json::Value = serde_json::from_str(&raw).ok()?;
    Some(DaemonPidInfo {
        pid: parsed
            .get("pid")
            .and_then(|value| value.as_i64())
            .and_then(|value| i32::try_from(value).ok()),
        port: parsed
            .get("port")
            .and_then(|value| value.as_u64())
            .and_then(|value| u16::try_from(value).ok()),
        version: parsed
            .get("version")
            .and_then(|value| value.as_str())
            .map(str::to_string),
        global_db: parsed
            .get("global_db")
            .and_then(|value| value.as_str())
            .map(str::to_string),
    })
}

pub(crate) fn daemon_mismatch_reason(
    lock_pid: i32,
    pid_info: Option<&DaemonPidInfo>,
    global_db_path: &Path,
) -> Option<String> {
    let Some(info) = pid_info else {
        return Some("daemon.pid missing or unreadable".to_string());
    };
    if let Some(pid) = info.pid {
        if pid != lock_pid {
            return Some(format!(
                "daemon.pid pid={pid} does not match lock pid={lock_pid}"
            ));
        }
    }
    match info.version.as_deref() {
        Some(version) => {
            if version != env!("CARGO_PKG_VERSION") {
                return Some(format!(
                    "daemon version {version} does not match binary {}",
                    env!("CARGO_PKG_VERSION")
                ));
            }
        }
        None => return Some("daemon.pid missing version".to_string()),
    }
    if let Some(daemon_global) = info.global_db.as_deref() {
        if !daemon_global.is_empty() && !path_matches_string(global_db_path, daemon_global) {
            return Some(format!(
                "daemon global_db {} does not match {}",
                daemon_global,
                global_db_path.display()
            ));
        }
    }
    None
}

fn path_matches_string(left: &Path, right: &str) -> bool {
    if left.as_os_str() == right {
        return true;
    }
    std::fs::canonicalize(left)
        .ok()
        .zip(std::fs::canonicalize(right).ok())
        .map(|(left, right)| left == right)
        .unwrap_or(false)
}
