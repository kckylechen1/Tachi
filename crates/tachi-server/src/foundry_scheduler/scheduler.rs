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
        Self::start_inner(manifest_path, foundry_tx, own_global, own_project, true)
    }

    /// Spawn a scheduler that only watches the daemon's own DB scope.
    ///
    /// Agent-global/no-project daemons are intentionally isolated from the
    /// machine-wide manifest so an embedded host such as OpenClaw does not try
    /// to open project DBs under Desktop/Volumes that belong to other agents.
    pub fn start_own_dbs(
        manifest_path: PathBuf,
        foundry_tx: mpsc::Sender<FoundryMaintenanceItem>,
        own_global: PathBuf,
        own_project: Option<PathBuf>,
    ) -> Self {
        Self::start_inner(manifest_path, foundry_tx, own_global, own_project, false)
    }

    fn start_inner(
        manifest_path: PathBuf,
        foundry_tx: mpsc::Sender<FoundryMaintenanceItem>,
        own_global: PathBuf,
        own_project: Option<PathBuf>,
        include_manifest: bool,
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
            reconcile_workers_with_scope(
                &manifest_path_owned,
                include_manifest,
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
                        reconcile_workers_with_scope(
                            &manifest_path_owned,
                            include_manifest,
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

fn reconcile_workers_with_scope(
    manifest_path: &Path,
    include_manifest: bool,
    workers: &Arc<Mutex<BTreeMap<PathBuf, WorkerHandle>>>,
    foundry_tx: &mpsc::Sender<FoundryMaintenanceItem>,
    own_global: &Path,
    own_project: Option<&Path>,
    cancel_root: &tokio_util::sync::CancellationToken,
) {
    let by_path = if include_manifest {
        manifest_worker_targets(manifest_path, own_global, own_project)
    } else {
        own_worker_targets(own_global, own_project)
    };

    reconcile_worker_targets(workers, foundry_tx, by_path, cancel_root);
}

fn manifest_worker_targets(
    manifest_path: &Path,
    own_global: &Path,
    own_project: Option<&Path>,
) -> BTreeMap<PathBuf, (String, Route)> {
    let manifest = match Manifest::load(manifest_path) {
        Ok(m) => m,
        Err(e) => {
            // Manifest may not yet exist on a fresh install; log at debug
            // level (stderr) and try again next tick.
            eprintln!(
                "[foundry-scheduler] manifest load failed ({}): {e}",
                manifest_path.display()
            );
            return BTreeMap::new();
        }
    };

    let mut by_path: BTreeMap<PathBuf, (String, Route)> = BTreeMap::new();
    for entry in &manifest.dbs {
        let path = PathBuf::from(&entry.path);
        // Only schedule against actual SQLite files. Manifest may carry
        // stale paths; skip silently rather than spawning doomed workers.
        match crate::path_utils::manifest_db_leaf_exists(entry) {
            Ok(true) => {}
            Ok(false) => continue,
            Err(error) => {
                eprintln!(
                    "[foundry-scheduler] refusing manifest DB {}: {error}",
                    path.display()
                );
                continue;
            }
        }
        let label = manifest_label_for(&path, &entry.scope_hint);
        let route = classify_route(entry, &path, own_global, own_project);
        by_path.insert(path, (label, route));
    }
    by_path
}

fn own_worker_targets(
    own_global: &Path,
    own_project: Option<&Path>,
) -> BTreeMap<PathBuf, (String, Route)> {
    let mut by_path = BTreeMap::new();
    if own_global.exists() {
        by_path.insert(
            own_global.to_path_buf(),
            ("global".to_string(), Route::Global),
        );
    }
    if let Some(project) = own_project {
        if project.exists() {
            by_path.insert(
                project.to_path_buf(),
                ("project".to_string(), Route::Project),
            );
        }
    }
    by_path
}

fn reconcile_worker_targets(
    workers: &Arc<Mutex<BTreeMap<PathBuf, WorkerHandle>>>,
    foundry_tx: &mpsc::Sender<FoundryMaintenanceItem>,
    by_path: BTreeMap<PathBuf, (String, Route)>,
    cancel_root: &tokio_util::sync::CancellationToken,
) {
    let desired: HashSet<PathBuf> = by_path.keys().cloned().collect();

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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn own_worker_targets_only_include_daemon_dbs() {
        let tmp = tempfile::tempdir().expect("tmp");
        let global = tmp.path().join("agent").join("memory.db");
        let project = tmp.path().join("project").join("memory.db");
        let unrelated = tmp.path().join("unrelated").join("memory.db");
        std::fs::create_dir_all(global.parent().unwrap()).expect("global parent");
        std::fs::create_dir_all(project.parent().unwrap()).expect("project parent");
        std::fs::create_dir_all(unrelated.parent().unwrap()).expect("unrelated parent");
        std::fs::write(&global, b"").expect("global db");
        std::fs::write(&project, b"").expect("project db");
        std::fs::write(&unrelated, b"").expect("unrelated db");

        let targets = own_worker_targets(&global, Some(&project));

        assert_eq!(targets.len(), 2);
        assert!(matches!(
            targets.get(&global).map(|(_, route)| route),
            Some(Route::Global)
        ));
        assert!(matches!(
            targets.get(&project).map(|(_, route)| route),
            Some(Route::Project)
        ));
        assert!(!targets.contains_key(&unrelated));
    }
}
