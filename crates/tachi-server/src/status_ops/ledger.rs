//! Dispatch / foundry / eval ledger reads for `tachi status`: latest foundry
//! jobs (active/terminal/failed), recent dispatch outcomes, recent eval rows,
//! and the daily-report / distill markers. Extracted from `status_ops::mod`
//! (no behavior change).

use super::{
    status_health, DispatchStatus, DistillMarkerStatus, LatestFailedJob, LatestFoundryJob,
    RecentEval, DISPATCH_STALE_THRESHOLD_SECS, DISTILL_STALE_THRESHOLD_SECS,
    FOUNDRY_RECALL_CACHE_SOURCE,
};
use chrono::{DateTime, Utc};
use memcore::MemoryStore;
use rusqlite::OptionalExtension;
use serde_json::json;
use std::path::Path;

pub(crate) fn latest_failed_job(
    conn: &rusqlite::Connection,
) -> Result<Option<LatestFailedJob>, rusqlite::Error> {
    conn.query_row(
        "SELECT id, kind, lane, updated_at, metadata
         FROM foundry_jobs
         WHERE status = 'failed'
         ORDER BY datetime(updated_at) DESC, datetime(created_at) DESC, id DESC
         LIMIT 1",
        [],
        |row| {
            let metadata: String = row.get(4)?;
            let reason = extract_terminal_reason(&metadata);
            let kind: String = row.get(1)?;
            let lane: Option<String> = row.get::<_, Option<String>>(2)?;
            Ok(LatestFailedJob {
                id: row.get(0)?,
                kind: kind.clone(),
                lane: lane.clone(),
                updated_at: row.get::<_, Option<String>>(3)?,
                inferred_invalid_provider: reason.as_deref().and_then(|reason| {
                    status_health::infer_provider_from_failed_job(&kind, lane.as_deref(), reason)
                }),
                reason,
            })
        },
    )
    .optional()
}

pub(crate) fn latest_foundry_job(
    conn: &rusqlite::Connection,
) -> Result<Option<LatestFoundryJob>, rusqlite::Error> {
    conn.query_row(
        "SELECT id, kind, status, updated_at
         FROM foundry_jobs
         ORDER BY datetime(updated_at) DESC, datetime(created_at) DESC, id DESC
         LIMIT 1",
        [],
        |row| {
            Ok(LatestFoundryJob {
                id: row.get(0)?,
                kind: row.get(1)?,
                status: row.get(2)?,
                updated_at: row.get::<_, Option<String>>(3)?,
            })
        },
    )
    .optional()
}

pub(crate) fn latest_foundry_job_with_statuses(
    conn: &rusqlite::Connection,
    statuses: &[&str],
) -> Result<Option<LatestFoundryJob>, rusqlite::Error> {
    let placeholders = vec!["?"; statuses.len()].join(",");
    let sql = format!(
        "SELECT id, kind, status, updated_at
         FROM foundry_jobs
         WHERE status IN ({placeholders})
         ORDER BY datetime(updated_at) DESC, datetime(created_at) DESC, id DESC
         LIMIT 1"
    );
    let params = rusqlite::params_from_iter(statuses.iter().copied());
    conn.query_row(&sql, params, |row| {
        Ok(LatestFoundryJob {
            id: row.get(0)?,
            kind: row.get(1)?,
            status: row.get(2)?,
            updated_at: row.get::<_, Option<String>>(3)?,
        })
    })
    .optional()
}

