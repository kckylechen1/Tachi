use rusqlite::{params, Connection};

use crate::error::MemoryError;
use crate::foundry::{FoundryJobKind, FoundryJobStatus, FoundryModelLane};

use super::PersistedFoundryJob;

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
    // Batch all requeue/dead-letter writes into one transaction: a single
    // commit/fsync instead of an implicit transaction per UPDATE.
    let tx = conn.unchecked_transaction()?;
    let mut stmt = tx.prepare(
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
            tx.execute(
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

        tx.execute(
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
    tx.commit()?;
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
            spec: crate::foundry::FoundryJobSpec {
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
