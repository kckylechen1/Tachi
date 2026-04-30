//! R2 — Backfill NULL `retention_policy` on per-project DBs.
//!
//! PR-1 ran the same backfill on the global DB. This rule covers everything
//! else: handoff/kanban → "pinned", wiki → "permanent", foundry_distill
//! source → "permanent".

use rusqlite::params;
use serde_json::json;

use super::{DbContext, Finding, RepairError, RepairRule, RuleReport};

pub struct RetentionBackfill;

fn count_null(ctx: &DbContext, where_clause: &str, params_vec: &[&str]) -> Result<i64, RepairError> {
    let sql = format!(
        "SELECT COUNT(*) FROM memories WHERE retention_policy IS NULL AND ({where_clause})"
    );
    // rusqlite needs &dyn ToSql; map &str slice.
    let p: Vec<&dyn rusqlite::ToSql> = params_vec.iter().map(|s| s as &dyn rusqlite::ToSql).collect();
    let n: i64 = ctx
        .conn
        .query_row(&sql, p.as_slice(), |r| r.get(0))?;
    Ok(n)
}

fn count_null_source_distill(ctx: &DbContext) -> Result<i64, RepairError> {
    let n: i64 = ctx
        .conn
        .query_row(
            "SELECT COUNT(*) FROM memories WHERE retention_policy IS NULL AND source = 'foundry_distill'",
            [],
            |r| r.get(0),
        )?;
    Ok(n)
}

impl RepairRule for RetentionBackfill {
    fn id(&self) -> &'static str {
        "R2"
    }

    fn name(&self) -> &'static str {
        "Retention backfill"
    }

    fn dry_run(&self, ctx: &mut DbContext) -> Result<RuleReport, RepairError> {
        let mut r = RuleReport::new(self.id(), self.name(), ctx.label.clone());
        let pinned = count_null(
            ctx,
            "path LIKE ? OR path LIKE ?",
            &["/handoff%", "/kanban%"],
        )?;
        if pinned > 0 {
            r.findings.push(
                Finding::new("retention_null_handoff_kanban", pinned as usize)
                    .with_detail(json!({"would_set": "pinned"})),
            );
        }
        let wiki = count_null(ctx, "path LIKE ?", &["/wiki%"])?;
        if wiki > 0 {
            r.findings.push(
                Finding::new("retention_null_wiki", wiki as usize)
                    .with_detail(json!({"would_set": "permanent"})),
            );
        }
        let distill = count_null_source_distill(ctx)?;
        if distill > 0 {
            r.findings.push(
                Finding::new("retention_null_foundry_distill", distill as usize)
                    .with_detail(json!({"would_set": "permanent"})),
            );
        }
        Ok(r)
    }

    fn apply(&self, ctx: &mut DbContext) -> Result<RuleReport, RepairError> {
        let mut r = RuleReport::new(self.id(), self.name(), ctx.label.clone());
        let tx = ctx.conn.transaction()?;
        let n1 = tx.execute(
            "UPDATE memories SET retention_policy = 'pinned'
             WHERE retention_policy IS NULL AND (path LIKE '/handoff%' OR path LIKE '/kanban%')",
            [],
        )?;
        let n2 = tx.execute(
            "UPDATE memories SET retention_policy = 'permanent'
             WHERE retention_policy IS NULL AND path LIKE '/wiki%'",
            [],
        )?;
        let n3 = tx.execute(
            "UPDATE memories SET retention_policy = 'permanent'
             WHERE retention_policy IS NULL AND source = 'foundry_distill'",
            [],
        )?;
        tx.commit()?;
        if n1 > 0 {
            r.findings.push(Finding::new("retention_null_handoff_kanban", n1));
        }
        if n2 > 0 {
            r.findings.push(Finding::new("retention_null_wiki", n2));
        }
        if n3 > 0 {
            r.findings.push(Finding::new("retention_null_foundry_distill", n3));
        }
        r.applied = n1 + n2 + n3;
        // Suppress `unused` warning for params helper.
        let _ = params![1];
        Ok(r)
    }
}
