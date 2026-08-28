//! Descendant-liveness probe (#894 S2e, ordered enforcement point 5).
//!
//! The lease may be quarantined or reclaimed **only after the worker and every
//! descendant have terminated**. Tearing down (or even re-scanning) a workspace
//! while a background grandchild is still writing would race the very thing the
//! gate exists to detect: the post-image would be taken mid-write, and a
//! "clean" verdict could be produced for a workspace that is still mutating.
//!
//! Dispatch already spawns every worker in its own POSIX process group
//! (`dispatch_ops::subprocess::configure_process_group` → `process_group(0)`,
//! so `pgid == worker pid`), which makes "any descendant alive?" a single
//! `kill(-pgid, 0)` probe rather than a `ps` table walk.
//!
//! A descendant that calls `setsid()` leaves the group and becomes invisible
//! to this probe. Therefore process-group absence alone is never promoted to
//! `ConfirmedReaped`; production runners report it as `Indeterminate` and a
//! required postflight gate fences the lease without scanning or releasing
//! worker-authored artifacts.

/// Can the parent still see a live process from this worker's tree?
pub trait DescendantLiveness: Send + Sync {
    /// `Ok(true)` = at least one process of the worker's tree is still alive.
    /// `Err` = the probe could not answer, which callers MUST treat as
    /// fail-closed (never as "nothing alive").
    fn any_alive(&self) -> Result<bool, String>;

    /// Short description for receipts/logs.
    fn describe(&self) -> String;
}

/// Terminal liveness evidence produced by the runner that owned the worker.
///
/// This type deliberately carries no PID. Once a runner has reaped its group
/// leader, the numeric PID/PGID may be reused and no downstream postflight
/// code may reconstruct ownership or signal through that number.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RunnerLivenessEvidence {
    /// The runner failed or was cancelled before spawning a worker.
    NoWorkerSpawned,
    /// The runner terminated and reaped the worker tree, then proved the
    /// process group absent while it still owned the lifecycle transition.
    ConfirmedReaped { proof: &'static str },
    /// The runner could not prove terminal descendant state. Postflight must
    /// fail closed before scanning the workspace or releasing artifacts.
    Indeterminate { detail: String },
}

impl RunnerLivenessEvidence {
    pub(crate) fn confirmed_contained_reaped() -> Self {
        Self::ConfirmedReaped {
            proof: "kernel_denied_process_group_escape_and_owned_group_absent",
        }
    }

    pub(crate) fn indeterminate(detail: impl Into<String>) -> Self {
        Self::Indeterminate {
            detail: detail.into(),
        }
    }
}

impl DescendantLiveness for RunnerLivenessEvidence {
    fn any_alive(&self) -> Result<bool, String> {
        match self {
            Self::NoWorkerSpawned | Self::ConfirmedReaped { .. } => Ok(false),
            Self::Indeterminate { detail } => Err(format!(
                "runner could not establish terminal descendant liveness: {detail}"
            )),
        }
    }

    fn describe(&self) -> String {
        match self {
            Self::NoWorkerSpawned => "no worker spawned".to_string(),
            Self::ConfirmedReaped { proof } => format!("worker tree reaped ({proof})"),
            Self::Indeterminate { detail } => {
                format!("indeterminate runner liveness ({detail})")
            }
        }
    }
}

/// Liveness of the worker's process group.
#[derive(Debug, Clone, Copy)]
pub struct ProcessGroupLiveness {
    pgid: i32,
}

impl ProcessGroupLiveness {
    /// Build a probe for a worker spawned with `process_group(0)` (so its pid
    /// is its pgid).
    pub fn for_worker_pid(worker_pid: u32) -> Self {
        ProcessGroupLiveness {
            pgid: worker_pid as i32,
        }
    }

    pub fn pgid(&self) -> i32 {
        self.pgid
    }
}

impl DescendantLiveness for ProcessGroupLiveness {
    #[cfg(unix)]
    fn any_alive(&self) -> Result<bool, String> {
        // Guard the two pgids that would make `kill(-pgid, 0)` mean something
        // catastrophically different: 0 (the CALLER's own process group — i.e.
        // the daemon) and 1 (init). A worker can never legitimately have them,
        // so treat them as an unanswerable probe, not as "nothing alive".
        if self.pgid <= 1 {
            return Err(format!(
                "refusing to probe process group {} (0 = the daemon's own group, 1 = init); \
                 the worker pid was never captured, so descendant liveness is UNKNOWN",
                self.pgid
            ));
        }
        // SAFETY: `kill(-pgid, 0)` is an existence probe against a process
        // group — it delivers no signal, passes no pointers across the FFI
        // boundary, and aliases no Rust memory.
        let rc = unsafe { libc::kill(-(self.pgid as libc::pid_t), 0) };
        if rc == 0 {
            return Ok(true);
        }
        let err = std::io::Error::last_os_error();
        match err.raw_os_error() {
            // No process in that group — the worker tree is gone (as long as it
            // was reaped; a zombie still answers to signal 0, which is why the
            // dispatch runner waits on the child before the gate runs).
            Some(code) if code == libc::ESRCH => Ok(false),
            // The group exists but is not ours to signal: it is ALIVE.
            Some(code) if code == libc::EPERM => Ok(true),
            _ => Err(format!(
                "process-group liveness probe for pgid {} failed: {err}",
                self.pgid
            )),
        }
    }

    #[cfg(not(unix))]
    fn any_alive(&self) -> Result<bool, String> {
        Err(
            "descendant-liveness probing is not implemented on this platform; \
             the postflight gate fails closed rather than assuming the worker tree is gone"
                .to_string(),
        )
    }

    fn describe(&self) -> String {
        format!("process group {}", self.pgid)
    }
}

/// A runner failure without a captured worker identity is not evidence that
/// the worker tree was reaped. This probe makes that missing evidence an
/// explicit fail-closed error before the gate can scan or release anything.
#[derive(Debug, Clone)]
pub struct MissingLivenessEvidence {
    reason: String,
}

impl MissingLivenessEvidence {
    pub fn new(reason: impl Into<String>) -> Self {
        Self {
            reason: reason.into(),
        }
    }
}

impl DescendantLiveness for MissingLivenessEvidence {
    fn any_alive(&self) -> Result<bool, String> {
        Err(format!(
            "descendant liveness evidence is unavailable: {}",
            self.reason
        ))
    }

    fn describe(&self) -> String {
        format!("missing descendant liveness evidence ({})", self.reason)
    }
}

/// A probe that reports all descendants reaped (used when the runner has reaped the child process).
#[derive(Debug, Clone, Copy, Default)]
pub struct ReapedLiveness;

impl DescendantLiveness for ReapedLiveness {
    fn any_alive(&self) -> Result<bool, String> {
        Ok(false)
    }

    fn describe(&self) -> String {
        "worker tree reaped".to_string()
    }
}
