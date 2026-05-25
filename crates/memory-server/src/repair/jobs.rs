//! R4 — Drop legacy / dead-letter foundry_jobs.

use chrono::{DateTime, Duration, Utc};
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
        let dl_cutoff: DateTime<Utc> = Utc::now() - Duration::days(dl_days);
        let cp_cutoff: DateTime<Utc> = Utc::now() - Duration::days(cp_days);
        let dl_iso = dl_cutoff.to_rfc3339();
        let cp_iso = cp_cutoff.to_rfc3339();

        let n_dl: i64 = ctx.conn.query_row(
            "SELECT COUNT(*) FROM foundry_jobs
                 WHERE status = 'dead_letter' AND updated_at < ?1",
            [&dl_iso],
            |row| row.get(0),
        )?;
        let n_cp: i64 = ctx.conn.query_row(
            "SELECT COUNT(*) FROM foundry_jobs
                 WHERE status = 'completed' AND updated_at < ?1",
            [&cp_iso],
            |row| row.get(0),
        )?;
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
            let fd_iso = (Utc::now() - Duration::days(fd_days as i64)).to_rfc3339();
            let n_fd: i64 = ctx.conn.query_row(
                "SELECT COUNT(*) FROM foundry_jobs
                     WHERE status = 'failed' AND updated_at < ?1",
                [&fd_iso],
                |row| row.get(0),
            )?;
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
        let dl_iso = (Utc::now() - Duration::days(dl_days)).to_rfc3339();
        let cp_iso = (Utc::now() - Duration::days(cp_days)).to_rfc3339();
        let tx = ctx.conn.transaction()?;
        let n_dl = tx.execute(
            "DELETE FROM foundry_jobs WHERE status = 'dead_letter' AND updated_at < ?1",
            [&dl_iso],
        )?;
        let n_cp = tx.execute(
            "DELETE FROM foundry_jobs WHERE status = 'completed' AND updated_at < ?1",
            [&cp_iso],
        )?;
        let n_fd = if let Some(fd_days) = self.failed_days {
            let fd_iso = (Utc::now() - Duration::days(fd_days as i64)).to_rfc3339();
            tx.execute(
                "DELETE FROM foundry_jobs WHERE status = 'failed' AND updated_at < ?1",
                [&fd_iso],
            )?
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
