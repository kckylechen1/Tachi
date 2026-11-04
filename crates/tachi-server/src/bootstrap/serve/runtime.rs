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

/// Decision (with the reason attached) for whether the daily distill scheduler
/// spawns in this process — kckylechen1/tachi#1605.
///
/// The gate used to be a bare `has_project_db()`, which silently disabled the
/// scheduler on the global-only owner daemon. That was wrong on the facts:
/// `run_daily_batch_distill` already distills **every** manifest-attached
/// named-project DB under `<tachi_home>/projects/*` and only treats the bound
/// project as an extra target
/// (`foundry_runtime_ops/daily_distill/runner.rs:47-79`), so a daemon with no
/// bound project still has real work. The reason travels with the decision so
/// the caller can log a `DISABLED(reason)` line that names the gate and the
/// remedy instead of refusing in silence.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum DailyDistillGate {
    Enabled {
        /// The daemon has a bound project DB (distilled as target #1).
        bound_project: bool,
        /// Distillable named-project DBs found in the manifest home
        /// (`wiki` excluded — it has its own curation path).
        named_projects: usize,
    },
    Disabled {
        reason: String,
        remedy: String,
    },
}

/// Named-project DBs the distill batch would actually scan, mirroring the
/// runner's own `wiki` skip (`daily_distill/runner.rs:63`).
fn distillable_named_projects(server: &crate::MemoryServer) -> Vec<String> {
    crate::path_utils::list_named_projects_in_home(&server.tachi_home_dir())
        .into_iter()
        .filter(|name| !name.eq_ignore_ascii_case("wiki"))
        .collect()
}

pub(super) fn daily_distill_scheduler_gate(server: &crate::MemoryServer) -> DailyDistillGate {
    let bound_project = server.has_project_db();
    let named_projects = distillable_named_projects(server).len();
    if bound_project || named_projects > 0 {
        return DailyDistillGate::Enabled {
            bound_project,
            named_projects,
        };
    }
    let projects_dir = server.tachi_home_dir().join("projects");
    DailyDistillGate::Disabled {
        reason: format!(
            "gate daily_distill_scheduler_gate: no distillable store — no bound project DB and no named-project DB under {}",
            projects_dir.display()
        ),
        remedy: format!(
            "bind a project DB (serve with --project-db <PATH>, or without --no-project-db from inside a repo) or create a named project store at {}/<name>/{}",
            projects_dir.display(),
            memcore::MEMORY_DB_FILENAME
        ),
    }
}

pub(super) fn daily_distill_marker_path(app_home: &std::path::Path) -> std::path::PathBuf {
    app_home.join("foundry-runs").join(".last_distill_run")
}

/// Native HTTP clients do not spawn a compatibility process to restart the
/// service. The daemon therefore stays resident by default. Operators may opt
/// into idle shutdown with a positive TACHI_DAEMON_IDLE_TIMEOUT_SECS value.
pub(super) fn daemon_idle_timeout() -> Option<std::time::Duration> {
    let secs = std::env::var("TACHI_DAEMON_IDLE_TIMEOUT_SECS")
        .ok()
        .and_then(|v| v.trim().parse::<u64>().ok())
        .unwrap_or(0);
    (secs > 0).then(|| std::time::Duration::from_secs(secs))
}

#[cfg(test)]
mod daemon_idle_tests {
    #[test]
    fn native_http_daemon_stays_resident_unless_idle_shutdown_is_explicit() {
        let _guard = crate::utils::global_test_lock()
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let previous = std::env::var_os("TACHI_DAEMON_IDLE_TIMEOUT_SECS");
        for (raw, seconds) in [
            (None, None),
            (Some("0"), None),
            (Some("bad"), None),
            (Some("120"), Some(120)),
        ] {
            if let Some(raw) = raw {
                std::env::set_var("TACHI_DAEMON_IDLE_TIMEOUT_SECS", raw);
            } else {
                std::env::remove_var("TACHI_DAEMON_IDLE_TIMEOUT_SECS");
            }
            assert_eq!(
                super::daemon_idle_timeout().map(|duration| duration.as_secs()),
                seconds
            );
        }
        if let Some(previous) = previous {
            std::env::set_var("TACHI_DAEMON_IDLE_TIMEOUT_SECS", previous);
        } else {
            std::env::remove_var("TACHI_DAEMON_IDLE_TIMEOUT_SECS");
        }
    }
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

/// Resolves on SIGTERM (Unix). Hosts and the version-skew daemon-replace path
/// send SIGTERM, not SIGINT, so the daemon must catch it for a graceful
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

/// Fields drop in declaration order. The runtime must finish shutting down all
/// async workers and blocking tasks before the singleton ownership is released.
/// Keeping the lock outside the root future also covers early errors and panic
/// unwinding, including workers whose handles are owned by a nested scheduler.
pub(super) struct StartupRuntime {
    pub(super) runtime: tokio::runtime::Runtime,
    pub(super) daemon_lock: Option<crate::daemon_lock::DaemonLock>,
}

impl StartupRuntime {
    pub(super) fn new() -> std::io::Result<Self> {
        Ok(Self {
            runtime: tokio::runtime::Builder::new_multi_thread()
                .enable_all()
                .build()?,
            daemon_lock: None,
        })
    }
}

#[cfg(all(test, unix))]
mod ownership_tests {
    use super::StartupRuntime;
    use crate::daemon_lock::{DaemonLock, DaemonLockError};
    use std::sync::mpsc;
    use std::time::Duration;

    struct NotifyOnDrop(mpsc::Sender<()>);

    impl Drop for NotifyOnDrop {
        fn drop(&mut self) {
            let _ = self.0.send(());
        }
    }

    #[test]
    fn daemon_ownership_outlives_runtime_teardown_and_blocking_workers() {
        let home = tempfile::tempdir().unwrap();
        let lock_path = home.path().join("daemon.lock");
        let worker_lock_path = lock_path.clone();
        let (release_blocking, wait_for_release) = mpsc::channel();
        let (runtime_dropping, observe_drop) = mpsc::channel();
        let thread = std::thread::spawn(move || {
            let mut owner = StartupRuntime::new().unwrap();
            owner.daemon_lock = Some(DaemonLock::acquire(worker_lock_path).unwrap());
            owner.runtime.block_on(async move {
                tokio::task::spawn_blocking(move || {
                    // A timeout keeps a failing assertion from stranding the
                    // test process inside Runtime::drop.
                    let _ = wait_for_release.recv_timeout(Duration::from_secs(30));
                });
                let (ready, started) = tokio::sync::oneshot::channel();
                tokio::spawn(async move {
                    let _drop_notice = NotifyOnDrop(runtime_dropping);
                    let _ = ready.send(());
                    std::future::pending::<()>().await;
                });
                started.await.unwrap();
            });
            drop(owner);
        });
        // Cancellation of the pending task proves that Runtime::drop began;
        // the blocking task keeps that drop in progress until we release it.
        let dropping = observe_drop.recv_timeout(Duration::from_secs(15));
        let concurrent_owner = DaemonLock::acquire(&lock_path);
        let _ = release_blocking.send(());
        thread.join().unwrap();
        dropping.expect("runtime teardown must cancel the pending task");
        assert!(matches!(
            concurrent_owner,
            Err(DaemonLockError::AlreadyRunning { .. })
        ));
        DaemonLock::acquire(&lock_path).expect("ownership releases after runtime teardown");
    }
}
