use super::*;

/// How often each per-DB worker scans `foundry_jobs` for pending work the
/// in-process channel may have missed (cross-process writes, post-crash
/// running jobs, dark DBs).
pub const POLL_INTERVAL: Duration = Duration::from_secs(30);

/// How often the scheduler re-reads the manifest to spawn workers for new
/// DBs and shut down workers for removed ones.
pub const MANIFEST_REFRESH_INTERVAL: Duration = Duration::from_secs(60);

/// Per-DB worker handle. The scheduler holds one per active manifest DB.
pub(super) struct WorkerHandle {
    pub(super) cancel: tokio_util::sync::CancellationToken,
    pub(super) join: tokio::task::JoinHandle<()>,
}