fn extract_terminal_reason(metadata: &str) -> Option<String> {
    let value: serde_json::Value = serde_json::from_str(metadata).ok()?;
    value
        .pointer("/terminal_reason/reason")
        .and_then(|v| v.as_str())
        .or_else(|| value.pointer("/error").and_then(|v| v.as_str()))
        .or_else(|| value.pointer("/last_error").and_then(|v| v.as_str()))
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

pub(crate) fn collect_dispatches(
    global_db_path: &Path,
    project_db_path: Option<&Path>,
) -> Vec<DispatchStatus> {
    let now = Utc::now();
    let mut out: Vec<(String, DispatchStatus)> = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for db_path in std::iter::once(global_db_path).chain(project_db_path) {
        let path_str = match db_path.to_str() {
            Some(s) => s,
            None => continue,
        };
        let store = match MemoryStore::open_read_only(path_str) {
            Ok(s) => s,
            Err(_) => continue,
        };
        let conn = store.connection();
        let mut stmt = match conn.prepare(
            "SELECT id, summary, text, metadata, created_at FROM memories \
             WHERE path LIKE '/kanban/tasks/%' \
               AND source != ?1 \
             ORDER BY created_at DESC LIMIT 10",
        ) {
            Ok(s) => s,
            Err(_) => continue,
        };
        let rows = match stmt.query_map([FOUNDRY_RECALL_CACHE_SOURCE], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, String>(4)?,
            ))
        }) {
            Ok(r) => r,
            Err(_) => continue,
        };
        for row in rows {
            let (id, summary, text, meta_str, created_at) = match row {
                Ok(r) => r,
                Err(_) => continue,
            };
            let meta: serde_json::Value = serde_json::from_str(&meta_str).unwrap_or(json!({}));
            let dispatch_id_full = meta
                .get("dispatch_id")
                .and_then(|v| v.as_str())
                .unwrap_or(&id)
                .to_string();
            if !seen.insert(dispatch_id_full.clone()) {
                continue;
            }
            let agent = meta
                .get("agent")
                .and_then(|v| v.as_str())
                .unwrap_or("unknown")
                .to_string();
            let outcome = normalize_dispatch_outcome(
                meta.get("a2a_state").and_then(|v| v.as_str()),
                &created_at,
                now,
            );
            let reviewed = meta
                .get("reviewed")
                .and_then(|v| v.as_bool())
                .unwrap_or(false);
            let elapsed = created_at
                .parse::<DateTime<Utc>>()
                .ok()
                .map(|dt| status_health::format_elapsed(now - dt))
                .unwrap_or_default();
            let dispatch_id = dispatch_id_full.chars().take(12).collect();
            let task = meta
                .get("task")
                .and_then(|v| v.as_str())
                .map(|s| s.chars().take(60).collect::<String>())
                .or_else(|| {
                    text.lines()
                        .find(|l| l.starts_with("Task: "))
                        .map(|l| l.trim_start_matches("Task: ").chars().take(60).collect())
                })
                .unwrap_or_else(|| {
                    summary
                        .trim_start_matches(|c: char| !c.is_alphanumeric())
                        .chars()
                        .take(60)
                        .collect()
                });
            out.push((
                created_at,
                DispatchStatus {
                    dispatch_id,
                    agent,
                    task,
                    outcome,
                    elapsed,
                    reviewed,
                },
            ));
        }
    }
    out.sort_by(|a, b| b.0.cmp(&a.0));
    out.truncate(10);
    out.into_iter().map(|(_, status)| status).collect()
}

pub(crate) fn normalize_dispatch_outcome(
    a2a_state: Option<&str>,
    created_at: &str,
    now: DateTime<Utc>,
) -> String {
    let outcome = a2a_state
        .map(|s| match s {
            "TASK_STATE_WORKING" | "TASK_STATE_IN_PROGRESS" => "in_progress",
            "TASK_STATE_COMPLETED" => "completed",
            "TASK_STATE_FAILED" => "failed",
            "TASK_STATE_CANCELED" => "aborted",
            "TASK_STATE_INPUT_REQUIRED" => "partial",
            other => other,
        })
        .unwrap_or("unknown")
        .to_string();

    if outcome == "in_progress"
        && created_at
            .parse::<DateTime<Utc>>()
            .ok()
            .is_some_and(|dt| now - dt > chrono::Duration::seconds(DISPATCH_STALE_THRESHOLD_SECS))
    {
        return "stale_working".to_string();
    }

    outcome
}

pub(crate) fn collect_recent_evals(
    global_db_path: &Path,
    project_db_path: Option<&Path>,
) -> Vec<RecentEval> {
    let mut evals = Vec::new();
    for db_path in std::iter::once(global_db_path).chain(project_db_path) {
        let path_str = match db_path.to_str() {
            Some(s) => s,
            None => continue,
        };
        let store = match MemoryStore::open_read_only(path_str) {
            Ok(s) => s,
            Err(_) => continue,
        };
        let conn = store.connection();
        let mut stmt = match conn.prepare(
            "SELECT id, summary, metadata, created_at FROM memories \
             WHERE path LIKE '/eval/2%' \
               AND id NOT LIKE 'foundry:%' \
               AND category IN ('eval', 'experience') \
             ORDER BY created_at DESC LIMIT 5",
        ) {
            Ok(s) => s,
            Err(_) => continue,
        };
        let rows = match stmt.query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
            ))
        }) {
            Ok(r) => r,
            Err(_) => continue,
        };
        for row in rows {
            let (id, _summary, meta_str, created_at) = match row {
                Ok(r) => r,
                Err(_) => continue,
            };
            let meta: serde_json::Value = serde_json::from_str(&meta_str).unwrap_or(json!({}));
            let agent = meta
                .get("agent")
                .and_then(|v| v.as_str())
                .unwrap_or("unknown")
                .to_string();
            let outcome = meta
                .get("outcome")
                .and_then(|v| v.as_str())
                .unwrap_or("unknown")
                .to_string();
            let quality_score = meta.get("quality_score").and_then(|v| v.as_f64());
            let task_id = id.chars().take(24).collect();
            evals.push(RecentEval {
                task_id,
                agent,
                outcome,
                quality_score,
                timestamp: created_at,
            });
        }
    }
    evals.sort_by(|a, b| b.timestamp.cmp(&a.timestamp));
    evals.truncate(5);
    evals
}

