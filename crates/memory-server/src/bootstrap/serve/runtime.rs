/// Liveness probe for the manifest self-lock fix: returns true iff some
/// process *other than us* currently holds the file at `db_path` open.
/// Uses `lsof` on Unix (best-effort: missing binary, non-zero exit, or
/// non-Unix platform → returns false, falling back to the prior pid-file
/// check). Critical for the stdio MCP case where the holder never wrote
/// `~/.tachi/daemon.pid`.
pub(super) fn db_path_held_by_other_process(db_path: &str) -> bool {
    #[cfg(unix)]
    {
        use std::process::Command;
        let our_pid = std::process::id().to_string();
        // -t prints PIDs, one per line. -F p would also work but -t is portable.
        let output = Command::new("lsof")
            .arg("-t")
            .arg("--")
            .arg(db_path)
            .output();
        match output {
            Ok(o) if o.status.success() => {
                let stdout = String::from_utf8_lossy(&o.stdout);
                stdout
                    .lines()
                    .any(|line| line.trim() != our_pid && !line.trim().is_empty())
            }
            _ => false,
        }
    }
    #[cfg(not(unix))]
    {
        let _ = db_path;
        false
    }
}

pub(super) fn daily_distill_scheduler_enabled(server: &crate::MemoryServer) -> bool {
    server.has_project_db()
}

pub(super) fn daily_distill_marker_path(app_home: &std::path::Path) -> std::path::PathBuf {
    app_home.join("foundry-runs").join(".last_distill_run")
}

/// Idle window after which a detached daemon self-terminates. `None` disables
/// the reaper (env value `0`). Defaults to 30 minutes.
pub(super) fn daemon_idle_timeout() -> Option<std::time::Duration> {
    let secs = std::env::var("TACHI_DAEMON_IDLE_TIMEOUT_SECS")
        .ok()
        .and_then(|v| v.trim().parse::<u64>().ok())
        .unwrap_or(1800);
    (secs > 0).then(|| std::time::Duration::from_secs(secs))
}

/// Resolves when the launching parent has exited. An orphaned stdio MCP server
/// has no host left to talk to, so it should exit rather than linger as an
/// init/launchd-reparented process. Unix-only; default on, disable with
/// `TACHI_STDIO_PARENT_DEATH_EXIT=0`. On non-Unix (or when the process was
/// already parentless at startup) it never resolves, leaving the other
/// `select!` arms in control.
pub(super) async fn wait_for_parent_death() {
    #[cfg(unix)]
    {
        let disabled = std::env::var("TACHI_STDIO_PARENT_DEATH_EXIT")
            .map(|v| matches!(v.trim(), "0" | "false" | "no" | "off"))
            .unwrap_or(false);
        // SAFETY: getppid() is always safe — it reads the caller's parent pid.
        let original_ppid = unsafe { libc::getppid() };
        if disabled || original_ppid <= 1 {
            // Already parentless (or opted out): nothing to watch.
            std::future::pending::<()>().await;
            return;
        }
        loop {
            tokio::time::sleep(std::time::Duration::from_secs(5)).await;
            // Reparented to init/launchd or a userspace subreaper ⇒ the host died.
            if unsafe { libc::getppid() } != original_ppid {
                return;
            }
        }
    }
    #[cfg(not(unix))]
    {
        std::future::pending::<()>().await;
    }
}

/// Resolves on SIGTERM (Unix). Hosts and the version-skew daemon-replace path
/// send SIGTERM, not SIGINT, so both serve loops must catch it for a graceful
/// shutdown (flush + pid-file cleanup) instead of an abrupt default kill. Never
/// resolves on non-Unix or if the handler can't be installed.
pub(super) async fn sigterm() {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{signal, SignalKind};
        match signal(SignalKind::terminate()) {
            Ok(mut s) => {
                s.recv().await;
            }
            Err(_) => std::future::pending::<()>().await,
        }
    }
    #[cfg(not(unix))]
    {
        std::future::pending::<()>().await;
    }
}
