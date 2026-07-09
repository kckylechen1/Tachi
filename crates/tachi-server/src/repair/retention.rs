//! R2 — backfill missing `retention_policy` values.

use serde_json::json;

use super::{DbContext, Finding, RepairError, RepairRule, RuleReport};

pub struct RetentionBackfill;

const RETENTION_RULES: &[(&str, &str, &str)] = &[
    // Coordination records (handoff/kanban) are PINNED — they must survive
    // GC so dispatch state machines can find their rows. This matches
    // `default_retention_for` in types.rs and the schema.rs migration backfill.
    (
        "retention_null_pinned_coordination",
        "pinned",
        "category IN ('handoff', 'kanban') OR path LIKE '/handoff%' OR path LIKE '/kanban%'",
    ),
    (
        "retention_null_ephemeral_ops",
        "ephemeral",
        "category = 'ghost' OR path LIKE '/ghost%'",
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

pub(crate) fn default_retention_for_row(path: &str, category: &str, source: &str) -> &'static str {
    let path_lower = path.trim().to_ascii_lowercase();
    let category_lower = category.trim().to_ascii_lowercase();
    let source_lower = source.trim().to_ascii_lowercase();

    if category_lower == "handoff"
        || category_lower == "kanban"
        || path_lower.starts_with("/handoff")
        || path_lower.starts_with("/kanban")
    {
        "pinned"
    } else if category_lower == "ghost" || path_lower.starts_with("/ghost") {
        "ephemeral"
    } else if path_lower.starts_with("/wiki")
        || path_lower.starts_with("/skills")
        || path_lower.starts_with("/behavior")
        || matches!(category_lower.as_str(), "decision" | "preference")
        || source_lower == "foundry_distill"
    {
        "permanent"
    } else {
        "durable"
    }
}

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
                report
                    .findings
                    .push(Finding::new(*kind, count).with_detail(json!({"would_set": target})));
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
                report
                    .findings
                    .push(Finding::new(*kind, changed).with_detail(json!({"applied": target})));
                report.applied += changed;
            }
        }
        tx.commit()?;
        Ok(report)
    }
}
