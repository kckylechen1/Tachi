//! R2 — backfill missing `retention_policy` values.

use serde_json::json;

use super::{DbContext, Finding, RepairError, RepairRule, RuleReport};

pub struct RetentionBackfill;

const RETENTION_RULES: &[(&str, &str, &str)] = &[
    (
        "retention_null_ephemeral_ops",
        "ephemeral",
        "category IN ('handoff', 'kanban', 'ghost') OR path LIKE '/handoff%' OR path LIKE '/kanban%' OR path LIKE '/ghost%'",
    ),
    (
        "retention_null_permanent_knowledge",
        "permanent",
        "path LIKE '/wiki%' OR path LIKE '/skills%' OR path LIKE '/behavior%' OR category IN ('decision', 'preference') OR source = 'foundry_distill'",
    ),
    (
        "retention_null_durable_default",
        "durable",
        "1=1",
    ),
];

fn count_clause(ctx: &DbContext, clause: &str) -> Result<usize, RepairError> {
    let sql = format!(
        "SELECT COUNT(*) FROM memories WHERE (retention_policy IS NULL OR TRIM(retention_policy) = '') AND ({clause})"
    );
    Ok(ctx.conn.query_row(&sql, [], |row| row.get::<_, i64>(0))? as usize)
}

impl RepairRule for RetentionBackfill {
    fn id(&self) -> &'static str {
        "R2"
    }

    fn name(&self) -> &'static str {
        "Retention backfill"
    }

    fn dry_run(&self, ctx: &mut DbContext) -> Result<RuleReport, RepairError> {
        let mut report = RuleReport::new(self.id(), self.name(), ctx.label.clone());
        let mut already_claimed = 0usize;
        let missing_total = count_clause(ctx, "1=1")?;
        for (kind, target, clause) in RETENTION_RULES {
            let mut count = count_clause(ctx, clause)?;
            if *target == "durable" {
                count = missing_total.saturating_sub(already_claimed);
            } else {
                already_claimed += count;
            }
            if count > 0 {
                report.findings.push(
                    Finding::new(*kind, count).with_detail(json!({"would_set": target})),
                );
            }
        }
        Ok(report)
    }

    fn apply(&self, ctx: &mut DbContext) -> Result<RuleReport, RepairError> {
        let mut report = RuleReport::new(self.id(), self.name(), ctx.label.clone());
        let tx = ctx.conn.transaction()?;
        for (kind, target, clause) in RETENTION_RULES {
            let sql = format!(
                "UPDATE memories SET retention_policy = '{target}' WHERE (retention_policy IS NULL OR TRIM(retention_policy) = '') AND ({clause})"
            );
            let changed = tx.execute(&sql, [])?;
            if changed > 0 {
                report.findings.push(
                    Finding::new(*kind, changed)
                        .with_detail(json!({"applied": target})),
                );
                report.applied += changed;
            }
        }
        tx.commit()?;
        Ok(report)
    }
}
