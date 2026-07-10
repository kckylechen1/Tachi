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

/// True when this process is an embedded stdio MCP facade for a host runtime
/// (OpenClaw, editor plugins, etc.) rather than the canonical owner daemon.
/// In this mode request handling remains available, but owner duties such as
/// background workers and periodic GC/checkpoints stay with the daemon.
pub(super) fn embedded_mcp_facade() -> bool {
    let embedded = std::env::var("TACHI_EMBEDDED_MCP")
        .map(|value| {
            let value = value.trim();
            value == "1" || value.eq_ignore_ascii_case("true") || value.eq_ignore_ascii_case("yes")
        })
        .unwrap_or(false);
    embedded && !crate::cli_client::is_daemon_process()
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

/// Liveness watchdog cadence + consecutive-failure threshold for the HTTP
/// daemon (#936). The watchdog self-probes `/health` through the real socket;
/// after `fails` consecutive failures it logs at ERROR and `exit(2)` so
/// launchd's `KeepAlive` respawns a daemon that can actually serve.
///
/// Returns `None` (watchdog disabled) when `TACHI_DAEMON_WATCHDOG_FAILS=0`.
/// Interval defaults to 30s, clamped to `[5, 3600]`; failures default to 4.
pub(super) fn daemon_watchdog_config() -> Option<(std::time::Duration, u32)> {
    let fails = std::env::var("TACHI_DAEMON_WATCHDOG_FAILS")
        .ok()
        .and_then(|v| v.trim().parse::<u32>().ok())
        .unwrap_or(4);
    if fails == 0 {
        return None;
    }
    let secs = std::env::var("TACHI_DAEMON_WATCHDOG_INTERVAL_SECS")
        .ok()
        .and_then(|v| v.trim().parse::<u64>().ok())
        .unwrap_or(30)
        .clamp(5, 3600);
    Some((std::time::Duration::from_secs(secs), fails))
}

/// Grace window after bind before the watchdog starts counting failures (#936).
/// A slow first startup (cold caches, vector index warm-up) must not trip the
/// watchdog before the surface has had a chance to answer. Failures inside this
/// window are ignored; the first successful probe also arms the counter early.
/// Defaults to 60s; env `TACHI_DAEMON_WATCHDOG_GRACE_SECS`.
pub(super) fn daemon_watchdog_grace() -> std::time::Duration {
    let secs = std::env::var("TACHI_DAEMON_WATCHDOG_GRACE_SECS")
        .ok()
        .and_then(|v| v.trim().parse::<u64>().ok())
        .unwrap_or(60);
    std::time::Duration::from_secs(secs)
}

/// Idle window after which a **direct** stdio MCP server (one that opened the
/// DB for writing) self-terminates. Targets the "host alive but session
/// abandoned" leak mode identified in #520: the dominant pattern is NOT
/// reparent-to-init (parent-death catches that), but long-lived hosts that
/// leave stdio tachi-serve processes connected yet inactive — each holding a
/// write-capable SQLite connection and feeding the lock storm.
///
/// `None` disables (env value `0`). Defaults to 1 hour (longer than the
/// daemon's 30 min because stdio sessions are tied to interactive IDE/agent
/// windows where the user may be away for a meeting).
pub(super) fn stdio_idle_timeout() -> Option<std::time::Duration> {
    let secs = std::env::var("TACHI_STDIO_IDLE_TIMEOUT_SECS")
        .ok()
        .and_then(|v| v.trim().parse::<u64>().ok())
        .unwrap_or(3600);
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
            // SAFETY: getppid() reads the caller's parent pid; it passes no
            // pointers across the FFI boundary and aliases no Rust memory.
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

pub(super) fn trading_hours_kill_guard_enabled() -> bool {
    std::env::var("TACHI_TRADING_HOURS_GUARD")
        .map(|v| matches!(v.trim(), "1" | "true" | "yes"))
        .unwrap_or(false)
}

pub(super) fn is_within_trading_hours() -> bool {
    use chrono::{Datelike, TimeZone, Timelike};
    let cst = chrono::FixedOffset::east_opt(8 * 3600).unwrap();
    let now = cst.from_utc_datetime(&chrono::Utc::now().naive_utc());
    if now.weekday().num_days_from_monday() >= 5 {
        return false;
    }
    let hm = now.hour() * 100 + now.minute();
    (900..1530).contains(&hm)
}
