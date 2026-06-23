use std::path::Path;

use crate::cli::DaemonAction;

pub(crate) async fn run_daemon(
    action: DaemonAction,
    app_home: &Path,
    global_db_path: &Path,
) -> Result<(), Box<dyn std::error::Error>> {
    match action {
        DaemonAction::Status { json: json_out } => {
            let daemon = crate::status_ops::collect_daemon_status(app_home, global_db_path);
            if json_out {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&serde_json::to_value(&daemon)?)?
                );
            } else {
                match daemon {
                    crate::status_ops::DaemonStatus::Running { pid, lock_path } => {
                        println!("[OK] daemon running pid={pid} lock={}", lock_path.display())
                    }
                    crate::status_ops::DaemonStatus::Foreign {
                        pid,
                        lock_path,
                        reason,
                        version,
                        port,
                        global_db,
                    } => {
                        println!(
                            "[!] foreign daemon pid={pid} lock={} reason={reason}",
                            lock_path.display()
                        );
                        println!(
                            "    version={} port={} global_db={}",
                            version.as_deref().unwrap_or("unknown"),
                            port.map(|p| p.to_string())
                                .unwrap_or_else(|| "unknown".to_string()),
                            global_db.as_deref().unwrap_or("unknown")
                        );
                    }
                    crate::status_ops::DaemonStatus::StalePid { pid, lock_path } => {
                        println!(
                            "[!] stale pid file pid={pid} at {} (process not alive)",
                            lock_path.display()
                        )
                    }
                    crate::status_ops::DaemonStatus::None => println!("[OK] no daemon running"),
                }
            }
            Ok(())
        }
        DaemonAction::Kill { force } => {
            match crate::status_ops::collect_daemon_status(app_home, global_db_path) {
                crate::status_ops::DaemonStatus::Running { pid, .. } => {
                    #[cfg(unix)]
                    {
                        let r = unsafe { libc::kill(pid as libc::pid_t, libc::SIGTERM) };
                        if r == 0 {
                            println!("[OK] sent SIGTERM to daemon pid={pid}");
                        } else {
                            let err = std::io::Error::last_os_error();
                            return Err(format!("kill({pid}) failed: {err}").into());
                        }
                    }
                    #[cfg(not(unix))]
                    {
                        return Err("daemon kill is only implemented on unix".into());
                    }
                }
                crate::status_ops::DaemonStatus::StalePid { pid, lock_path } => {
                    if force {
                        let _ = std::fs::remove_file(&lock_path);
                        println!(
                            "[OK] removed stale lock {} (pid {pid} was not alive)",
                            lock_path.display()
                        );
                    } else {
                        println!(
                            "[!] pid {pid} in {} is not alive; rerun with --force to unlink the stale lock",
                            lock_path.display()
                        );
                    }
                }
                crate::status_ops::DaemonStatus::Foreign { reason, .. } => {
                    println!("[!] refusing to kill foreign daemon for current DB scope: {reason}");
                }
                crate::status_ops::DaemonStatus::None => {
                    println!("[OK] no daemon to kill for current DB scope");
                }
            }
            Ok(())
        }
        DaemonAction::Reap { apply, json } => reap_stale_processes(app_home, apply, json),
    }
}

/// True when `pid` names a live process this user can see. `EPERM` means the
/// process exists but is owned by someone else (still "alive").
#[cfg(unix)]
fn process_alive(pid: i64) -> bool {
    if pid <= 0 {
        return false;
    }
    let r = unsafe { libc::kill(pid as libc::pid_t, 0) };
    r == 0 || std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
}

/// Extract the token following `flag` from a ps command line.
fn flag_value(command: &str, flag: &str) -> Option<String> {
    let mut it = command.split_whitespace();
    while let Some(tok) = it.next() {
        if tok == flag {
            return it.next().map(|s| s.to_string());
        }
    }
    None
}

