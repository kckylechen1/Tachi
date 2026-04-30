//! Multi-DB foundry scheduler (PR-4 reduced scope).
//!
//! ### What this owns
//! The scheduler is a single tokio task spawned by the daemon at startup that
//! periodically rescans `~/.tachi/manifest.json` and, for every DB it lists,
//! runs a per-DB **safety-net poll** every [`POLL_INTERVAL`]. Each poll opens
//! the DB by absolute path, calls [`memory_core::load_pending_foundry_jobs`]
//! to find queued or stuck `in_progress` jobs, and (for DBs the existing
//! single-process foundry worker can route to) re-injects them into the
//! shared `foundry_tx` mpsc channel for execution. For DBs the existing
//! worker does **not** know how to route to (agents/, hub/, vault/, anything
//! outside global + project + named-projects), it counts the orphans into
//! [`SchedulerSnapshot::orphan_jobs_total`] so `tachi status` can surface
//! them as a warning.
//!
//! ### What this does NOT own
//! - Job execution: the existing `run_foundry_maintenance_worker` in
//!   `foundry_runtime_ops::maintenance` keeps that role unchanged.
//! - Channel-driven low-latency enrichment: the existing 500 ms enrichment
//!   batcher → foundry_tx fast path is untouched. Sub-millisecond enqueue
//!   latency is preserved for in-process writes.
//! - Daemon singleton enforcement: that lives in [`crate::daemon_lock`].
//!
//! ### Cadences (per-design, see PR-4 spec)
//! - Manifest re-read: [`MANIFEST_REFRESH_INTERVAL`] (60 s).
//! - Per-DB safety-net poll: [`POLL_INTERVAL`] (30 s).
//!
//! ### Observability
//! [`SchedulerSnapshot`] is read by `tachi status` to render per-DB health,
//! pending-job counts, last-poll timestamp, and orphan warnings.

