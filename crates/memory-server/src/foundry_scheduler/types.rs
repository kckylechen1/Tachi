use super::*;

/// How often each per-DB worker scans `foundry_jobs` for pending work the
/// in-process channel may have missed (cross-process writes, post-crash
/// running jobs, dark DBs).
pub const POLL_INTERVAL: Duration = Duration::from_secs(30);

/// How often the scheduler re-reads the manifest to spawn workers for new
/// DBs and shut down workers for removed ones.
pub const MANIFEST_REFRESH_INTERVAL: Duration = Duration::from_secs(60);

/// Per-DB metrics surfaced via [`SchedulerSnapshot`]. Atomic counters are
/// updated lock-free from each worker tick.
#[derive(Debug, Default)]
pub struct WorkerMetrics {
    pub polls_total: AtomicU64,
    pub jobs_reinjected_total: AtomicU64,
    pub jobs_orphan_total: AtomicU64,
    pub last_poll_unix_secs: AtomicU64,
    pub last_pending_count: AtomicU64,
    pub errors_total: AtomicU64,
}

/// Routing classification for a manifest DB. Determines whether the
/// scheduler can re-inject jobs into the existing in-process worker
/// (`Routable`) or must surface them as orphans for operator attention
/// (`Orphan`).
#[derive(Debug, Clone)]
pub(super) enum Route {
    /// Daemon's own global DB — re-inject as `DbScope::Global`.
    Global,
    /// Daemon's own project DB — re-inject as `DbScope::Project`.
    Project,
    /// Named project under `~/.tachi/projects/<name>/memory.db` — re-inject
    /// with `named_project = Some(name)`.
    NamedProject(String),
    /// A manifest DB that must be opened by absolute path (OpenClaw agent DBs,
    /// legacy extension DBs, and any future non-project stores). The worker
    /// already knows the concrete path from the manifest, so it can preserve
    /// isolation while still using the shared maintenance pipeline.
    Path,
    /// Any other manifest DB (agents/, hub/, vault/, dark DBs). Existing
    /// `with_foundry_store` cannot route to it; jobs are counted as
    /// orphans so `tachi status` warns the operator. Full execution for
    /// these paths is deferred to a follow-up PR that refactors
    /// `with_foundry_store` to accept absolute paths.
    Orphan(&'static str),
}

/// Per-DB worker handle. The scheduler holds one per active manifest DB.
pub(super) struct WorkerHandle {
    pub(super) cancel: tokio_util::sync::CancellationToken,
    pub(super) join: tokio::task::JoinHandle<()>,
}