/// Machine-wide sweep for stale tachi processes. Only ever reaps the
/// unambiguously dead: stdio servers whose launching host died (reparented to
/// pid 1) and daemons whose backing global DB no longer exists. Healthy live
/// daemons/servers and this very process are left alone. Dry-run by default.
#[cfg(unix)]
fn reap_stale_processes(
    app_home: &Path,
    apply: bool,
    json_out: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    let self_pid = std::process::id() as i64;
    let out = std::process::Command::new("ps")
        .args(["-ax", "-o", "pid=,ppid=,command="])
        .output()?;
    let text = String::from_utf8_lossy(&out.stdout);

    let mut findings: Vec<serde_json::Value> = Vec::new();
    let mut reaped = 0usize;
    let mut kept = 0usize;

    for line in text.lines() {
        let tokens: Vec<&str> = line.split_whitespace().collect();
        if tokens.len() < 3 {
            continue;
        }
        let (Ok(pid), Ok(ppid)) = (tokens[0].parse::<i64>(), tokens[1].parse::<i64>()) else {
            continue;
        };
        if pid == self_pid {
            continue;
        }
        let command = tokens[2..].join(" ");
        // Only consider processes whose argv[0] basename is a tachi binary.
        let argv0 = tokens[2];
        let base = argv0.rsplit('/').next().unwrap_or(argv0);
        if base != "tachi" && base != "memory-server" {
            continue;
        }

        let is_daemon = command.contains("--daemon");
        let (kind, reap) = if ppid == 1 && !is_daemon {
            ("orphan-stdio", true)
        } else if is_daemon {
            match flag_value(&command, "--global-db") {
                Some(db) if !Path::new(&db).exists() => ("dead-db-daemon", true),
                _ => ("daemon", false),
            }
        } else {
            ("stdio", false)
        };

        if reap {
            reaped += 1;
            if apply {
                unsafe { libc::kill(pid as libc::pid_t, libc::SIGTERM) };
            }
        } else {
            kept += 1;
        }
        findings.push(serde_json::json!({
            "pid": pid,
            "ppid": ppid,
            "kind": kind,
            "reap": reap,
            "command": command.chars().take(120).collect::<String>(),
        }));
    }

    // Stale daemon discovery files (pid recorded but no longer alive).
    let mut stale_files: Vec<String> = Vec::new();
    if let Ok(rd) = std::fs::read_dir(app_home) {
        for ent in rd.flatten() {
            let name = ent.file_name().to_string_lossy().to_string();
            if !(name.starts_with("daemon") && name.ends_with(".pid")) {
                continue;
            }
            let path = ent.path();
            let alive = std::fs::read_to_string(&path)
                .ok()
                .and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok())
                .and_then(|v| v.get("pid").and_then(|p| p.as_i64()))
                .map(process_alive)
                .unwrap_or(false);
            if !alive {
                stale_files.push(name);
                if apply {
                    let _ = std::fs::remove_file(&path);
                    let _ = std::fs::remove_file(path.with_extension("lock"));
                }
            }
        }
    }

    if json_out {
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({
                "applied": apply,
                "reaped": reaped,
                "kept": kept,
                "stale_files": stale_files,
                "processes": findings,
            }))?
        );
        return Ok(());
    }

    let verb = if apply { "reaped" } else { "would reap" };
    println!(
        "tachi process sweep: {verb} {reaped}, kept {kept} healthy{}",
        if apply {
            ""
        } else {
            " (dry-run — pass --apply to act)"
        }
    );
    for f in &findings {
        let mark = if f["reap"].as_bool().unwrap_or(false) {
            "KILL"
        } else {
            "keep"
        };
        println!(
            "  [{mark}] pid={} ppid={} {} :: {}",
            f["pid"],
            f["ppid"],
            f["kind"].as_str().unwrap_or(""),
            f["command"].as_str().unwrap_or("")
        );
    }
    if !stale_files.is_empty() {
        let fverb = if apply { "removed" } else { "stale" };
        println!("  {fverb} lock/pid files: {}", stale_files.join(", "));
    }
    Ok(())
}

#[cfg(not(unix))]
fn reap_stale_processes(
    _app_home: &Path,
    _apply: bool,
    _json_out: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    Err("daemon reap is only implemented on unix".into())
}
