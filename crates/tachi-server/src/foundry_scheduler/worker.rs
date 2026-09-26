use super::*;

/// Per-DB worker task. Wakes every [`POLL_INTERVAL`], opens the DB by
/// absolute path, scans `foundry_jobs` for queued or stale running
/// rows, and either re-injects them into the shared `foundry_tx` (for
/// routable DBs) or warns about them as orphans (for everything else).
pub(super) async fn run_db_worker(
    db_path: PathBuf,
    label: String,
    route: Route,
    foundry_tx: mpsc::Sender<FoundryMaintenanceItem>,
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
                run_one_poll(&db_path, &label, &route, &foundry_tx).await;
            }
        }
    }
}

fn interval_at(start: Instant, period: Duration) -> tokio::time::Interval {
    tokio::time::interval_at(start, period)
}

/// Open one scheduler-poll store.
///
/// The scheduler's `label` is manifest display text (`manifest_label_for`:
/// the `scope_hint`, or `parent/file` when absent) — inventory naming, never
/// a resolved store identity. Confering it write-opened live project DBs with
/// `project:<name>` role stamps that the bare named-project claims then
/// refused (`StoreRoleConflict`). Open unlabelled instead — the same ruling
/// as `tachi migrate --apply` (#1761): the store's own stamp answers, an
/// unstamped DB stays unstamped, and only a door that actually resolved the
/// role (the named-project doors) confers identity.
fn open_poll_store(path_str: &str) -> Result<MemoryStore, memcore::MemoryError> {
    MemoryStore::open_with_label(path_str, memcore::path_router::UNKNOWN_DB_LABEL)
}

async fn run_one_poll(
    db_path: &Path,
    label: &str,
    route: &Route,
    foundry_tx: &mpsc::Sender<FoundryMaintenanceItem>,
) {
    // Open the DB by absolute path. WAL keeps this cheap (a fresh
    // open + drop is microseconds in steady state). We do this on a
    // blocking thread so we never stall the tokio runtime if the DB
    // happens to be locked by a writer.
    let path_owned = db_path.to_path_buf();
    let running_cutoff = (chrono::Utc::now()
        - chrono::Duration::seconds(crate::status_ops::STUCK_THRESHOLD_SECS))
    .to_rfc3339();
    let pending: Result<Vec<PersistedFoundryJob>, String> =
        tokio::task::spawn_blocking(move || -> Result<Vec<PersistedFoundryJob>, String> {
            let path_str = path_owned
                .to_str()
                .ok_or_else(|| format!("non-utf8 db path: {}", path_owned.display()))?;
            let store = open_poll_store(path_str)
                .map_err(|e| format!("open {}: {e}", path_owned.display()))?;
            load_pending_foundry_jobs(store.connection(), &running_cutoff)
                .map_err(|e| format!("load pending: {e}"))
        })
        .await
        .unwrap_or_else(|e| Err(format!("poll join error: {e}")));

    let pending = match pending {
        Ok(v) => v,
        Err(e) => {
            tracing::warn!(target: "tachi::foundry_scheduler", label = %label, error = %e, "foundry scheduler poll error");
            return;
        }
    };

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
                tracing::info!(target: "tachi::foundry_scheduler", label = %label, jobs = sent, "re-injected pending foundry job(s)");
            }
        }
        Route::Orphan(reason) => {
            // Existing worker has no route to this DB path; warn with the
            // pending count. Execution is deferred to follow-up work that
            // adds DbScope::Path.
            let n = pending.len() as u64;
            tracing::warn!(
                target: "tachi::foundry_scheduler",
                label = %label,
                jobs = n,
                reason = reason,
                "pending foundry job(s) in non-routable DB; execution deferred until DbScope::Path routing lands"
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::OptionalExtension;

    fn role_stamp(path: &Path) -> Option<String> {
        let conn = rusqlite::Connection::open(path).expect("open fixture db");
        conn.query_row(
            "SELECT value_json FROM hard_state WHERE namespace = ?1 AND key = ?2",
            rusqlite::params![
                memcore::db::store_profile::STORE_IDENTITY_NAMESPACE,
                memcore::db::store_profile::STORE_ROLE_KEY
            ],
            |row| row.get::<_, String>(0),
        )
        .optional()
        .expect("read role stamp")
    }

    /// The poll open must confer no store identity: an unstamped DB stays
    /// unstamped. Red on the pre-fix code, where the scope_hint label
    /// (`project:<name>`) became the store's write-once role stamp.
    #[test]
    fn scheduler_poll_open_confers_no_store_role() {
        let tmp = tempfile::tempdir().expect("tmp");
        let db = tmp.path().join("poll-unstamped.db");
        let db_str = db.to_str().expect("utf8 db path");
        drop(MemoryStore::open(db_str).expect("create unstamped fixture db"));
        assert_eq!(role_stamp(&db), None, "fixture must start unstamped");

        drop(open_poll_store(db_str).expect("poll open must succeed"));

        assert_eq!(
            role_stamp(&db),
            None,
            "the scheduler poll must not stamp a store identity"
        );
    }

    /// A DB already carrying the legacy `project:<name>` stamp (stamped by
    /// the pre-fix poll) must keep opening for the poll: conferring nothing
    /// means the store's own stamp answers without conflict.
    #[test]
    fn scheduler_poll_open_reads_a_legacy_scope_stamped_db() {
        let tmp = tempfile::tempdir().expect("tmp");
        let db = tmp.path().join("poll-legacy-stamped.db");
        let db_str = db.to_str().expect("utf8 db path");
        drop(
            MemoryStore::open_with_label(db_str, "project:legacy-poll-proj")
                .expect("stamp fixture db the way the pre-fix poll did"),
        );
        assert!(
            role_stamp(&db).is_some_and(|stamp| stamp.contains("project:legacy-poll-proj")),
            "fixture must carry the legacy scope stamp"
        );

        drop(open_poll_store(db_str).expect("poll open must succeed"));

        assert!(
            role_stamp(&db).is_some_and(|stamp| stamp.contains("project:legacy-poll-proj")),
            "the poll must neither rewrite nor erase the existing stamp"
        );
    }
}
