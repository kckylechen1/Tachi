use rusqlite::{params, Connection};
use serde_json;

use super::common::{normalize_utc_iso_or_now, now_utc_iso};
use crate::error::MemoryError;
use crate::foundry::FoundryJobSpec;

mod queue;
mod status;

pub use queue::{
    claim_foundry_job_for_run, load_pending_foundry_jobs, requeue_retryable_foundry_jobs,
    FoundryJobLease, FoundryRetryPolicy, RequeueOutcome,
};
pub use status::{
    find_foundry_jobs_for_memory, gc_foundry_jobs, job_status_histogram,
    update_foundry_job_status_with_reason, FoundryJobSummary, JobStatusHistogram,
};

/// A persisted Foundry job row including dispatch context.
#[derive(Debug, Clone)]
pub struct PersistedFoundryJob {
    pub spec: FoundryJobSpec,
    pub target_db: String,
    pub named_project: Option<String>,
    pub path_prefix: String,
    pub memory_ids: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InsertFoundryJobResult {
    Inserted,
    Existing,
}

/// Insert a new Foundry job.
///
/// Existing rows are intentionally left alone. Re-inserting the same job id must
/// not resurrect a terminal job back to `queued`.
pub fn insert_foundry_job(
    conn: &Connection,
    job: &PersistedFoundryJob,
) -> Result<InsertFoundryJobResult, MemoryError> {
    let now = now_utc_iso();
    let created_at = if job.spec.created_at.is_empty() {
        now.clone()
    } else {
        normalize_utc_iso_or_now(&job.spec.created_at)
    };
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

    let changed = conn.execute(
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
            created_at,
            now,
        ],
    )?;
    Ok(if changed == 1 {
        InsertFoundryJobResult::Inserted
    } else {
        InsertFoundryJobResult::Existing
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::schema::init_schema;
    use crate::foundry::{FoundryJobKind, FoundryJobStatus, FoundryModelLane};
    use rusqlite::Connection;

    fn open_test_db() -> Connection {
        // Match the convention used by db::tests so init_schema can build FTS5
        // tables that depend on the libsimple tokenizer + sqlite-vec extension.
        let _ = crate::db::enable_simple_auto_extension();
        crate::db::sqlite_vec::register_sqlite_vec();
        let conn = Connection::open_in_memory().unwrap();
        init_schema(&conn).unwrap();
        crate::db::sqlite_vec::try_load_sqlite_vec(&conn);
        conn
    }

    fn canonical_ts(dt: chrono::DateTime<chrono::Utc>) -> String {
        dt.to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
    }

    fn insert_job_at(
        conn: &Connection,
        id: &str,
        status: &str,
        created_at: &str,
        updated_at: &str,
    ) {
        conn.execute(
            "INSERT INTO foundry_jobs (id, kind, lane, status, created_at, updated_at, metadata)
             VALUES (?1, 'thesis_compaction', 'fast', ?2, ?3, ?4, '{}')",
            params![id, status, created_at, updated_at],
        )
        .unwrap();
    }

    fn insert_minimal_job(conn: &Connection, id: &str, status: &str) {
        let now = now_utc_iso();
        insert_job_at(conn, id, status, &now, &now);
    }

    fn insert_job_with_metadata(conn: &Connection, id: &str, status: &str, metadata: &str) {
        let now = now_utc_iso();
        conn.execute(
            "INSERT INTO foundry_jobs (id, kind, lane, status, created_at, updated_at, metadata)
             VALUES (?1, 'thesis_compaction', 'fast', ?2, ?3, ?3, ?4)",
            params![id, status, now, metadata],
        )
        .unwrap();
    }

    fn backdate_updated_at(conn: &Connection, id: &str, secs_ago: i64) {
        let ts = canonical_ts(chrono::Utc::now() - chrono::Duration::seconds(secs_ago));
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
        let old = canonical_ts(chrono::Utc::now() - chrono::Duration::days(45));
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

        let running_cutoff = canonical_ts(chrono::Utc::now() - chrono::Duration::minutes(10));
        let jobs = load_pending_foundry_jobs(&conn, &running_cutoff).unwrap();
        assert!(
            jobs.iter().any(|j| j.spec.id == "f4"),
            "a backed-off failed job should be recovered to queued and replayed"
        );
    }

    #[test]
    fn pending_loader_uses_datetime_for_mixed_updated_cutoff_and_created_order() {
        // tachi#1638: this seeds both legacy bare `+00:00` and canonical `Z`
        // forms. A writer-only migration leaves the raw `updated_at < ?` and
        // `ORDER BY created_at` readers in place, so it would wrongly replay
        // the same-second running row and sort the bare row ahead of the
        // canonical row by bytes (`+` before `.`). The datetime() reader wrap
        // is the discriminating fix; there is intentionally no backfill.
        let conn = open_test_db();
        insert_job_at(
            &conn,
            "a-canonical",
            "queued",
            "2026-07-20T12:00:00.000Z",
            "2026-07-20T12:00:00.000Z",
        );
        insert_job_at(
            &conn,
            "z-bare",
            "queued",
            "2026-07-20T12:00:00+00:00",
            "2026-07-20T12:00:00+00:00",
        );
        insert_job_at(
            &conn,
            "same-second-running",
            "running",
            "2026-07-20T11:00:00.000Z",
            "2026-07-20T12:00:00.100999+00:00",
        );

        let jobs = load_pending_foundry_jobs(&conn, "2026-07-20T12:00:00.100Z").unwrap();
        let ids = jobs
            .iter()
            .map(|job| job.spec.id.as_str())
            .collect::<Vec<_>>();
        assert_eq!(ids, vec!["a-canonical", "z-bare"]);
    }

    #[test]
    fn gc_only_removes_aged_terminal_jobs() {
        let conn = open_test_db();
        insert_minimal_job(&conn, "fresh-done", "completed");
        insert_minimal_job(&conn, "old-done", "completed");
        insert_minimal_job(&conn, "old-running", "running");

        let old = canonical_ts(chrono::Utc::now() - chrono::Duration::days(45));
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

        let old = canonical_ts(chrono::Utc::now() - chrono::Duration::minutes(20));
        conn.execute(
            "UPDATE foundry_jobs SET updated_at = ?1 WHERE id = 'old-running'",
            params![old],
        )
        .unwrap();
        let cutoff = canonical_ts(chrono::Utc::now() - chrono::Duration::minutes(10));

        let jobs = load_pending_foundry_jobs(&conn, &cutoff).unwrap();
        let ids = jobs
            .iter()
            .map(|job| job.spec.id.as_str())
            .collect::<Vec<_>>();
        assert_eq!(ids, vec!["queued", "old-running"]);
    }

    #[test]
    fn claim_foundry_job_for_run_uses_datetime_for_mixed_updated_cutoff() {
        // tachi#1638: writer-only canonicalization would still leave this
        // legacy bare row vulnerable to the raw string predicate
        // `updated_at < ?`, where `...100999+00:00` sorts before
        // `...100Z` even though it is not older at SQLite datetime()
        // precision. The stale-running lease must therefore remain blocked.
        let conn = open_test_db();
        insert_job_at(
            &conn,
            "same-second-running",
            "running",
            "2026-07-20T11:00:00.000Z",
            "2026-07-20T12:00:00.100999+00:00",
        );

        assert_eq!(
            claim_foundry_job_for_run(&conn, "same-second-running", "2026-07-20T12:00:00.100Z")
                .unwrap(),
            None
        );
        assert_eq!(status_of(&conn, "same-second-running"), "running");
    }

    #[test]
    fn claim_foundry_job_for_run_leases_only_queued_or_stale_running_jobs() {
        let conn = open_test_db();
        insert_minimal_job(&conn, "queued", "queued");
        insert_minimal_job(&conn, "fresh-running", "running");
        insert_minimal_job(&conn, "old-running", "running");
        insert_minimal_job(&conn, "completed", "completed");

        let old = canonical_ts(chrono::Utc::now() - chrono::Duration::minutes(20));
        conn.execute(
            "UPDATE foundry_jobs SET updated_at = ?1 WHERE id = 'old-running'",
            params![old],
        )
        .unwrap();
        let cutoff = canonical_ts(chrono::Utc::now() - chrono::Duration::minutes(10));

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
                created_at: now_utc_iso(),
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
