use rusqlite::{params, Connection};

use crate::db::now_utc_iso;
use crate::error::MemoryError;

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
    let now = now_utc_iso();
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

    let cutoff = (chrono::Utc::now() - chrono::Duration::days(gc_threshold_days))
        .to_rfc3339_opts(chrono::SecondsFormat::Millis, true);
    hist.gc_eligible = count_gc_eligible_foundry_jobs(conn, &cutoff).unwrap_or(0) as usize;

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

fn count_gc_eligible_foundry_jobs(conn: &Connection, cutoff: &str) -> Result<i64, rusqlite::Error> {
    conn.query_row(
        "SELECT COUNT(*) FROM foundry_jobs
         WHERE status IN ('completed','failed','skipped') AND datetime(created_at) < datetime(?1)",
        params![cutoff],
        |row| row.get::<_, i64>(0),
    )
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
                  datetime(created_at) DESC,
                  id DESC",
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
    let cutoff = (chrono::Utc::now() - chrono::Duration::days(days))
        .to_rfc3339_opts(chrono::SecondsFormat::Millis, true);
    delete_gc_eligible_foundry_jobs(conn, &cutoff)
}

fn delete_gc_eligible_foundry_jobs(conn: &Connection, cutoff: &str) -> Result<usize, MemoryError> {
    let deleted = conn.execute(
        "DELETE FROM foundry_jobs WHERE status IN ('completed', 'failed', 'skipped') AND datetime(created_at) < datetime(?1)",
        params![cutoff],
    )?;
    Ok(deleted)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn open_test_db() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute(
            "CREATE TABLE foundry_jobs (
                id TEXT PRIMARY KEY,
                kind TEXT NOT NULL DEFAULT 'thesis_compaction',
                status TEXT NOT NULL,
                memory_ids TEXT NOT NULL DEFAULT '[]',
                metadata TEXT NOT NULL DEFAULT '{}',
                created_at TEXT NOT NULL,
                updated_at TEXT NOT NULL DEFAULT ''
            )",
            [],
        )
        .unwrap();
        conn
    }

    fn insert_job(conn: &Connection, id: &str, status: &str, memory_ids: &str, created_at: &str) {
        conn.execute(
            "INSERT INTO foundry_jobs (id, status, memory_ids, created_at)
             VALUES (?1, ?2, ?3, ?4)",
            params![id, status, memory_ids, created_at],
        )
        .unwrap();
    }

    #[test]
    fn gc_readers_use_datetime_for_mixed_created_cutoff() {
        // tachi#1638: this pins the `created_at < ?` readers with both
        // timestamp forms. A writer-only migration would keep the raw lexical
        // predicate and treat `...100999+00:00` as older than `...100Z`;
        // reader-side datetime() canonicalization keeps that row while still
        // deleting the genuinely older terminal row. No backfill is involved.
        let conn = open_test_db();
        insert_job(
            &conn,
            "same-second-terminal",
            "completed",
            "[]",
            "2026-07-20T12:00:00.100999+00:00",
        );
        insert_job(
            &conn,
            "actually-old-terminal",
            "failed",
            "[]",
            "2026-07-19T12:00:00.999999+00:00",
        );
        insert_job(
            &conn,
            "old-running-excluded",
            "running",
            "[]",
            "2026-07-19T12:00:00.999999+00:00",
        );

        let cutoff = "2026-07-20T12:00:00.100Z";
        assert_eq!(count_gc_eligible_foundry_jobs(&conn, cutoff).unwrap(), 1);
        assert_eq!(delete_gc_eligible_foundry_jobs(&conn, cutoff).unwrap(), 1);

        let remaining = conn
            .prepare("SELECT id FROM foundry_jobs ORDER BY id")
            .unwrap()
            .query_map([], |row| row.get::<_, String>(0))
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap();
        assert_eq!(
            remaining,
            vec!["old-running-excluded", "same-second-terminal"]
        );
    }

    #[test]
    fn find_jobs_for_memory_orders_mixed_created_at_by_datetime_then_id() {
        // tachi#1638: both rows represent the same instant. A writer-only
        // migration would leave `ORDER BY created_at DESC`, which sorts the
        // canonical row first by rendering bytes. The datetime() ordering
        // treats them as equal instants and the deterministic id tie-breaker
        // pins the reader-side fix.
        let conn = open_test_db();
        insert_job(
            &conn,
            "a-canonical",
            "queued",
            r#"["m1"]"#,
            "2026-07-20T12:00:00.000Z",
        );
        insert_job(
            &conn,
            "z-bare",
            "queued",
            r#"["m1"]"#,
            "2026-07-20T12:00:00+00:00",
        );

        let jobs = find_foundry_jobs_for_memory(&conn, "m1").unwrap();
        let ids = jobs.iter().map(|job| job.id.as_str()).collect::<Vec<_>>();
        assert_eq!(ids, vec!["z-bare", "a-canonical"]);
    }
}
