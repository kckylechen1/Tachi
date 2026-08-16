//! R10 — reset stale enrichment failure markers.
//!
//! This is intentionally not a network retry. It clears failed enrichment
//! metadata after the operator has fixed provider keys or schema drift, so
//! status no longer presents old failures as missing-vector backfill work.

use rusqlite::{Transaction, TransactionBehavior};
use serde_json::json;

use super::{DbContext, Finding, RepairError, RepairRule, RuleReport};

pub struct EnrichmentFailureReset;

fn ordinary_failures(tx: &Transaction<'_>) -> Result<Vec<(String, String, String)>, RepairError> {
    let rows = {
        let mut stmt = tx.prepare(
            "SELECT id,
                    COALESCE(json_extract(metadata, '$.enrichment.failed_stage'), 'unknown'),
                    COALESCE(json_extract(metadata, '$.enrichment.last_error'), '')
             FROM memories
             WHERE json_extract(metadata, '$.enrichment.status') = 'failed'
             ORDER BY id",
        )?;
        let rows = stmt
            .query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                ))
            })?
            .collect::<Result<Vec<_>, _>>()?;
        rows
    };
    let mut ordinary = Vec::new();
    for row in rows {
        match memcore::db::refuse_retired_sticky_row_within_tx(
            tx,
            &row.0,
            "enrichment metadata reset",
        ) {
            Ok(()) => ordinary.push(row),
            Err(memcore::MemoryError::InvalidArg(_)) => {}
            Err(error) => return Err(error.into()),
        }
    }
    Ok(ordinary)
}

fn findings_for(rows: &[(String, String, String)]) -> Vec<Finding> {
    let mut groups = std::collections::BTreeMap::<(String, String), usize>::new();
    for (_, stage, error) in rows {
        *groups.entry((stage.clone(), error.clone())).or_default() += 1;
    }
    groups
        .into_iter()
        .map(|((stage, last_error), count)| {
            Finding::new(format!("enrichment_failed_{stage}"), count).with_detail(json!({
                "stage": stage, "last_error": last_error, "action": "clear_failed_marker"
            }))
        })
        .collect()
}

impl RepairRule for EnrichmentFailureReset {
    fn id(&self) -> &'static str {
        "R10"
    }

    fn name(&self) -> &'static str {
        "Enrichment failure reset"
    }

    fn dry_run(&self, ctx: &mut DbContext) -> Result<RuleReport, RepairError> {
        let mut report = RuleReport::new(self.id(), self.name(), ctx.label.clone());
        let tx = ctx
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        report.findings = findings_for(&ordinary_failures(&tx)?);
        tx.commit()?;
        Ok(report)
    }

    fn apply(&self, ctx: &mut DbContext) -> Result<RuleReport, RepairError> {
        let mut report = RuleReport::new(self.id(), self.name(), ctx.label.clone());
        let tx = ctx
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let failures = ordinary_failures(&tx)?;
        report.findings = findings_for(&failures);
        if failures.is_empty() {
            tx.commit()?;
            return Ok(report);
        }
        let mut changed = 0;
        for (id, _, _) in failures {
            changed += tx.execute(
                "UPDATE memories
             SET metadata = json_remove(
                   CASE WHEN json_valid(metadata) THEN metadata ELSE '{}' END,
                   '$.enrichment.status',
                   '$.enrichment.failed_stage',
                   '$.enrichment.last_error',
                   '$.enrichment.last_failure_at'
                 ),
                 updated_at = strftime('%Y-%m-%dT%H:%M:%fZ', 'now')
             WHERE id=?1 AND json_extract(metadata, '$.enrichment.status') = 'failed'",
                [id],
            )?;
        }
        tx.commit()?;

        report.applied = changed;
        Ok(report)
    }
}
