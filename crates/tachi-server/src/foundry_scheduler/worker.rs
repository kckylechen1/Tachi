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
    let mut ledger = InjectionLedger::new(REINJECT_WINDOW);

    loop {
        tokio::select! {
            _ = cancel.cancelled() => break,
            _ = tick.tick() => {
                run_one_poll(&db_path, &label, &route, &foundry_tx, &metrics, &mut ledger).await;
            }
        }
    }
}

fn interval_at(start: Instant, period: Duration) -> tokio::time::Interval {
    tokio::time::interval_at(start, period)
}

/// How long a job this worker handed to `foundry_tx` is treated as in
/// flight. Within this window an unchanged pending row (same `status` and
/// `updated_at`) is not sent again: it is either still waiting in the channel
/// backlog or being leased right now. After it, the poll sends it once more,
/// as the safety net for an item that was lost. It is the same threshold after
/// which a `running` row counts as stuck.
const REINJECT_WINDOW: Duration =
    Duration::from_secs(crate::status_ops::STUCK_THRESHOLD_SECS.unsigned_abs());

/// Per-worker record of the pending rows this worker already sent to
/// `foundry_tx`, keyed by job id. In-memory only: after a restart every
/// pending row is sent again, which is the crash-recovery path.
pub(super) struct InjectionLedger {
    window: Duration,
    sent: HashMap<String, (PendingFoundryJobMarker, Instant)>,
}

impl InjectionLedger {
    pub(super) fn new(window: Duration) -> Self {
        Self {
            window,
            sent: HashMap::new(),
        }
    }

    /// Whether `marker` was sent in this row state within the window.
    fn recently_sent(&self, marker: &PendingFoundryJobMarker, now: Instant) -> bool {
        self.sent.get(&marker.id).is_some_and(|(sent, at)| {
            sent == marker && now.saturating_duration_since(*at) < self.window
        })
    }

    fn record(&mut self, marker: PendingFoundryJobMarker, now: Instant) {
        self.sent.insert(marker.id.clone(), (marker, now));
    }

    /// Forget every job that is no longer pending, so the ledger stays bounded
    /// by the pending set.
    fn retain_pending(&mut self, pending: &[PendingFoundryJobMarker]) {
        let live: HashSet<&str> = pending.iter().map(|m| m.id.as_str()).collect();
        self.sent.retain(|id, _| live.contains(id.as_str()));
    }
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
///
/// This is the full, write-capable open (process-wide startup lock, schema
/// init). The poll takes it only when [`probe_poll_store`] reports work.
fn open_poll_store(path_str: &str) -> Result<MemoryStore, memcore::MemoryError> {
    MemoryStore::open_with_label(path_str, memcore::path_router::UNKNOWN_DB_LABEL)
}

/// Read-only probe of one scheduler-poll store.
///
/// `MemoryStore::open_read_only` is the validated read door: it refuses a
/// legacy filename and a stamped private store, brackets the open with the
/// physical-identity check, and enforces the schema-version gate, the schema
/// integrity check and the trigger inventory. Its handle is
/// `SQLITE_OPEN_READ_ONLY`, so it never creates, migrates or stamps a DB, and
/// it takes no startup lock. It is unlabelled for the same reason as
/// [`open_poll_store`]: it resolves the store's own stamp and confers none.
fn probe_poll_store(
    path_str: &str,
    running_before: &str,
    now: chrono::DateTime<chrono::Utc>,
) -> Result<FoundryPendingProbe, memcore::MemoryError> {
    let store = MemoryStore::open_read_only(path_str)?;
    probe_pending_foundry_jobs(store.connection(), running_before, now)
}

/// What one poll did, for tests and diagnostics.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum PollOutcome {
    /// The read-only probe found nothing new; no write-capable open.
    ProbeOnly,
    /// The poll took the full open and loaded pending jobs.
    FullOpen,
    /// The poll failed (counted in `errors_total`).
    Error,
}

enum PollRead {
    /// Probe succeeded and nothing needs the full open.
    Idle { pending: usize },
    /// Full open: the loaded jobs plus their row markers read right after.
    Loaded {
        jobs: Vec<PersistedFoundryJob>,
        markers: Vec<PendingFoundryJobMarker>,
    },
}