use std::collections::{BTreeMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde::Serialize;
use tokio::sync::mpsc;
use tokio::time::{interval, Instant, MissedTickBehavior};

use memory_core::{load_pending_foundry_jobs, MemoryStore, PersistedFoundryJob};

use crate::foundry_runtime_ops::FoundryMaintenanceItem;
use crate::manifest::Manifest;
use crate::DbScope;

/// How often each per-DB worker scans `foundry_jobs` for pending work the
/// in-process channel may have missed (cross-process writes, post-crash
/// in_progress jobs, dark DBs).
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
enum Route {
    /// Daemon's own global DB — re-inject as `DbScope::Global`.
    Global,
    /// Daemon's own project DB — re-inject as `DbScope::Project`.
    Project,
    /// Named project under `~/.tachi/projects/<name>/memory.db` — re-inject
    /// with `named_project = Some(name)`.
    NamedProject(String),
    /// Any other manifest DB (agents/, hub/, vault/, dark DBs). Existing
    /// `with_foundry_store` cannot route to it; jobs are counted as
    /// orphans so `tachi status` warns the operator. Full execution for
    /// these paths is deferred to a follow-up PR that refactors
    /// `with_foundry_store` to accept absolute paths.
    Orphan(&'static str),
}

/// Per-DB worker handle. The scheduler holds one per active manifest DB.
struct WorkerHandle {
    db_path: PathBuf,
    label: String,
    route: Route,
    metrics: Arc<WorkerMetrics>,
    cancel: tokio_util::sync::CancellationToken,
    join: tokio::task::JoinHandle<()>,
}

/// Read-only snapshot of scheduler state for `tachi status`.
#[derive(Debug, Clone, Serialize)]
#[allow(dead_code)] // exposed for future in-daemon callers; tachi status reads manifest cross-process
pub struct SchedulerSnapshot {
    pub workers: Vec<WorkerSnapshot>,
    pub orphan_dbs_total: u64,
    pub orphan_jobs_total: u64,
}

#[derive(Debug, Clone, Serialize)]
#[allow(dead_code)]
pub struct WorkerSnapshot {
    pub db_path: String,
    pub label: String,
    pub route: String,
    pub polls_total: u64,
    pub jobs_reinjected_total: u64,
    pub jobs_orphan_total: u64,
    pub last_poll_unix_secs: u64,
    pub last_pending_count: u64,
    pub errors_total: u64,
}

/// Multi-DB foundry scheduler.
///
/// Created once at daemon startup via [`FoundryScheduler::start`]. The
/// returned handle owns the manifest-watcher task and per-DB worker tasks.
/// Drop the handle to shut everything down (workers receive cancellation
/// via their per-task `CancellationToken`).
pub struct FoundryScheduler {
    workers: Arc<Mutex<BTreeMap<PathBuf, WorkerHandle>>>,
    manifest_path: PathBuf,
    foundry_tx: mpsc::Sender<FoundryMaintenanceItem>,
    own_global: PathBuf,
    own_project: Option<PathBuf>,
    cancel_root: tokio_util::sync::CancellationToken,
    _manifest_task: tokio::task::JoinHandle<()>,
}

impl FoundryScheduler {
    /// Spawn the scheduler. `foundry_tx` is the same shared sender used by
    /// the existing in-process enrichment-driven enqueue path; the
    /// scheduler reuses it to feed re-discovered jobs to the existing
    /// worker. `own_global` and `own_project` identify which manifest paths
    /// are this daemon's own scopes (so the scheduler routes them via
    /// `DbScope::Global`/`DbScope::Project` instead of as named projects).
    pub fn start(
        manifest_path: PathBuf,
        foundry_tx: mpsc::Sender<FoundryMaintenanceItem>,
        own_global: PathBuf,
        own_project: Option<PathBuf>,
    ) -> Self {
        let workers: Arc<Mutex<BTreeMap<PathBuf, WorkerHandle>>> =
            Arc::new(Mutex::new(BTreeMap::new()));
        let cancel_root = tokio_util::sync::CancellationToken::new();

        // Manifest-watcher task: reconciles the worker set against
        // disk every MANIFEST_REFRESH_INTERVAL.
        let manifest_workers = workers.clone();
        let manifest_tx = foundry_tx.clone();
        let manifest_global = own_global.clone();
        let manifest_project = own_project.clone();
        let manifest_path_owned = manifest_path.clone();
        let manifest_cancel = cancel_root.clone();
        let manifest_task = tokio::spawn(async move {
            // Run an immediate reconcile before the first tick so workers
            // come up at startup, not 60 s later.
            reconcile_workers(
                &manifest_path_owned,
                &manifest_workers,
                &manifest_tx,
                &manifest_global,
                manifest_project.as_deref(),
                &manifest_cancel,
            );

            let mut tick = interval(MANIFEST_REFRESH_INTERVAL);
            tick.set_missed_tick_behavior(MissedTickBehavior::Skip);
            // First .tick() returns immediately; consume it.
            tick.tick().await;
            loop {
                tokio::select! {
                    _ = manifest_cancel.cancelled() => break,
                    _ = tick.tick() => {
                        reconcile_workers(
                            &manifest_path_owned,
                            &manifest_workers,
                            &manifest_tx,
                            &manifest_global,
                            manifest_project.as_deref(),
                            &manifest_cancel,
                        );
                    }
                }
            }
        });

        Self {
            workers,
            manifest_path,
            foundry_tx,
            own_global,
            own_project,
            cancel_root,
            _manifest_task: manifest_task,
        }
    }

    /// Capture a read-only snapshot for `tachi status`.
    #[allow(dead_code)]
    pub fn snapshot(&self) -> SchedulerSnapshot {
        let map = self.workers.lock().unwrap_or_else(|e| e.into_inner());
        let mut workers = Vec::with_capacity(map.len());
        let mut orphan_dbs_total: u64 = 0;
        let mut orphan_jobs_total: u64 = 0;
        for handle in map.values() {
            let polls = handle.metrics.polls_total.load(Ordering::Relaxed);
            let reinj = handle.metrics.jobs_reinjected_total.load(Ordering::Relaxed);
            let orph = handle.metrics.jobs_orphan_total.load(Ordering::Relaxed);
            let last_poll = handle.metrics.last_poll_unix_secs.load(Ordering::Relaxed);
            let last_pending = handle.metrics.last_pending_count.load(Ordering::Relaxed);
            let errs = handle.metrics.errors_total.load(Ordering::Relaxed);
            let route_str = match &handle.route {
                Route::Global => "global".to_string(),
                Route::Project => "project".to_string(),
                Route::NamedProject(n) => format!("named:{n}"),
                Route::Orphan(reason) => format!("orphan:{reason}"),
            };
            if matches!(handle.route, Route::Orphan(_)) {
                orphan_dbs_total += 1;
                orphan_jobs_total += orph;
            }
            workers.push(WorkerSnapshot {
                db_path: handle.db_path.display().to_string(),
                label: handle.label.clone(),
                route: route_str,
                polls_total: polls,
                jobs_reinjected_total: reinj,
                jobs_orphan_total: orph,
                last_poll_unix_secs: last_poll,
                last_pending_count: last_pending,
                errors_total: errs,
            });
        }
        SchedulerSnapshot {
            workers,
            orphan_dbs_total,
            orphan_jobs_total,
        }
    }

    /// Cancel all workers and the manifest watcher. Idempotent.
    #[allow(dead_code)]
    pub fn shutdown(&self) {
        self.cancel_root.cancel();
        let map = self.workers.lock().unwrap_or_else(|e| e.into_inner());
        for handle in map.values() {
            handle.cancel.cancel();
        }
    }

    /// Return the manifest path this scheduler is watching.
    #[allow(dead_code)]
    pub fn manifest_path(&self) -> &Path {
        &self.manifest_path
    }

    /// Return the foundry sender this scheduler re-injects into.
    #[allow(dead_code)]
    pub fn foundry_tx(&self) -> &mpsc::Sender<FoundryMaintenanceItem> {
        &self.foundry_tx
    }

    /// Return the daemon's own global DB path.
    #[allow(dead_code)]
    pub fn own_global(&self) -> &Path {
        &self.own_global
    }

    /// Return the daemon's own project DB path, if any.
    #[allow(dead_code)]
    pub fn own_project(&self) -> Option<&Path> {
        self.own_project.as_deref()
    }
}

impl Drop for FoundryScheduler {
    fn drop(&mut self) {
        self.shutdown();
    }
}

/// Reconcile in-memory worker set against the manifest on disk.
fn reconcile_workers(
    manifest_path: &Path,
    workers: &Arc<Mutex<BTreeMap<PathBuf, WorkerHandle>>>,
    foundry_tx: &mpsc::Sender<FoundryMaintenanceItem>,
    own_global: &Path,
    own_project: Option<&Path>,
    cancel_root: &tokio_util::sync::CancellationToken,
) {
    let manifest = match Manifest::load(manifest_path) {
        Ok(m) => m,
        Err(e) => {
            // Manifest may not yet exist on a fresh install; log at debug
            // level (stderr) and try again next tick.
            eprintln!(
                "[foundry-scheduler] manifest load failed ({}): {e}",
                manifest_path.display()
            );
            return;
        }
    };

    let mut desired: HashSet<PathBuf> = HashSet::new();
    let mut by_path: BTreeMap<PathBuf, (String, Route)> = BTreeMap::new();
    for entry in &manifest.dbs {
        let path = PathBuf::from(&entry.path);
        // Only schedule against actual SQLite files. Manifest may carry
        // stale paths; skip silently rather than spawning doomed workers.
        if !path.exists() {
            continue;
        }
        let label = manifest_label_for(&path, &entry.scope_hint);
        let route = classify_route(&path, own_global, own_project, &entry.scope_hint);
        desired.insert(path.clone());
        by_path.insert(path, (label, route));
    }

    let mut map = workers.lock().unwrap_or_else(|e| e.into_inner());

    // Remove workers whose DBs are no longer in the manifest.
    let to_remove: Vec<PathBuf> = map
        .keys()
        .filter(|p| !desired.contains(*p))
        .cloned()
        .collect();
    for path in to_remove {
        if let Some(handle) = map.remove(&path) {
            handle.cancel.cancel();
            // The task will exit on its own when it next sees the
            // cancellation; we do not block-await here to keep the
            // reconciler non-blocking.
            handle.join.abort();
        }
    }

    // Spawn workers for newly-listed DBs.
    for (path, (label, route)) in by_path {
        if map.contains_key(&path) {
            continue;
        }
        let metrics = Arc::new(WorkerMetrics::default());
        let cancel = cancel_root.child_token();
        let task_metrics = metrics.clone();
        let task_cancel = cancel.clone();
        let task_path = path.clone();
        let task_label = label.clone();
        let task_route = route.clone();
        let task_tx = foundry_tx.clone();
        let join = tokio::spawn(async move {
            run_db_worker(
                task_path,
                task_label,
                task_route,
                task_tx,
                task_metrics,
                task_cancel,
            )
            .await;
        });
        map.insert(
            path.clone(),
            WorkerHandle {
                db_path: path,
                label,
                route,
                metrics,
                cancel,
                join,
            },
        );
    }
}

/// Per-DB worker task. Wakes every [`POLL_INTERVAL`], opens the DB by
/// absolute path, scans `foundry_jobs` for pending or stuck-in-progress
/// rows, and either re-injects them into the shared `foundry_tx` (for
/// routable DBs) or counts them as orphans (for everything else).
async fn run_db_worker(
    db_path: PathBuf,
    label: String,
    route: Route,
    foundry_tx: mpsc::Sender<FoundryMaintenanceItem>,
    metrics: Arc<WorkerMetrics>,
    cancel: tokio_util::sync::CancellationToken,
) {
    // Stagger startup by a small jitter derived from the path so 30+
    // workers don't all tick at the same wall-clock instant. The jitter
    // is bounded to half the poll interval.
    let jitter_secs = (path_hash(&db_path) % POLL_INTERVAL.as_secs()) as u64;
    tokio::time::sleep(Duration::from_secs(jitter_secs)).await;

    let mut tick = interval_at(Instant::now(), POLL_INTERVAL);
    tick.set_missed_tick_behavior(MissedTickBehavior::Skip);

    loop {
        tokio::select! {
            _ = cancel.cancelled() => break,
            _ = tick.tick() => {
                run_one_poll(&db_path, &label, &route, &foundry_tx, &metrics).await;
            }
        }
    }
}

fn interval_at(start: Instant, period: Duration) -> tokio::time::Interval {
    tokio::time::interval_at(start, period)
}

async fn run_one_poll(
    db_path: &Path,
    label: &str,
    route: &Route,
    foundry_tx: &mpsc::Sender<FoundryMaintenanceItem>,
    metrics: &WorkerMetrics,
) {
    metrics.polls_total.fetch_add(1, Ordering::Relaxed);
    let now_secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    metrics
        .last_poll_unix_secs
        .store(now_secs, Ordering::Relaxed);

    // Open the DB by absolute path. WAL keeps this cheap (a fresh
    // open + drop is microseconds in steady state). We do this on a
    // blocking thread so we never stall the tokio runtime if the DB
    // happens to be locked by a writer.
    let path_owned = db_path.to_path_buf();
    let label_owned = label.to_string();
    let pending: Result<Vec<PersistedFoundryJob>, String> =
        tokio::task::spawn_blocking(move || -> Result<Vec<PersistedFoundryJob>, String> {
            let path_str = path_owned
                .to_str()
                .ok_or_else(|| format!("non-utf8 db path: {}", path_owned.display()))?;
            let store = MemoryStore::open_with_label(path_str, &label_owned)
                .map_err(|e| format!("open {}: {e}", path_owned.display()))?;
            load_pending_foundry_jobs(store.connection()).map_err(|e| format!("load pending: {e}"))
        })
        .await
        .unwrap_or_else(|e| Err(format!("poll join error: {e}")));

    let pending = match pending {
        Ok(v) => v,
        Err(e) => {
            metrics.errors_total.fetch_add(1, Ordering::Relaxed);
            eprintln!("[foundry-scheduler:{label}] poll error: {e}");
            return;
        }
    };

    metrics
        .last_pending_count
        .store(pending.len() as u64, Ordering::Relaxed);

    if pending.is_empty() {
        return;
    }

    match route {
        Route::Global | Route::Project | Route::NamedProject(_) => {
            let mut sent = 0u64;
            for job in pending {
                let target_db = match route {
                    Route::Global => DbScope::Global,
                    Route::Project => DbScope::Project,
                    Route::NamedProject(_) => DbScope::Project, // existing routing path uses named_project for the actual store open
                    Route::Orphan(_) => unreachable!(),
                };
                let named_project = match route {
                    Route::NamedProject(n) => Some(n.clone()),
                    _ => job.named_project.clone(),
                };
                let item = FoundryMaintenanceItem {
                    job: job.spec,
                    target_db,
                    named_project,
                    path_prefix: job.path_prefix,
                    memory_ids: job.memory_ids,
                };
                // try_send is non-blocking; if the channel is full we
                // simply skip this tick — next poll will retry. The
                // dedup gate (try_claim_event) makes that safe.
                if foundry_tx.try_send(item).is_ok() {
                    sent += 1;
                }
            }
            if sent > 0 {
                metrics
                    .jobs_reinjected_total
                    .fetch_add(sent, Ordering::Relaxed);
                eprintln!("[foundry-scheduler:{label}] re-injected {sent} pending job(s)");
            }
        }
        Route::Orphan(reason) => {
            // Existing worker has no route to this DB path; record the
            // orphan count so `tachi status` can warn. Execution is
            // deferred to follow-up work that adds DbScope::Path.
            let n = pending.len() as u64;
            metrics.jobs_orphan_total.fetch_add(n, Ordering::Relaxed);
            eprintln!(
                "[foundry-scheduler:{label}] {n} pending job(s) in non-routable DB ({reason}); see `tachi status`"
            );
        }
    }
}

/// Decide how to route jobs found in `db_path`. The daemon's own global +
/// project DBs route via the existing `with_store_for_scope` path; any DB
/// under `~/.tachi/projects/<name>/memory.db` routes as a named project;
/// everything else is currently treated as orphan.
fn classify_route(
    db_path: &Path,
    own_global: &Path,
    own_project: Option<&Path>,
    scope_hint: &str,
) -> Route {
    if paths_equal(db_path, own_global) {
        return Route::Global;
    }
    if let Some(proj) = own_project {
        if paths_equal(db_path, proj) {
            return Route::Project;
        }
    }
    if let Some(name) = named_project_from_path(db_path) {
        return Route::NamedProject(name);
    }
    // Use scope_hint to give the operator a more readable orphan reason.
    let reason: &'static str = match scope_hint {
        "agent" => "agent_db",
        "foundry" => "foundry_db",
        "vault" => "vault_db",
        "hub" => "hub_db",
        "" => "unscoped",
        _ => "unrouted",
    };
    Route::Orphan(reason)
}

/// Extract a project name from `~/.tachi/projects/<name>/memory.db`.
fn named_project_from_path(db_path: &Path) -> Option<String> {
    // Walk up: parent must be ".../projects/<name>", grandparent ".../projects".
    let parent = db_path.parent()?;
    let name = parent.file_name()?.to_str()?;
    let grand = parent.parent()?;
    let grand_name = grand.file_name()?.to_str()?;
    if grand_name == "projects" && db_path.file_name()?.to_str()? == "memory.db" {
        Some(name.to_string())
    } else {
        None
    }
}

fn paths_equal(a: &Path, b: &Path) -> bool {
    // Manifest paths are pre-canonicalized by the doctor / manifest
    // pipeline; we compare via std::fs::canonicalize when possible to
    // tolerate symlinks but fall back to lexical equality.
    match (std::fs::canonicalize(a), std::fs::canonicalize(b)) {
        (Ok(x), Ok(y)) => x == y,
        _ => a == b,
    }
}

/// Build a short label for logs / status. Prefers manifest-supplied
/// scope_hint, falls back to the parent dir + filename.
fn manifest_label_for(db_path: &Path, scope_hint: &str) -> String {
    if !scope_hint.is_empty() {
        return scope_hint.to_string();
    }
    let file = db_path
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("?.db");
    let parent = db_path
        .parent()
        .and_then(|p| p.file_name())
        .and_then(|s| s.to_str())
        .unwrap_or("?");
    format!("{parent}/{file}")
}

/// Cheap deterministic 64-bit hash of a path. Used only for startup
/// jitter; not security-sensitive.
fn path_hash(p: &Path) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    p.hash(&mut h);
    h.finish()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn classify_route_routes_own_global() {
        let global = PathBuf::from("/tmp/sched-test/global.db");
        let r = classify_route(&global, &global, None, "global");
        assert!(matches!(r, Route::Global));
    }

    #[test]
    fn classify_route_routes_own_project() {
        let global = PathBuf::from("/tmp/sched-test/global.db");
        let project = PathBuf::from("/tmp/sched-test/proj.db");
        let r = classify_route(&project, &global, Some(&project), "project");
        assert!(matches!(r, Route::Project));
    }

    #[test]
    fn classify_route_recognizes_named_project() {
        let global = PathBuf::from("/tmp/sched-test/global.db");
        let np = PathBuf::from("/home/u/.tachi/projects/sigil/memory.db");
        let r = classify_route(&np, &global, None, "");
        match r {
            Route::NamedProject(n) => assert_eq!(n, "sigil"),
            other => panic!("expected NamedProject(sigil), got {other:?}"),
        }
    }

    #[test]
    fn classify_route_orphan_for_agent_db() {
        let global = PathBuf::from("/tmp/sched-test/global.db");
        let agent = PathBuf::from("/home/u/.tachi/agents/main/memory.db");
        let r = classify_route(&agent, &global, None, "agent");
        match r {
            Route::Orphan(reason) => assert_eq!(reason, "agent_db"),
            other => panic!("expected Orphan(agent_db), got {other:?}"),
        }
    }

    #[test]
    fn classify_route_orphan_default_reason_when_no_hint() {
        let global = PathBuf::from("/tmp/sched-test/global.db");
        let weird = PathBuf::from("/somewhere/else/x.db");
        let r = classify_route(&weird, &global, None, "");
        match r {
            Route::Orphan(reason) => assert_eq!(reason, "unscoped"),
            other => panic!("expected Orphan(unscoped), got {other:?}"),
        }
    }

    #[test]
    fn manifest_label_prefers_scope_hint() {
        let p = PathBuf::from("/x/y/z.db");
        assert_eq!(manifest_label_for(&p, "global"), "global");
    }

    #[test]
    fn manifest_label_falls_back_to_parent_filename() {
        let p = PathBuf::from("/x/agents/main/memory.db");
        assert_eq!(manifest_label_for(&p, ""), "main/memory.db");
    }

    #[test]
    fn named_project_extracted_from_canonical_layout() {
        let p = PathBuf::from("/home/u/.tachi/projects/myproj/memory.db");
        assert_eq!(named_project_from_path(&p).as_deref(), Some("myproj"));
    }

    #[test]
    fn named_project_rejects_non_canonical_layout() {
        let p = PathBuf::from("/x/y/notprojects/foo/memory.db");
        assert!(named_project_from_path(&p).is_none());
    }
}
