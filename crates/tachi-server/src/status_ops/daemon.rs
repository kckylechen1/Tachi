//! Daemon singleton detection for `tachi status`: read the PID file + flock
//! peer, and decide whether a running daemon matches this binary and global DB.
//! Extracted from `status_ops::mod` (no behavior change).

use super::{DaemonInventoryEntry, DaemonPidInfo, DaemonStatus};
use crate::daemon_lock::{process_alive, read_pid_file};
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::process::Command;

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

pub(crate) fn collect_daemon_inventory(
    app_home: &Path,
    current_global_db_path: &Path,
) -> Vec<DaemonInventoryEntry> {
    let mut scopes = BTreeSet::new();
    let mut entries = Vec::new();

    let legacy_lock = crate::daemon_lock::legacy_daemon_lock_path(app_home);
    let legacy_pid = crate::daemon_lock::legacy_daemon_pid_path(app_home);
    if legacy_lock.exists() || legacy_pid.exists() {
        entries.push(daemon_inventory_entry(
            "legacy".to_string(),
            legacy_lock,
            legacy_pid,
            current_global_db_path,
        ));
    }

    if let Ok(read_dir) = std::fs::read_dir(app_home) {
        for entry in read_dir.flatten() {
            let name = entry.file_name();
            let name = name.to_string_lossy();
            if let Some(scope) = name
                .strip_prefix("daemon-")
                .and_then(|value| value.strip_suffix(".lock"))
            {
                scopes.insert(scope.to_string());
            } else if let Some(scope) = name
                .strip_prefix("daemon-")
                .and_then(|value| value.strip_suffix(".pid"))
            {
                scopes.insert(scope.to_string());
            }
        }
    }

    for scope in scopes {
        let lock_path = app_home.join(format!("daemon-{scope}.lock"));
        let pid_path = app_home.join(format!("daemon-{scope}.pid"));
        entries.push(daemon_inventory_entry(
            scope,
            lock_path,
            pid_path,
            current_global_db_path,
        ));
    }

    let known_pids = entries
        .iter()
        .filter_map(|entry| entry.pid)
        .collect::<BTreeSet<_>>();
    entries.extend(collect_unregistered_daemon_processes(
        app_home,
        current_global_db_path,
        &known_pids,
    ));

    entries.sort_by(|a, b| {
        b.authoritative_for_current_global
            .cmp(&a.authoritative_for_current_global)
            .then_with(|| b.process_running.cmp(&a.process_running))
            .then_with(|| a.scope.cmp(&b.scope))
    });
    entries
}

fn collect_unregistered_daemon_processes(
    app_home: &Path,
    current_global_db_path: &Path,
    known_pids: &BTreeSet<i32>,
) -> Vec<DaemonInventoryEntry> {
    let Ok(output) = Command::new("ps").args(["-axo", "pid=,command="]).output() else {
        return Vec::new();
    };
    if !output.status.success() {
        return Vec::new();
    }
    let Ok(raw) = String::from_utf8(output.stdout) else {
        return Vec::new();
    };
    raw.lines()
        .filter_map(|line| {
            daemon_inventory_entry_from_process_line(
                app_home,
                current_global_db_path,
                known_pids,
                line,
            )
        })
        .collect()
}

fn daemon_inventory_entry_from_process_line(
    app_home: &Path,
    current_global_db_path: &Path,
    known_pids: &BTreeSet<i32>,
    line: &str,
) -> Option<DaemonInventoryEntry> {
    let trimmed = line.trim();
    let (pid_raw, command) = trimmed.split_once(char::is_whitespace)?;
    let pid = pid_raw.trim().parse::<i32>().ok()?;
    if known_pids.contains(&pid) || !process_alive(pid) {
        return None;
    }
    let command = command.trim();
    if !command.contains("tachi") || !command.contains("--daemon") {
        return None;
    }
    let args = command.split_whitespace().collect::<Vec<_>>();
    let global_db = arg_value(&args, "--global-db")
        .map(str::to_string)
        .unwrap_or_else(|| {
            app_home
                .join("global")
                .join("memory.db")
                .display()
                .to_string()
        });
    let project_db = arg_value(&args, "--project-db").map(str::to_string);
    let port = arg_value(&args, "--port").and_then(|value| value.parse::<u16>().ok());
    let authoritative_for_current_global = path_matches_string(current_global_db_path, &global_db);
    let reason = (!authoritative_for_current_global).then(|| {
        format!(
            "daemon process global_db {global_db} does not match {}",
            current_global_db_path.display()
        )
    });
    Some(DaemonInventoryEntry {
        scope: format!("process:{pid}"),
        pid: Some(pid),
        process_running: true,
        authoritative_for_current_global,
        state: if authoritative_for_current_global {
            "running".to_string()
        } else {
            "foreign".to_string()
        },
        reason,
        lock_path: String::new(),
        pid_path: String::new(),
        version: None,
        port,
        global_db: Some(global_db),
        project_db,
    })
}

fn arg_value<'a>(args: &'a [&str], flag: &str) -> Option<&'a str> {
    args.windows(2)
        .find_map(|pair| (pair[0] == flag).then_some(pair[1]))
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
        project_db: parsed
            .get("project_db")
            .and_then(|value| value.as_str())
            .map(str::to_string),
    })
}

fn daemon_inventory_entry(
    scope: String,
    lock_path: PathBuf,
    pid_path: PathBuf,
    current_global_db_path: &Path,
) -> DaemonInventoryEntry {
    let pid_info = read_daemon_pid_info_from_path(&pid_path);
    let lock_pid = read_pid_file(&lock_path);
    let pid = lock_pid.or_else(|| pid_info.as_ref().and_then(|info| info.pid));
    let process_running = pid.map(process_alive).unwrap_or(false);
    let reason = if process_running {
        match lock_pid {
            Some(lock_pid) => {
                daemon_mismatch_reason(lock_pid, pid_info.as_ref(), current_global_db_path)
            }
            None => Some("daemon lock pid missing or unreadable".to_string()),
        }
    } else {
        None
    };
    let authoritative_for_current_global = process_running && reason.is_none();
    let state = if authoritative_for_current_global {
        "running"
    } else if process_running {
        "foreign"
    } else if pid.is_some() {
        "stale"
    } else {
        "none"
    };

    DaemonInventoryEntry {
        scope,
        pid,
        process_running,
        authoritative_for_current_global,
        state: state.to_string(),
        reason,
        lock_path: lock_path.display().to_string(),
        pid_path: pid_path.display().to_string(),
        version: pid_info.as_ref().and_then(|info| info.version.clone()),
        port: pid_info.as_ref().and_then(|info| info.port),
        global_db: pid_info.as_ref().and_then(|info| info.global_db.clone()),
        project_db: pid_info.and_then(|info| info.project_db),
    }
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
