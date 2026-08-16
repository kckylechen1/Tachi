//! R2 — backfill missing `retention_policy` values.

use rusqlite::{Transaction, TransactionBehavior};
use serde_json::json;

use super::{DbContext, Finding, RepairError, RepairRule, RuleReport};

pub struct RetentionBackfill;

const RETENTION_RULES: &[(&str, &str)] = &[
    // Coordination records (handoff/kanban) are PINNED — they must survive
    // GC so dispatch state machines can find their rows. This matches
    // `default_retention_for` in types.rs and the schema.rs migration backfill.
    ("retention_null_pinned_coordination", "pinned"),
    ("retention_null_ephemeral_ops", "ephemeral"),
    ("retention_null_permanent_knowledge", "permanent"),
    ("retention_null_durable_default", "durable"),
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

fn ordinary_candidates(tx: &Transaction<'_>) -> Result<Vec<(String, &'static str)>, RepairError> {
    let rows = {
        let mut stmt = tx.prepare(
            "SELECT id, path, category, COALESCE(source, '')
             FROM memories
             WHERE retention_policy IS NULL OR TRIM(retention_policy) = ''",
        )?;
        let rows = stmt
            .query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                ))
            })?
            .collect::<Result<Vec<_>, _>>()?;
        rows
    };
    let mut candidates = Vec::with_capacity(rows.len());
    for (id, path, category, source) in rows {
        match memcore::db::refuse_retired_sticky_row_within_tx(tx, &id, "retention-backfilled") {
            Ok(()) => candidates.push((id, default_retention_for_row(&path, &category, &source))),
            Err(memcore::MemoryError::InvalidArg(_)) => {}
            Err(error) => return Err(error.into()),
        }
    }
    Ok(candidates)
}

fn append_findings(
    report: &mut RuleReport,
    candidates: &[(String, &'static str)],
    detail_key: &str,
) {
    for (kind, target) in RETENTION_RULES {
        let count = candidates
            .iter()
            .filter(|(_, candidate_target)| candidate_target == target)
            .count();
        if count > 0 {
            let detail = if detail_key == "would_set" {
                json!({"would_set": target})
            } else {
                json!({"applied": target})
            };
            report
                .findings
                .push(Finding::new(*kind, count).with_detail(detail));
        }
    }
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
        let tx = ctx
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let candidates = ordinary_candidates(&tx)?;
        append_findings(&mut report, &candidates, "would_set");
        tx.commit()?;
        Ok(report)
    }

    fn apply(&self, ctx: &mut DbContext) -> Result<RuleReport, RepairError> {
        let mut report = RuleReport::new(self.id(), self.name(), ctx.label.clone());
        let tx = ctx
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let candidates = ordinary_candidates(&tx)?;
        for (id, target) in &candidates {
            report.applied += tx.execute(
                "UPDATE memories SET retention_policy = ?1
                 WHERE id = ?2
                   AND (retention_policy IS NULL OR TRIM(retention_policy) = '')",
                rusqlite::params![target, id],
            )?;
        }
        append_findings(&mut report, &candidates, "applied");
        tx.commit()?;
        Ok(report)
    }
}
