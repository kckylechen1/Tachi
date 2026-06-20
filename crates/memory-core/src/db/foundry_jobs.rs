use rusqlite::{params, Connection};
use serde_json;

use crate::error::MemoryError;
use crate::foundry::{FoundryJobKind, FoundryJobSpec, FoundryJobStatus, FoundryModelLane};

/// A persisted Foundry job row including dispatch context.
#[derive(Debug, Clone)]
pub struct PersistedFoundryJob {
    pub spec: FoundryJobSpec,
    pub target_db: String,
    pub named_project: Option<String>,
    pub path_prefix: String,
    pub memory_ids: Vec<String>,
}

/// Insert a new Foundry job.
///
/// Existing rows are intentionally left alone. Re-inserting the same job id must
/// not resurrect a terminal job back to `queued`.
pub fn insert_foundry_job(conn: &Connection, job: &PersistedFoundryJob) -> Result<(), MemoryError> {
    let now = chrono::Utc::now().to_rfc3339();
    let kind_str = serde_json::to_string(&job.spec.kind)
        .unwrap_or_default()
        .trim_matches('"')
        .to_string();
    let lane_str = serde_json::to_string(&job.spec.lane)
        .unwrap_or_default()
        .trim_matches('"')
        .to_string();
    let status_str = serde_json::to_string(&job.spec.status)
        .unwrap_or_default()
        .trim_matches('"')
        .to_string();

    conn.execute(
        "INSERT OR IGNORE INTO foundry_jobs
         (id, kind, lane, status, target_db, named_project, path_prefix, memory_ids,
          target_agent_id, requested_by, evidence_count, goal_count, metadata,
          created_at, updated_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15)",
        params![
            job.spec.id,
            kind_str,
            lane_str,
            status_str,
            job.target_db,
            job.named_project,
            job.path_prefix,
            serde_json::to_string(&job.memory_ids).unwrap_or_else(|_| "[]".to_string()),
            job.spec.target_agent_id,
            job.spec.requested_by,
            job.spec.evidence_count as i64,
            job.spec.goal_count as i64,
            job.spec.metadata.to_string(),
            if job.spec.created_at.is_empty() {
                &now
            } else {
                &job.spec.created_at
            },
            now,
        ],
    )?;
    Ok(())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FoundryJobLease {
    Queued,
    StaleRunning,
}

/// Atomically lease a queued or stale-running job for execution.
///
/// Returns `None` when another worker has already leased this job, or when the
/// row is already terminal. `running_before` is the same cutoff used by the
/// scheduler for crash recovery.
pub fn claim_foundry_job_for_run(
    conn: &Connection,
    id: &str,
    running_before: &str,
) -> Result<Option<FoundryJobLease>, MemoryError> {
    let now = chrono::Utc::now().to_rfc3339();
    let changed = conn.execute(
        "UPDATE foundry_jobs
         SET status = 'running', updated_at = ?1
         WHERE id = ?2
           AND status = 'queued'",
        params![now, id],
    )?;
    if changed > 0 {
        return Ok(Some(FoundryJobLease::Queued));
    }

    let changed = conn.execute(
        "UPDATE foundry_jobs
         SET status = 'running', updated_at = ?1
         WHERE id = ?2
           AND status = 'running'
           AND updated_at < ?3",
        params![now, id, running_before],
    )?;
    if changed > 0 {
        return Ok(Some(FoundryJobLease::StaleRunning));
    }

    Ok(None)
}

/// Branch #5: update job status AND record a structured reason for the
/// transition (skip-reason, fail-reason, abort-reason). The reason is stored
/// as `terminal_reason` inside the existing `metadata` JSON column, avoiding
/// a schema migration. Safe on legacy DBs (no schema change required).
pub fn update_foundry_job_status_with_reason(
    conn: &Connection,
    id: &str,
    status: &str,
    reason: Option<&str>,
) -> Result<(), MemoryError> {
    let now = chrono::Utc::now().to_rfc3339();
    if let Some(reason) = reason {
        let terminal_reason = serde_json::json!({
            "status": status,
            "reason": reason,
            "at": now,
        })
        .to_string();
        conn.execute(
            "UPDATE foundry_jobs
             SET status = ?1,
                 updated_at = ?2,
                 metadata = json_set(
                     CASE WHEN json_valid(metadata) THEN metadata ELSE '{}' END,
                     '$.terminal_reason',
                     json(?3)
                 )
             WHERE id = ?4",
            params![status, now, terminal_reason, id],
        )?;
    } else {
        conn.execute(
            "UPDATE foundry_jobs SET status = ?1, updated_at = ?2 WHERE id = ?3",
            params![status, now, id],
        )?;
    }
    Ok(())
}

/// Branch #5: a status histogram across all jobs in a single store. Returns
/// counts per status plus the count of jobs in terminal state older than
/// `gc_threshold_days` (which would be removed by the next GC run).
pub fn job_status_histogram(
    conn: &Connection,
    gc_threshold_days: i64,
) -> Result<JobStatusHistogram, MemoryError> {
    let mut stmt = conn.prepare("SELECT status, COUNT(*) FROM foundry_jobs GROUP BY status")?;
    let mut hist = JobStatusHistogram::default();
    let rows = stmt.query_map([], |row| {
        let s: String = row.get(0)?;
        let n: i64 = row.get(1)?;
        Ok((s, n as usize))
    })?;
    for r in rows {
        let r = r?;
        match r.0.as_str() {
            "planned" => hist.planned = r.1,
            "queued" => hist.queued = r.1,
            "running" => hist.running = r.1,
            "completed" => hist.completed = r.1,
            "failed" => hist.failed = r.1,
            "skipped" => hist.skipped = r.1,
            other => hist.other.push((other.to_string(), r.1)),
        }
        hist.total += r.1;
    }

    let cutoff = (chrono::Utc::now() - chrono::Duration::days(gc_threshold_days)).to_rfc3339();
    hist.gc_eligible = conn
        .query_row(
            "SELECT COUNT(*) FROM foundry_jobs
             WHERE status IN ('completed','failed','skipped') AND created_at < ?1",
            params![cutoff],
            |row| row.get::<_, i64>(0),
        )
        .unwrap_or(0) as usize;

    hist.dead_lettered = conn
        .query_row(
            "SELECT COUNT(*) FROM foundry_jobs
             WHERE status = 'failed'
               AND COALESCE(json_extract(metadata, '$.dead_letter'), 0) != 0",
            [],
            |row| row.get::<_, i64>(0),
        )
        .unwrap_or(0) as usize;

    Ok(hist)
}

#[derive(Debug, Clone, Default, serde::Serialize)]
pub struct JobStatusHistogram {
    pub total: usize,
    pub planned: usize,
    pub queued: usize,
    pub running: usize,
    pub completed: usize,
    pub failed: usize,
    pub skipped: usize,
    pub other: Vec<(String, usize)>,
    /// Failed jobs that have exhausted their retries (dead-lettered). These are
    /// the only failures the health score treats as genuine/current.
    pub dead_lettered: usize,
    /// Number of terminal-state jobs older than the GC threshold (would be deleted next GC).
    pub gc_eligible: usize,
}

/// Retry policy for failed Foundry jobs.
///
/// A failed job is re-queued up to `max_attempts` times once an exponential
/// backoff (measured from the last failure) has elapsed. After the attempts are
/// exhausted it is parked in the **dead-letter** state: it stops being retried
/// and is what the health score treats as a genuine, current failure.
#[derive(Debug, Clone, Copy)]
pub struct FoundryRetryPolicy {
    pub max_attempts: u32,
    pub base_backoff_secs: i64,
    pub max_backoff_secs: i64,
}

impl Default for FoundryRetryPolicy {
    fn default() -> Self {
        Self {
            max_attempts: 4,
            base_backoff_secs: 300,     // 5 min
            max_backoff_secs: 6 * 3600, // 6 h
        }
    }
}

impl FoundryRetryPolicy {
    /// Backoff before the next retry given how many attempts have already been
    /// made (exponential with `base * 2^attempts`, capped at `max_backoff_secs`).
    fn backoff_secs(&self, attempts: u32) -> i64 {
        let factor = 1i64.checked_shl(attempts.min(20)).unwrap_or(i64::MAX);
        self.base_backoff_secs
            .saturating_mul(factor)
            .min(self.max_backoff_secs)
    }
}

/// Result of a retry sweep.
#[derive(Debug, Clone, Copy, Default, serde::Serialize)]
pub struct RequeueOutcome {
    pub requeued: usize,
    pub dead_lettered: usize,
}

/// Re-queue failed jobs whose backoff has elapsed and dead-letter the ones that
/// have exhausted their attempts.
///
/// All retry logic lives here so the many failure sites can keep simply marking
/// jobs `failed`; `attempts` / `dead_letter` are tracked in the existing
/// `metadata` JSON column (no schema migration). Idempotent and safe to call
/// before any replay.
pub fn requeue_retryable_foundry_jobs(
    conn: &Connection,
    policy: &FoundryRetryPolicy,
    now: chrono::DateTime<chrono::Utc>,
) -> Result<RequeueOutcome, MemoryError> {
    let mut stmt = conn.prepare(
        "SELECT id,
                COALESCE(json_extract(metadata, '$.attempts'), 0) AS attempts,
                updated_at
         FROM foundry_jobs
         WHERE status = 'failed'
           AND COALESCE(json_extract(metadata, '$.dead_letter'), 0) = 0",
    )?;
    let rows = stmt
        .query_map([], |row| {
            let id: String = row.get(0)?;
            let attempts: i64 = row.get(1)?;
            let updated_at: String = row.get(2)?;
            Ok((id, attempts.max(0) as u32, updated_at))
        })?
        .collect::<Result<Vec<_>, _>>()?;
    drop(stmt);

    let now_str = now.to_rfc3339();
    let mut outcome = RequeueOutcome::default();
    for (id, attempts, updated_at) in rows {
        if attempts >= policy.max_attempts {
            conn.execute(
                "UPDATE foundry_jobs
                 SET updated_at = ?1,
                     metadata = json_set(
                         CASE WHEN json_valid(metadata) THEN metadata ELSE '{}' END,
                         '$.dead_letter', 1)
                 WHERE id = ?2 AND status = 'failed'",
                params![now_str, id],
            )?;
            outcome.dead_lettered += 1;
            continue;
        }

        let ready = chrono::DateTime::parse_from_rfc3339(&updated_at)
            .map(|last| {
                (now - last.with_timezone(&chrono::Utc)).num_seconds()
                    >= policy.backoff_secs(attempts)
            })
            .unwrap_or(true);
        if !ready {
            continue;
        }

        conn.execute(
            "UPDATE foundry_jobs
             SET status = 'queued',
                 updated_at = ?1,
                 metadata = json_set(
                     CASE WHEN json_valid(metadata) THEN metadata ELSE '{}' END,
                     '$.attempts', ?2)
             WHERE id = ?3 AND status = 'failed'",
            params![now_str, (attempts + 1) as i64, id],
        )?;
        outcome.requeued += 1;
    }
    Ok(outcome)
}

/// Load queued jobs plus stale running jobs for startup/safety-net replay.
///
/// First runs a retry sweep ([`requeue_retryable_foundry_jobs`]) so failed jobs
/// whose backoff has elapsed are recovered — and exhausted ones dead-lettered —
/// before work is loaded.
pub fn load_pending_foundry_jobs(
    conn: &Connection,
    running_before: &str,
) -> Result<Vec<PersistedFoundryJob>, MemoryError> {
    // Best-effort: never block replay if the retry sweep hits an error.
    let _ =
        requeue_retryable_foundry_jobs(conn, &FoundryRetryPolicy::default(), chrono::Utc::now());

    let mut stmt = conn.prepare(
        "SELECT id, kind, lane, status, target_db, named_project, path_prefix, memory_ids,
                target_agent_id, requested_by, evidence_count, goal_count, metadata, created_at
         FROM foundry_jobs
         WHERE status = 'queued'
            OR (status = 'running' AND updated_at < ?1)
         ORDER BY created_at ASC",
    )?;

    let rows = stmt.query_map(params![running_before], |row| {
        let kind_str: String = row.get(1)?;
        let lane_str: String = row.get(2)?;
        let status_str: String = row.get(3)?;
        let memory_ids_str: String = row.get(7)?;
        let metadata_str: String = row.get(12)?;

        Ok(PersistedFoundryJob {
            spec: FoundryJobSpec {
                id: row.get(0)?,
                kind: serde_json::from_str(&format!("\"{}\"", kind_str))
                    .unwrap_or(FoundryJobKind::ForgetSweep),
                lane: serde_json::from_str(&format!("\"{}\"", lane_str))
                    .unwrap_or(FoundryModelLane::Distill),
                status: serde_json::from_str(&format!("\"{}\"", status_str))
                    .unwrap_or(FoundryJobStatus::Queued),
                target_agent_id: row.get(8)?,
                requested_by: row.get(9)?,
                created_at: row.get(13)?,
                evidence_count: row.get::<_, i64>(10).unwrap_or(0) as usize,
                goal_count: row.get::<_, i64>(11).unwrap_or(1) as usize,
                metadata: serde_json::from_str(&metadata_str).unwrap_or(serde_json::json!({})),
            },
            target_db: row.get(4)?,
            named_project: row.get(5)?,
            path_prefix: row.get(6)?,
            memory_ids: serde_json::from_str(&memory_ids_str).unwrap_or_default(),
        })
    })?;

    let mut jobs = Vec::new();
    for job in rows {
        jobs.push(job?);
    }
    Ok(jobs)
}

/// Lightweight per-memory job summary for status surfacing in `get_memory`.
#[derive(Debug, Clone, serde::Serialize)]
pub struct FoundryJobSummary {
    pub id: String,
    pub kind: String,
    pub status: String,
    pub created_at: String,
}

/// Find all Foundry jobs whose `memory_ids` JSON array contains `memory_id`.
///
/// Implemented with a substring `LIKE` over the JSON column. SQLite has no
/// JSON1-array index, but a per-memory call is bounded by the small number of
/// jobs ever queued for one id (typically 0–3). Pending/active jobs first.
pub fn find_foundry_jobs_for_memory(
    conn: &Connection,
    memory_id: &str,
) -> Result<Vec<FoundryJobSummary>, MemoryError> {
    let needle = format!("%\"{}\"%", memory_id.replace('"', "\\\""));
    let mut stmt = conn.prepare(
        "SELECT id, kind, status, created_at
         FROM foundry_jobs
         WHERE memory_ids LIKE ?1
         ORDER BY CASE status
                    WHEN 'running'   THEN 0
                    WHEN 'queued'    THEN 1
                    WHEN 'planned'   THEN 2
                    WHEN 'failed'    THEN 3
                    WHEN 'completed' THEN 4
                    WHEN 'skipped'   THEN 5
                    ELSE 6
                  END,
                  created_at DESC",
    )?;
    let rows = stmt.query_map(params![needle], |row| {
        Ok(FoundryJobSummary {
            id: row.get(0)?,
            kind: row.get(1)?,
            status: row.get(2)?,
            created_at: row.get(3)?,
        })
    })?;
    let mut out = Vec::new();
    for row in rows {
        out.push(row?);
    }
    Ok(out)
}

/// Delete completed/failed/skipped jobs older than `days` days.
pub fn gc_foundry_jobs(conn: &Connection, days: i64) -> Result<usize, MemoryError> {
    let cutoff = (chrono::Utc::now() - chrono::Duration::days(days)).to_rfc3339();
    let deleted = conn.execute(
        "DELETE FROM foundry_jobs WHERE status IN ('completed', 'failed', 'skipped') AND created_at < ?1",
        params![cutoff],
    )?;
    Ok(deleted)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::schema::init_schema;
    use rusqlite::Connection;

    fn open_test_db() -> Connection {
        // Match the convention used by db::tests so init_schema can build FTS5
        // tables that depend on the libsimple tokenizer + sqlite-vec extension.
        let _ = libsimple::enable_auto_extension();
        crate::db::sqlite_vec::register_sqlite_vec();
        let conn = Connection::open_in_memory().unwrap();
        init_schema(&conn).unwrap();
        crate::db::sqlite_vec::try_load_sqlite_vec(&conn);
        conn
    }

    fn insert_minimal_job(conn: &Connection, id: &str, status: &str) {
        let now = chrono::Utc::now().to_rfc3339();
        conn.execute(
            "INSERT INTO foundry_jobs (id, kind, lane, status, created_at, updated_at, metadata)
             VALUES (?1, 'thesis_compaction', 'fast', ?2, ?3, ?3, '{}')",
            params![id, status, now],
        )
        .unwrap();
    }

    fn insert_job_with_metadata(conn: &Connection, id: &str, status: &str, metadata: &str) {
        let now = chrono::Utc::now().to_rfc3339();
        conn.execute(
            "INSERT INTO foundry_jobs (id, kind, lane, status, created_at, updated_at, metadata)
             VALUES (?1, 'thesis_compaction', 'fast', ?2, ?3, ?3, ?4)",
            params![id, status, now, metadata],
        )
        .unwrap();
    }

    fn backdate_updated_at(conn: &Connection, id: &str, secs_ago: i64) {
        let ts = (chrono::Utc::now() - chrono::Duration::seconds(secs_ago)).to_rfc3339();
        conn.execute(
            "UPDATE foundry_jobs SET updated_at = ?1 WHERE id = ?2",
            params![ts, id],
        )
        .unwrap();
    }

    fn status_of(conn: &Connection, id: &str) -> String {
        conn.query_row(
            "SELECT status FROM foundry_jobs WHERE id = ?1",
            params![id],
            |r| r.get(0),
        )
        .unwrap()
    }

    fn metadata_of(conn: &Connection, id: &str) -> serde_json::Value {
        let s: String = conn
            .query_row(
                "SELECT metadata FROM foundry_jobs WHERE id = ?1",
                params![id],
                |r| r.get(0),
            )
            .unwrap();
        serde_json::from_str(&s).unwrap()
    }

    #[test]
    fn update_with_reason_grafts_terminal_reason_into_metadata() {
        let conn = open_test_db();
        insert_job_with_metadata(&conn, "j1", "running", r#"{"existing":true}"#);

        update_foundry_job_status_with_reason(&conn, "j1", "failed", Some("evidence empty"))
            .unwrap();

        let (status, meta): (String, String) = conn
            .query_row(
                "SELECT status, metadata FROM foundry_jobs WHERE id = 'j1'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(status, "failed");
        let v: serde_json::Value = serde_json::from_str(&meta).unwrap();
        assert_eq!(v["existing"], true);
        assert_eq!(v["terminal_reason"]["status"], "failed");
        assert_eq!(v["terminal_reason"]["reason"], "evidence empty");
        assert!(v["terminal_reason"]["at"].is_string());
    }

    #[test]
    fn update_with_reason_none_only_touches_status() {
        let conn = open_test_db();
        insert_minimal_job(&conn, "j2", "running");
        update_foundry_job_status_with_reason(&conn, "j2", "completed", None).unwrap();
        let (status, meta): (String, String) = conn
            .query_row(
                "SELECT status, metadata FROM foundry_jobs WHERE id = 'j2'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(status, "completed");
        // No terminal_reason key written for clean completions.
        let v: serde_json::Value = serde_json::from_str(&meta).unwrap();
        assert!(v.get("terminal_reason").is_none());
    }

    #[test]
    fn histogram_counts_by_status_and_gc_eligibility() {
        let conn = open_test_db();
        insert_minimal_job(&conn, "a", "running");
        insert_minimal_job(&conn, "b", "completed");
        insert_minimal_job(&conn, "c", "failed");
        insert_minimal_job(&conn, "d", "skipped");
        insert_minimal_job(&conn, "e", "queued");

        // Backdate one terminal job so it falls in the GC window (>= 30d).
        let old = (chrono::Utc::now() - chrono::Duration::days(45)).to_rfc3339();
        conn.execute(
            "UPDATE foundry_jobs SET created_at = ?1 WHERE id = 'b'",
            params![old],
        )
        .unwrap();

        let h = job_status_histogram(&conn, 30).unwrap();
        assert_eq!(h.total, 5);
        assert_eq!(h.running, 1);
        assert_eq!(h.completed, 1);
        assert_eq!(h.failed, 1);
        assert_eq!(h.skipped, 1);
        assert_eq!(h.queued, 1);
        assert_eq!(
            h.gc_eligible, 1,
            "only the backdated 'completed' should be GC-eligible"
        );
    }

    #[test]
    fn requeue_recovers_failed_job_after_backoff_and_increments_attempts() {
        let conn = open_test_db();
        insert_minimal_job(&conn, "f1", "failed");
        backdate_updated_at(&conn, "f1", 3600); // well past the 300s base backoff

        let out = requeue_retryable_foundry_jobs(
            &conn,
            &FoundryRetryPolicy::default(),
            chrono::Utc::now(),
        )
        .unwrap();
        assert_eq!(out.requeued, 1);
        assert_eq!(out.dead_lettered, 0);
        assert_eq!(status_of(&conn, "f1"), "queued");
        assert_eq!(metadata_of(&conn, "f1")["attempts"], 1);
    }

    #[test]
    fn requeue_respects_backoff_window() {
        let conn = open_test_db();
        insert_minimal_job(&conn, "f2", "failed"); // updated_at = now, no backoff elapsed

        let out = requeue_retryable_foundry_jobs(
            &conn,
            &FoundryRetryPolicy::default(),
            chrono::Utc::now(),
        )
        .unwrap();
        assert_eq!(out.requeued, 0);
        assert_eq!(status_of(&conn, "f2"), "failed");
    }

    #[test]
    fn requeue_dead_letters_after_max_attempts_then_stops_retrying() {
        let conn = open_test_db();
        let policy = FoundryRetryPolicy {
            max_attempts: 2,
            base_backoff_secs: 1,
            max_backoff_secs: 10,
        };
        insert_job_with_metadata(&conn, "f3", "failed", r#"{"attempts":2}"#);
        backdate_updated_at(&conn, "f3", 3600);

        let out = requeue_retryable_foundry_jobs(&conn, &policy, chrono::Utc::now()).unwrap();
        assert_eq!(out.dead_lettered, 1);
        assert_eq!(out.requeued, 0);
        assert_eq!(status_of(&conn, "f3"), "failed");
        assert_eq!(metadata_of(&conn, "f3")["dead_letter"], 1);

        // A dead-lettered job is excluded from every later sweep.
        let again = requeue_retryable_foundry_jobs(&conn, &policy, chrono::Utc::now()).unwrap();
        assert_eq!(again.requeued, 0);
        assert_eq!(again.dead_lettered, 0);

        let h = job_status_histogram(&conn, 30).unwrap();
        assert_eq!(h.dead_lettered, 1);
        assert_eq!(
            h.failed, 1,
            "dead-lettered jobs still report as failed for display"
        );
    }

    #[test]
    fn load_pending_recovers_backed_off_failed_jobs() {
        let conn = open_test_db();
        insert_minimal_job(&conn, "f4", "failed");
        backdate_updated_at(&conn, "f4", 3600);

        let running_cutoff = (chrono::Utc::now() - chrono::Duration::minutes(10)).to_rfc3339();
        let jobs = load_pending_foundry_jobs(&conn, &running_cutoff).unwrap();
        assert!(
            jobs.iter().any(|j| j.spec.id == "f4"),
            "a backed-off failed job should be recovered to queued and replayed"
        );
    }

    #[test]
    fn gc_only_removes_aged_terminal_jobs() {
        let conn = open_test_db();
        insert_minimal_job(&conn, "fresh-done", "completed");
        insert_minimal_job(&conn, "old-done", "completed");
        insert_minimal_job(&conn, "old-running", "running");

        let old = (chrono::Utc::now() - chrono::Duration::days(45)).to_rfc3339();
        conn.execute(
            "UPDATE foundry_jobs SET created_at = ?1 WHERE id IN ('old-done', 'old-running')",
            params![old],
        )
        .unwrap();

        let removed = gc_foundry_jobs(&conn, 30).unwrap();
        assert_eq!(removed, 1, "only the aged terminal job should be deleted");

        let remaining: i64 = conn
            .query_row("SELECT COUNT(*) FROM foundry_jobs", [], |r| r.get(0))
            .unwrap();
        assert_eq!(remaining, 2);
    }

    #[test]
    fn pending_loader_replays_only_queued_and_stale_running_jobs() {
        let conn = open_test_db();
        insert_minimal_job(&conn, "queued", "queued");
        insert_minimal_job(&conn, "fresh-running", "running");
        insert_minimal_job(&conn, "old-running", "running");
        insert_minimal_job(&conn, "completed", "completed");

        let old = (chrono::Utc::now() - chrono::Duration::minutes(20)).to_rfc3339();
        conn.execute(
            "UPDATE foundry_jobs SET updated_at = ?1 WHERE id = 'old-running'",
            params![old],
        )
        .unwrap();
        let cutoff = (chrono::Utc::now() - chrono::Duration::minutes(10)).to_rfc3339();

        let jobs = load_pending_foundry_jobs(&conn, &cutoff).unwrap();
        let ids = jobs
            .iter()
            .map(|job| job.spec.id.as_str())
            .collect::<Vec<_>>();
        assert_eq!(ids, vec!["queued", "old-running"]);
    }

    #[test]
    fn claim_foundry_job_for_run_leases_only_queued_or_stale_running_jobs() {
        let conn = open_test_db();
        insert_minimal_job(&conn, "queued", "queued");
        insert_minimal_job(&conn, "fresh-running", "running");
        insert_minimal_job(&conn, "old-running", "running");
        insert_minimal_job(&conn, "completed", "completed");

        let old = (chrono::Utc::now() - chrono::Duration::minutes(20)).to_rfc3339();
        conn.execute(
            "UPDATE foundry_jobs SET updated_at = ?1 WHERE id = 'old-running'",
            params![old],
        )
        .unwrap();
        let cutoff = (chrono::Utc::now() - chrono::Duration::minutes(10)).to_rfc3339();

        assert_eq!(
            claim_foundry_job_for_run(&conn, "queued", &cutoff).unwrap(),
            Some(FoundryJobLease::Queued)
        );
        assert_eq!(
            claim_foundry_job_for_run(&conn, "old-running", &cutoff).unwrap(),
            Some(FoundryJobLease::StaleRunning)
        );
        assert_eq!(
            claim_foundry_job_for_run(&conn, "fresh-running", &cutoff).unwrap(),
            None
        );
        assert_eq!(
            claim_foundry_job_for_run(&conn, "completed", &cutoff).unwrap(),
            None
        );
        assert_eq!(
            claim_foundry_job_for_run(&conn, "missing", &cutoff).unwrap(),
            None
        );

        let statuses = ["queued", "old-running", "fresh-running", "completed"]
            .into_iter()
            .map(|id| {
                conn.query_row(
                    "SELECT status FROM foundry_jobs WHERE id = ?1",
                    params![id],
                    |row| row.get::<_, String>(0),
                )
                .unwrap()
            })
            .collect::<Vec<_>>();
        assert_eq!(statuses, vec!["running", "running", "running", "completed"]);
    }

    #[test]
    fn insert_foundry_job_does_not_resurrect_existing_terminal_job() {
        let conn = open_test_db();
        insert_minimal_job(&conn, "j1", "completed");

        let job = PersistedFoundryJob {
            spec: FoundryJobSpec {
                id: "j1".to_string(),
                kind: FoundryJobKind::RecallRerankCache,
                lane: FoundryModelLane::Rerank,
                status: FoundryJobStatus::Queued,
                target_agent_id: Some("agent".to_string()),
                requested_by: Some("test".to_string()),
                created_at: chrono::Utc::now().to_rfc3339(),
                evidence_count: 1,
                goal_count: 1,
                metadata: serde_json::json!({"new": true}),
            },
            target_db: "project".to_string(),
            named_project: None,
            path_prefix: "/x".to_string(),
            memory_ids: vec!["m1".to_string()],
        };

        insert_foundry_job(&conn, &job).unwrap();

        let (status, kind): (String, String) = conn
            .query_row(
                "SELECT status, kind FROM foundry_jobs WHERE id = 'j1'",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!(status, "completed");
        assert_eq!(kind, "thesis_compaction");
    }
}