pub(crate) fn find_last_daily_report(app_home: &Path) -> Option<String> {
    crate::daily_pipeline::validated_latest_daily_report(app_home)
}

pub(crate) fn read_distill_marker(app_home: &Path) -> Option<DistillMarkerStatus> {
    let marker_path = app_home.join("foundry-runs").join(".last_distill_run");
    let raw = std::fs::read_to_string(&marker_path).ok()?;
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return None;
    }
    // Two on-disk formats. New: a JSON object `{"ts": <rfc3339>, "groups_distilled": …}`.
    // Legacy: a bare RFC3339 timestamp. A bare timestamp is not valid JSON (the `-`
    // after the year breaks number parsing), so `from_str` cleanly rejects it.
    let (last_run_at, groups_distilled, groups_skipped, fallback_used, errors, error_reason) =
        match serde_json::from_str::<serde_json::Value>(trimmed) {
            Ok(v) if v.get("ts").and_then(|t| t.as_str()).is_some() => {
                let ts = v["ts"].as_str().unwrap_or_default().to_string();
                let field = |k: &str| v.get(k).and_then(|x| x.as_u64()).map(|n| n as usize);
                let err_reason = v
                    .get("error")
                    .and_then(|x| x.as_str())
                    .map(|s| s.to_string());
                (
                    ts,
                    field("groups_distilled"),
                    field("groups_skipped"),
                    field("fallback_used"),
                    field("errors"),
                    err_reason,
                )
            }
            _ => (trimmed.to_string(), None, None, None, None, None),
        };
    let dt = DateTime::parse_from_rfc3339(&last_run_at)
        .ok()?
        .with_timezone(&Utc);
    let age = Utc::now() - dt;
    let age_seconds = age.num_seconds().max(0);
    Some(DistillMarkerStatus {
        path: marker_path.display().to_string(),
        last_run_at,
        age_seconds,
        age: status_health::format_elapsed(age),
        is_stale: age_seconds > DISTILL_STALE_THRESHOLD_SECS,
        groups_distilled,
        groups_skipped,
        fallback_used,
        errors,
        error_reason,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn open_foundry_jobs_db() -> rusqlite::Connection {
        let conn = rusqlite::Connection::open_in_memory().expect("memory db");
        conn.execute(
            "CREATE TABLE foundry_jobs (
                id TEXT PRIMARY KEY,
                kind TEXT NOT NULL,
                lane TEXT,
                status TEXT NOT NULL,
                metadata TEXT NOT NULL DEFAULT '{}',
                created_at TEXT NOT NULL,
                updated_at TEXT NOT NULL
            )",
            [],
        )
        .expect("create foundry jobs table");
        conn
    }

    #[test]
    fn latest_foundry_job_readers_order_mixed_updated_at_by_datetime_then_id() {
        // tachi#1638: these status readers order by `updated_at`/`created_at`.
        // A writer-only migration leaves raw DESC ordering, which picks the
        // canonical row first by bytes. Wrapping both ORDER BY keys in
        // datetime() treats the mixed renderings as the same instant, and the
        // id tie-breaker pins deterministic reader behavior with no backfill.
        let conn = open_foundry_jobs_db();
        conn.execute(
            "INSERT INTO foundry_jobs (id, kind, lane, status, metadata, created_at, updated_at)
             VALUES
             ('a-canonical', 'memory_distill', 'distill', 'failed', '{}',
              '2026-07-20T12:00:00.000Z', '2026-07-20T12:00:00.000Z'),
             ('z-bare', 'memory_distill', 'distill', 'failed', '{}',
              '2026-07-20T12:00:00+00:00', '2026-07-20T12:00:00+00:00')",
            [],
        )
        .expect("seed foundry jobs");

        assert_eq!(latest_failed_job(&conn).unwrap().unwrap().id, "z-bare");
        assert_eq!(latest_foundry_job(&conn).unwrap().unwrap().id, "z-bare");
        assert_eq!(
            latest_foundry_job_with_statuses(&conn, &["failed"])
                .unwrap()
                .unwrap()
                .id,
            "z-bare"
        );
    }
}
