use super::*;
use std::ffi::OsString;
use std::path::Path;
use tokio::io::{stdin, stdout};

pub(super) async fn serve_stdio(
    server: MemoryServer,
    app_home: PathBuf,
    global_db_path: PathBuf,
    project_db_path: Option<PathBuf>,
) -> Result<(), Box<dyn std::error::Error>> {
    // stdio mode (default) — auto-spawn daemon if not running
    {
        let auto_daemon_disabled = std::env::var("TACHI_DISABLE_AUTO_DAEMON")
            .map(|value| {
                let value = value.trim();
                value == "1"
                    || value.eq_ignore_ascii_case("true")
                    || value.eq_ignore_ascii_case("yes")
            })
            .unwrap_or(false);

        // Version-skew replace: if a daemon is already running but STRICTLY
        // OLDER than this binary, a new-binary child cannot forward to it
        // (version mismatch ⇒ in-process fallback), so multiple OS processes
        // would write the same SQLite file directly and contend (5s
        // SQLITE_BUSY stalls). Replacing the stale daemon keeps a single
        // current-version writer. Safeguards: only when strictly older
        // (never a newer daemon mid-rollout); only the SAME global DB
        // (detect_* guarantees it); SIGTERM is graceful (cleans its pid
        // file); and flock arbitrates the respawn race if several children
        // detect the old daemon at once.
        if !auto_daemon_disabled {
            if let Some(info) =
                crate::cli_client::detect_daemon_for_global_db(&app_home, &global_db_path).await
            {
                if crate::cli_client::daemon_is_older_than_current(&info) {
                    if let Some(pid) = info.pid {
                        eprintln!(
                            "[auto-daemon] replacing stale daemon pid={pid} v{} (< v{})",
                            info.version.as_deref().unwrap_or("?"),
                            env!("CARGO_PKG_VERSION")
                        );
                        #[cfg(unix)]
                        {
                            unsafe { libc::kill(pid as libc::pid_t, libc::SIGTERM) };
                            // Wait (≤3s) for it to release its flock / exit
                            // so our respawn can bind cleanly.
                            for _ in 0..30 {
                                if !crate::daemon_lock::process_alive(pid as i32) {
                                    break;
                                }
                                tokio::time::sleep(Duration::from_millis(100)).await;
                            }
                        }
                    }
                }
            }
        }

        // Re-detect after the possible replacement (the dead daemon's port
        // is closed, so detect's TCP probe returns None even if a stale pid
        // file lingers).
        let daemon_running =
            crate::cli_client::detect_daemon_for_global_db(&app_home, &global_db_path)
                .await
                .is_some();
        if !daemon_running && !auto_daemon_disabled {
            match std::env::current_exe() {
                Ok(exe) => {
                    let port_str = "0".to_string();
                    let daemon_args = auto_daemon_command_args(
                        &global_db_path,
                        project_db_path.as_deref(),
                        &port_str,
                    );
                    match std::process::Command::new(&exe)
                        .args(daemon_args)
                        .stdin(std::process::Stdio::null())
                        .stdout(std::process::Stdio::null())
                        .stderr(std::process::Stdio::null())
                        .spawn()
                    {
                        Ok(mut child) => {
                            eprintln!("[auto-daemon] spawned tachi daemon (pid={})", child.id());
                            // Reap child in background to avoid zombie processes on Unix
                            tokio::spawn(async move {
                                let _ = child.wait();
                            });
                            // Wait for daemon to become ready by polling scoped discovery.
                            let ready_app_home = app_home.clone();
                            let ready_global_db = global_db_path.clone();
                            let ready = tokio::time::timeout(Duration::from_secs(5), async {
                                for _ in 0..25 {
                                    tokio::time::sleep(Duration::from_millis(200)).await;
                                    if crate::cli_client::detect_daemon_for_global_db(
                                        &ready_app_home,
                                        &ready_global_db,
                                    )
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
                                tracing::warn!(
                                    "[auto-daemon] daemon did not become ready within 5s"
                                );
                            }
                        }
                        Err(e) => {
                            eprintln!("[auto-daemon] failed to spawn daemon: {e}");
                        }
                    }
                }
                Err(e) => {
                    eprintln!("[auto-daemon] cannot determine binary path: {e}");
                }
            }
        } else if !daemon_running {
            eprintln!("[auto-daemon] disabled by TACHI_DISABLE_AUTO_DAEMON");
        }
    }

    let transport = (stdin(), stdout());
    let running = rmcp::service::serve_server(server, transport).await?;

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
    Ok(())
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
}
