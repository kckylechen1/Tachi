use super::*;

/// Multi-DB foundry scheduler.
///
/// Created once at daemon startup via [`FoundryScheduler::start`]. The
/// returned handle owns the manifest-watcher task and per-DB worker tasks.
/// Drop the handle to shut everything down (workers receive cancellation
/// via their per-task `CancellationToken`).
pub struct FoundryScheduler {
    workers: Arc<Mutex<BTreeMap<PathBuf, WorkerHandle>>>,
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
            cancel_root,
            _manifest_task: manifest_task,
        }
    }

    /// Cancel all workers and the manifest watcher. Idempotent.
    pub fn shutdown(&self) {
        self.cancel_root.cancel();
        let map = self.workers.lock().unwrap_or_else(|e| e.into_inner());
        for handle in map.values() {
            handle.cancel.cancel();
        }
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
        let route = classify_route(entry, &path, own_global, own_project);
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
        map.insert(path.clone(), WorkerHandle { cancel, join });
    }
}
