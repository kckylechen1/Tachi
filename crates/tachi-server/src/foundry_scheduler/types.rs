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

/// Per-DB worker handle. The scheduler holds one per active manifest DB.
pub(super) struct WorkerHandle {
    pub(super) cancel: tokio_util::sync::CancellationToken,
    pub(super) join: tokio::task::JoinHandle<()>,
}
