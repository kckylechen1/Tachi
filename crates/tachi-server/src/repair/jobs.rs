//! R4 — Drop legacy / dead-letter foundry_jobs.

use chrono::{Duration, SecondsFormat, Utc};
use rusqlite::params;
use serde_json::json;

use super::{DbContext, Finding, RepairError, RepairRule, RuleReport};

#[derive(Default)]
pub struct JobsPurge {
    /// Custom dead-letter retention in days. Defaults to 30 if None.
    pub dead_letter_days: Option<u64>,
    /// Custom completed retention in days. Defaults to 90 if None.
    pub completed_days: Option<u64>,
    /// When Some(N), also purge `failed` foundry_jobs older than N days.
    /// Disabled by default — failed rows are usually retained for triage,
    /// but the Phase 1 distill migration leaves behind legacy
    /// MemoryDistill failures that operators may want to sweep with
    /// `tachi repair --purge-failed`.
    pub failed_days: Option<u64>,
}

fn has_table(ctx: &DbContext, name: &str) -> bool {
    ctx.conn
        .query_row(
            "SELECT 1 FROM sqlite_master WHERE type='table' AND name=?1",
            [name],
            |r| r.get::<_, i64>(0),
        )
        .is_ok()
}

fn retention_cutoff_iso(days: i64) -> String {
    (Utc::now() - Duration::days(days)).to_rfc3339_opts(SecondsFormat::Millis, true)
}

fn count_jobs_before(
    conn: &rusqlite::Connection,
    status: &str,
    cutoff: &str,
) -> Result<i64, rusqlite::Error> {
    conn.query_row(
        "SELECT COUNT(*) FROM foundry_jobs
         WHERE status = ?1 AND datetime(updated_at) < datetime(?2)",
        params![status, cutoff],
        |row| row.get(0),
    )
}

fn delete_jobs_before(
    conn: &rusqlite::Connection,
    status: &str,
    cutoff: &str,
) -> Result<usize, rusqlite::Error> {
    conn.execute(
        "DELETE FROM foundry_jobs WHERE status = ?1 AND datetime(updated_at) < datetime(?2)",
        params![status, cutoff],
    )
}

impl RepairRule for JobsPurge {
    fn id(&self) -> &'static str {
        "R4"
    }

    fn name(&self) -> &'static str {
        "Foundry jobs purge"
    }

    fn dry_run(&self, ctx: &mut DbContext) -> Result<RuleReport, RepairError> {
        let mut r = RuleReport::new(self.id(), self.name(), ctx.label.clone());
        if !has_table(ctx, "foundry_jobs") {
            return Ok(r);
        }
        let dl_days = self.dead_letter_days.unwrap_or(30) as i64;
        let cp_days = self.completed_days.unwrap_or(90) as i64;
        let dl_iso = retention_cutoff_iso(dl_days);
        let cp_iso = retention_cutoff_iso(cp_days);

        let n_dl = count_jobs_before(&ctx.conn, "dead_letter", &dl_iso)?;
        let n_cp = count_jobs_before(&ctx.conn, "completed", &cp_iso)?;
        if n_dl > 0 {
            r.findings.push(
                Finding::new("dead_letter_jobs", n_dl as usize)
                    .with_detail(json!({"older_than_days": dl_days})),
            );
        }
        if n_cp > 0 {
            r.findings.push(
                Finding::new("completed_jobs_stale", n_cp as usize)
                    .with_detail(json!({"older_than_days": cp_days})),
            );
        }
        if let Some(fd_days) = self.failed_days {
            let fd_iso = retention_cutoff_iso(fd_days as i64);
            let n_fd = count_jobs_before(&ctx.conn, "failed", &fd_iso)?;
            if n_fd > 0 {
                r.findings.push(
                    Finding::new("failed_jobs", n_fd as usize)
                        .with_detail(json!({"older_than_days": fd_days})),
                );
            }
        }
        Ok(r)
    }

    fn apply(&self, ctx: &mut DbContext) -> Result<RuleReport, RepairError> {
        let mut r = RuleReport::new(self.id(), self.name(), ctx.label.clone());
        if !has_table(ctx, "foundry_jobs") {
            return Ok(r);
        }
        let dl_days = self.dead_letter_days.unwrap_or(30) as i64;
        let cp_days = self.completed_days.unwrap_or(90) as i64;
        let dl_iso = retention_cutoff_iso(dl_days);
        let cp_iso = retention_cutoff_iso(cp_days);
        let tx = ctx.conn.transaction()?;
        let n_dl = delete_jobs_before(&tx, "dead_letter", &dl_iso)?;
        let n_cp = delete_jobs_before(&tx, "completed", &cp_iso)?;
        let n_fd = if let Some(fd_days) = self.failed_days {
            let fd_iso = retention_cutoff_iso(fd_days as i64);
            delete_jobs_before(&tx, "failed", &fd_iso)?
        } else {
            0
        };
        tx.commit()?;
        if n_dl > 0 {
            r.findings.push(Finding::new("dead_letter_jobs", n_dl));
        }
        if n_cp > 0 {
            r.findings.push(Finding::new("completed_jobs_stale", n_cp));
        }
        if n_fd > 0 {
            r.findings.push(Finding::new("failed_jobs", n_fd));
        }
        r.applied = n_dl + n_cp + n_fd;
        Ok(r)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn open_jobs_db() -> rusqlite::Connection {
        let conn = rusqlite::Connection::open_in_memory().expect("memory db");
        conn.execute(
            "CREATE TABLE foundry_jobs (
                id TEXT PRIMARY KEY,
                status TEXT NOT NULL,
                updated_at TEXT NOT NULL
            )",
            [],
        )
        .expect("create foundry jobs table");
        conn
    }

    #[test]
    fn purge_readers_use_datetime_for_mixed_updated_cutoff() {
        // tachi#1638: this covers both R4 dry-run counts and destructive
        // purge eligibility. A writer-only migration would leave raw
        // `updated_at < ?` predicates, where the same-second bare row sorts
        // before the canonical cutoff and is falsely selected. The datetime()
        // reader wrap is the discriminator; no foundry_jobs backfill is needed.
        let conn = open_jobs_db();
        conn.execute(
            "INSERT INTO foundry_jobs (id, status, updated_at)
             VALUES
             ('same-second-dead-letter', 'dead_letter', '2026-07-20T12:00:00.100999+00:00'),
             ('actually-old-dead-letter', 'dead_letter', '2026-07-19T12:00:00.999999+00:00'),
             ('old-completed', 'completed', '2026-07-19T12:00:00.999999+00:00')",
            [],
        )
        .expect("seed foundry jobs");

        let cutoff = "2026-07-20T12:00:00.100Z";
        assert_eq!(
            count_jobs_before(&conn, "dead_letter", cutoff).expect("count"),
            1
        );
        assert_eq!(
            delete_jobs_before(&conn, "dead_letter", cutoff).expect("delete"),
            1
        );

        let remaining = conn
            .prepare("SELECT id FROM foundry_jobs ORDER BY id")
            .unwrap()
            .query_map([], |row| row.get::<_, String>(0))
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap();
        assert_eq!(remaining, vec!["old-completed", "same-second-dead-letter"]);
    }
}
