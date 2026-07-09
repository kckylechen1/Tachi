use super::*;

/// Per-DB worker task. Wakes every [`POLL_INTERVAL`], opens the DB by
/// absolute path, scans `foundry_jobs` for queued or stale running
/// rows, and either re-injects them into the shared `foundry_tx` (for
/// routable DBs) or counts them as orphans (for everything else).
pub(super) async fn run_db_worker(
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
    let running_cutoff = (chrono::Utc::now()
        - chrono::Duration::seconds(crate::status_ops::STUCK_THRESHOLD_SECS))
    .to_rfc3339();
    let pending: Result<Vec<PersistedFoundryJob>, String> =
        tokio::task::spawn_blocking(move || -> Result<Vec<PersistedFoundryJob>, String> {
            let path_str = path_owned
                .to_str()
                .ok_or_else(|| format!("non-utf8 db path: {}", path_owned.display()))?;
            let store = MemoryStore::open_with_label(path_str, &label_owned)
                .map_err(|e| format!("open {}: {e}", path_owned.display()))?;
            load_pending_foundry_jobs(store.connection(), &running_cutoff)
                .map_err(|e| format!("load pending: {e}"))
        })
        .await
        .unwrap_or_else(|e| Err(format!("poll join error: {e}")));

    let pending = match pending {
        Ok(v) => v,
        Err(e) => {
            metrics.errors_total.fetch_add(1, Ordering::Relaxed);
            tracing::warn!(target: "tachi::foundry_scheduler", label = %label, error = %e, "foundry scheduler poll error");
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
        Route::Global | Route::Project | Route::NamedProject(_) | Route::Path => {
            let mut sent = 0u64;
            for job in pending {
                let target_db = match route {
                    Route::Global => DbScope::Global,
                    Route::Project => DbScope::Project,
                    Route::NamedProject(_) => DbScope::Project, // existing routing path uses named_project for the actual store open
                    Route::Path => DbScope::Global,
                    Route::Orphan(_) => unreachable!(),
                };
                let named_project = match route {
                    Route::NamedProject(n) => Some(n.clone()),
                    Route::Path => None,
                    _ => job.named_project.clone(),
                };
                let item = FoundryMaintenanceItem {
                    job: job.spec,
                    target_db,
                    named_project,
                    db_path: if matches!(route, Route::Path) {
                        Some(db_path.to_path_buf())
                    } else {
                        None
                    },
                    path_prefix: job.path_prefix,
                    memory_ids: job.memory_ids,
                    counted_queue_slot: false,
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
                tracing::info!(target: "tachi::foundry_scheduler", label = %label, jobs = sent, "re-injected pending foundry job(s)");
            }
        }
        Route::Orphan(reason) => {
            // Existing worker has no route to this DB path; record the
            // orphan count so `tachi status` can warn. Execution is
            // deferred to follow-up work that adds DbScope::Path.
            let n = pending.len() as u64;
            metrics.jobs_orphan_total.fetch_add(n, Ordering::Relaxed);
            tracing::warn!(
                target: "tachi::foundry_scheduler",
                label = %label,
                jobs = n,
                reason = reason,
                "pending foundry job(s) in non-routable DB; see tachi status"
            );
        }
    }
}