pub(super) async fn run_one_poll(
    db_path: &Path,
    label: &str,
    route: &Route,
    foundry_tx: &mpsc::Sender<FoundryMaintenanceItem>,
    metrics: &WorkerMetrics,
    ledger: &mut InjectionLedger,
) -> PollOutcome {
    metrics.polls_total.fetch_add(1, Ordering::Relaxed);
    let now_secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    metrics
        .last_poll_unix_secs
        .store(now_secs, Ordering::Relaxed);

    // A full open is not cheap: it takes the process-wide startup lock and
    // runs schema init (a write transaction and several fsyncs) on every
    // call. So the poll first probes read-only, and takes the full open only
    // when there is work: a pending row this worker has not sent within
    // `REINJECT_WINDOW`, or a failed row the retry sweep would act on. A
    // probe that fails (for example on a DB the read door refuses but schema
    // init repairs) falls back to the full open, as every poll did before, as
    // long as the file still exists. Both run on a blocking thread so we
    // never stall the tokio runtime if the DB is locked by a writer.
    let path_owned = db_path.to_path_buf();
    let now = chrono::Utc::now();
    let running_cutoff =
        (now - chrono::Duration::seconds(crate::status_ops::STUCK_THRESHOLD_SECS)).to_rfc3339();
    let probe_cutoff = running_cutoff.clone();
    let probe: Result<Result<FoundryPendingProbe, String>, String> =
        tokio::task::spawn_blocking(move || -> Result<Result<_, String>, String> {
            let path_str = path_owned
                .to_str()
                .ok_or_else(|| format!("non-utf8 db path: {}", path_owned.display()))?;
            Ok(probe_poll_store(path_str, &probe_cutoff, now)
                .map_err(|e| format!("probe {}: {e}", path_owned.display())))
        })
        .await
        .unwrap_or_else(|e| Err(format!("poll join error: {e}")));

    let read = match probe {
        Err(e) => Err(e),
        Ok(Ok(probe)) => {
            ledger.retain_pending(&probe.pending);
            let at = Instant::now();
            let has_work =
                probe.retry_sweep_due || probe.pending.iter().any(|m| !ledger.recently_sent(m, at));
            if has_work {
                load_pending_full(db_path, running_cutoff).await
            } else {
                Ok(PollRead::Idle {
                    pending: probe.pending.len(),
                })
            }
        }
        Ok(Err(probe_error)) => {
            if db_path.is_file() {
                tracing::debug!(target: "tachi::foundry_scheduler", label = %label, error = %probe_error, "foundry scheduler probe failed; using the full open");
                load_pending_full(db_path, running_cutoff).await
            } else {
                Err(probe_error)
            }
        }
    };

    let (jobs, markers) = match read {
        Ok(PollRead::Idle { pending }) => {
            metrics
                .last_pending_count
                .store(pending as u64, Ordering::Relaxed);
            return PollOutcome::ProbeOnly;
        }
        Ok(PollRead::Loaded { jobs, markers }) => (jobs, markers),
        Err(e) => {
            metrics.errors_total.fetch_add(1, Ordering::Relaxed);
            tracing::warn!(target: "tachi::foundry_scheduler", label = %label, error = %e, "foundry scheduler poll error");
            return PollOutcome::Error;
        }
    };

    metrics
        .last_pending_count
        .store(jobs.len() as u64, Ordering::Relaxed);
    ledger.retain_pending(&markers);

    if jobs.is_empty() {
        return PollOutcome::FullOpen;
    }

    match route {
        Route::Global | Route::Project | Route::NamedProject(_) | Route::Path => {
            let at = Instant::now();
            let marker_by_id: HashMap<&str, &PendingFoundryJobMarker> =
                markers.iter().map(|m| (m.id.as_str(), m)).collect();
            let mut sent = 0u64;
            for job in jobs {
                // A row with no marker changed between the load and the marker
                // read: send it and record nothing, so it is never suppressed.
                let marker = marker_by_id.get(job.spec.id.as_str()).copied();
                if marker.is_some_and(|m| ledger.recently_sent(m, at)) {
                    continue;
                }
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
                // simply skip this tick — next poll will retry (nothing is
                // recorded for an unsent job). The dedup gate
                // (try_claim_event) makes a duplicate send safe.
                if foundry_tx.try_send(item).is_ok() {
                    sent += 1;
                    if let Some(marker) = marker {
                        ledger.record(marker.clone(), at);
                    }
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
            let n = jobs.len() as u64;
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
    PollOutcome::FullOpen
}

/// The full-open half of a poll: open write-capable, run
/// `load_pending_foundry_jobs` (which also runs the retry sweep), then read
/// the row markers of what it returned on the same connection.
async fn load_pending_full(db_path: &Path, running_cutoff: String) -> Result<PollRead, String> {
    let path_owned = db_path.to_path_buf();
    tokio::task::spawn_blocking(move || -> Result<PollRead, String> {
        let path_str = path_owned
            .to_str()
            .ok_or_else(|| format!("non-utf8 db path: {}", path_owned.display()))?;
        let store =
            open_poll_store(path_str).map_err(|e| format!("open {}: {e}", path_owned.display()))?;
        let jobs = load_pending_foundry_jobs(store.connection(), &running_cutoff)
            .map_err(|e| format!("load pending: {e}"))?;
        let markers =
            probe_pending_foundry_jobs(store.connection(), &running_cutoff, chrono::Utc::now())
                .map_err(|e| format!("read pending markers: {e}"))?
                .pending;
        Ok(PollRead::Loaded { jobs, markers })
    })
    .await
    .unwrap_or_else(|e| Err(format!("poll join error: {e}")))
}

#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::OptionalExtension;

    fn marker_path(db: &Path) -> PathBuf {
        let mut s = db.as_os_str().to_owned();
        s.push(".migration-marker");
        PathBuf::from(s)
    }

    fn running_cutoff() -> String {
        (chrono::Utc::now() - chrono::Duration::seconds(crate::status_ops::STUCK_THRESHOLD_SECS))
            .to_rfc3339()
    }

    /// An unstamped current store with an empty queue, closed, with the
    /// `.migration-marker` that every full open rewrites removed, so a test can
    /// tell whether a full open happened.
    fn empty_store(dir: &Path, name: &str) -> PathBuf {
        let db = dir.join(name);
        drop(MemoryStore::open(db.to_str().expect("utf8 db path")).expect("create fixture db"));
        std::fs::remove_file(marker_path(&db)).expect("remove migration marker");
        db
    }

    fn insert_job(db: &Path, id: &str, status: &str, updated_secs_ago: i64, metadata: &str) {
        let store = MemoryStore::open(db.to_str().expect("utf8 db path")).expect("open fixture");
        let created = (chrono::Utc::now() - chrono::Duration::seconds(updated_secs_ago + 1))
            .to_rfc3339_opts(chrono::SecondsFormat::Millis, true);
        let updated = (chrono::Utc::now() - chrono::Duration::seconds(updated_secs_ago))
            .to_rfc3339_opts(chrono::SecondsFormat::Millis, true);
        store
            .connection()
            .execute(
                "INSERT INTO foundry_jobs (id, kind, lane, status, created_at, updated_at, metadata)
                 VALUES (?1, 'thesis_compaction', 'fast', ?2, ?3, ?4, ?5)",
                rusqlite::params![id, status, created, updated, metadata],
            )
            .expect("insert fixture job");
        drop(store);
        std::fs::remove_file(marker_path(db)).expect("remove migration marker");
    }

    fn status_of(db: &Path, id: &str) -> String {
        let conn = rusqlite::Connection::open(db).expect("open fixture db");
        conn.query_row(
            "SELECT status FROM foundry_jobs WHERE id = ?1",
            rusqlite::params![id],
            |row| row.get(0),
        )
        .expect("read job status")
    }

    fn drain(rx: &mut mpsc::Receiver<FoundryMaintenanceItem>) -> Vec<String> {
        let mut ids = Vec::new();
        while let Ok(item) = rx.try_recv() {
            ids.push(item.job.id);
        }
        ids
    }

    struct PollRig {
        _tmp: tempfile::TempDir,
        db: PathBuf,
        tx: mpsc::Sender<FoundryMaintenanceItem>,
        rx: mpsc::Receiver<FoundryMaintenanceItem>,
        metrics: WorkerMetrics,
        ledger: InjectionLedger,
    }

    impl PollRig {
        fn new(window: Duration) -> Self {
            let tmp = tempfile::tempdir().expect("tmp");
            let db = empty_store(tmp.path(), "poll.db");
            let (tx, rx) = mpsc::channel(16);
            Self {
                _tmp: tmp,
                db,
                tx,
                rx,
                metrics: WorkerMetrics::default(),
                ledger: InjectionLedger::new(window),
            }
        }

        async fn poll(&mut self) -> PollOutcome {
            run_one_poll(
                &self.db,
                "test",
                &Route::Global,
                &self.tx,
                &self.metrics,
                &mut self.ledger,
            )
            .await
        }
    }

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

    /// The read-only probe confers no identity and writes nothing: no role
    /// stamp, no `.migration-marker` rewrite, and the main file is
    /// byte-identical afterwards.
    #[test]
    fn scheduler_poll_probe_confers_no_store_role_and_writes_nothing() {
        let tmp = tempfile::tempdir().expect("tmp");
        let db = empty_store(tmp.path(), "probe-unstamped.db");
        assert_eq!(role_stamp(&db), None, "fixture must start unstamped");
        let before = std::fs::read(&db).expect("read db bytes");

        let probe = probe_poll_store(
            db.to_str().expect("utf8 db path"),
            &running_cutoff(),
            chrono::Utc::now(),
        )
        .expect("probe must succeed");

        assert_eq!(probe, FoundryPendingProbe::default());
        assert_eq!(
            role_stamp(&db),
            None,
            "the probe must not stamp a store identity"
        );
        assert!(
            !marker_path(&db).exists(),
            "the probe must not run schema init (which rewrites the marker)"
        );
        assert_eq!(std::fs::read(&db).expect("reread db bytes"), before);
    }

    /// Like the poll open, the probe reads a DB carrying the legacy
    /// `project:<name>` stamp without conflict and leaves the stamp alone.
    #[test]
    fn scheduler_poll_probe_reads_a_legacy_scope_stamped_db() {
        let tmp = tempfile::tempdir().expect("tmp");
        let db = tmp.path().join("probe-legacy-stamped.db");
        let db_str = db.to_str().expect("utf8 db path");
        drop(
            MemoryStore::open_with_label(db_str, "project:legacy-poll-proj")
                .expect("stamp fixture db the way the pre-fix poll did"),
        );

        probe_poll_store(db_str, &running_cutoff(), chrono::Utc::now())
            .expect("probe must succeed");

        assert!(
            role_stamp(&db).is_some_and(|stamp| stamp.contains("project:legacy-poll-proj")),
            "the probe must neither rewrite nor erase the existing stamp"
        );
    }

    /// Neither the probe nor the poll creates a DB that is not there.
    #[tokio::test]
    async fn scheduler_poll_never_creates_a_missing_db() {
        let mut rig = PollRig::new(REINJECT_WINDOW);
        let missing = rig.db.with_file_name("missing.db");

        assert!(probe_poll_store(
            missing.to_str().expect("utf8 db path"),
            &running_cutoff(),
            chrono::Utc::now()
        )
        .is_err());
        assert!(!missing.exists(), "the probe must not create the DB");

        rig.db = missing.clone();
        assert_eq!(rig.poll().await, PollOutcome::Error);
        assert!(!missing.exists(), "the poll must not create the DB");
        assert_eq!(rig.metrics.errors_total.load(Ordering::Relaxed), 1);
    }

    /// An empty queue is answered by the probe alone: no full open, so no
    /// startup lock, no schema init and no marker rewrite.
    #[tokio::test]
    async fn empty_queue_poll_takes_no_full_open() {
        let mut rig = PollRig::new(REINJECT_WINDOW);

        assert_eq!(rig.poll().await, PollOutcome::ProbeOnly);
        assert_eq!(rig.poll().await, PollOutcome::ProbeOnly);

        assert!(!marker_path(&rig.db).exists(), "no full open may happen");
        assert!(drain(&mut rig.rx).is_empty());
        assert_eq!(rig.metrics.polls_total.load(Ordering::Relaxed), 2);
        assert_eq!(rig.metrics.errors_total.load(Ordering::Relaxed), 0);
        assert_eq!(rig.metrics.last_pending_count.load(Ordering::Relaxed), 0);
    }

    /// A queued job is sent once, then not again while it is unchanged and
    /// inside the window. The later polls still count it as pending.
    #[tokio::test]
    async fn an_unchanged_queued_job_is_not_resent_within_the_window() {
        let mut rig = PollRig::new(REINJECT_WINDOW);
        insert_job(&rig.db, "q1", "queued", 0, "{}");

        assert_eq!(rig.poll().await, PollOutcome::FullOpen);
        assert_eq!(drain(&mut rig.rx), vec!["q1"]);

        std::fs::remove_file(marker_path(&rig.db)).expect("remove marker after full open");
        assert_eq!(rig.poll().await, PollOutcome::ProbeOnly);
        assert_eq!(rig.poll().await, PollOutcome::ProbeOnly);
        assert!(drain(&mut rig.rx).is_empty(), "no duplicate sends");
        assert!(!marker_path(&rig.db).exists(), "no full open for sent jobs");
        assert_eq!(rig.metrics.last_pending_count.load(Ordering::Relaxed), 1);
        assert_eq!(rig.metrics.jobs_reinjected_total.load(Ordering::Relaxed), 1);
    }

    /// The fallback poll stays a safety net: once the window has passed, a job
    /// that is still pending is sent again.
    #[tokio::test]
    async fn a_still_pending_job_is_resent_after_the_window() {
        let mut rig = PollRig::new(Duration::from_millis(20));
        insert_job(&rig.db, "q1", "queued", 0, "{}");

        assert_eq!(rig.poll().await, PollOutcome::FullOpen);
        assert_eq!(drain(&mut rig.rx), vec!["q1"]);
        tokio::time::sleep(Duration::from_millis(40)).await;

        assert_eq!(rig.poll().await, PollOutcome::FullOpen);
        assert_eq!(drain(&mut rig.rx), vec!["q1"]);
    }

    /// A pending row whose state moved (here `updated_at`, as a retry requeue
    /// or a cross-process write does) is new work and is sent at once.
    #[tokio::test]
    async fn a_changed_pending_row_is_resent_within_the_window() {
        let mut rig = PollRig::new(REINJECT_WINDOW);
        insert_job(&rig.db, "q1", "queued", 0, "{}");
        assert_eq!(rig.poll().await, PollOutcome::FullOpen);
        assert_eq!(drain(&mut rig.rx), vec!["q1"]);

        let conn = rusqlite::Connection::open(&rig.db).expect("open fixture db");
        conn.execute(
            "UPDATE foundry_jobs SET updated_at = ?1 WHERE id = 'q1'",
            rusqlite::params![
                chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Micros, true)
            ],
        )
        .expect("touch job");
        drop(conn);

        assert_eq!(rig.poll().await, PollOutcome::FullOpen);
        assert_eq!(drain(&mut rig.rx), vec!["q1"]);
    }

    /// A stale `running` row (left behind by a crash) is discovered by the
    /// poll and sent.
    #[tokio::test]
    async fn a_stale_running_job_is_sent() {
        let mut rig = PollRig::new(REINJECT_WINDOW);
        let stale = crate::status_ops::STUCK_THRESHOLD_SECS + 60;
        insert_job(&rig.db, "r1", "running", stale, "{}");
        insert_job(&rig.db, "r2", "running", 0, "{}");

        assert_eq!(rig.poll().await, PollOutcome::FullOpen);
        assert_eq!(drain(&mut rig.rx), vec!["r1"]);
    }

    /// A failed job whose backoff has elapsed needs the retry sweep, which
    /// writes, so the poll takes the full open; the sweep requeues the job and
    /// the poll sends it.
    #[tokio::test]
    async fn a_due_retry_sweep_takes_the_full_open() {
        let mut rig = PollRig::new(REINJECT_WINDOW);
        insert_job(&rig.db, "f1", "failed", 3600, "{}");

        assert_eq!(rig.poll().await, PollOutcome::FullOpen);
        assert_eq!(status_of(&rig.db, "f1"), "queued");
        assert_eq!(drain(&mut rig.rx), vec!["f1"]);
    }

    /// A failed job still in backoff needs nothing: probe only, no write.
    #[tokio::test]
    async fn a_failed_job_in_backoff_needs_no_full_open() {
        let mut rig = PollRig::new(REINJECT_WINDOW);
        insert_job(&rig.db, "f1", "failed", 0, "{}");

        assert_eq!(rig.poll().await, PollOutcome::ProbeOnly);
        assert_eq!(status_of(&rig.db, "f1"), "failed");
        assert!(!marker_path(&rig.db).exists());
    }
}
