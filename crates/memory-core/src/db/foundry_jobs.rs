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

/// Insert a new Foundry job (or replace if ID already exists).
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
        "INSERT OR REPLACE INTO foundry_jobs
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
    /// Number of terminal-state jobs older than the GC threshold (would be deleted next GC).
    pub gc_eligible: usize,
}

/// Load all jobs with status 'queued' or 'running' (for startup replay).
pub fn load_pending_foundry_jobs(
    conn: &Connection,
) -> Result<Vec<PersistedFoundryJob>, MemoryError> {
    let mut stmt = conn.prepare(
        "SELECT id, kind, lane, status, target_db, named_project, path_prefix, memory_ids,
                target_agent_id, requested_by, evidence_count, goal_count, metadata, created_at
         FROM foundry_jobs
         WHERE status IN ('queued', 'running')
         ORDER BY created_at ASC",
    )?;

    let rows = stmt.query_map([], |row| {
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
}
