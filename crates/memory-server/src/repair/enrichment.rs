//! R10 — reset stale enrichment failure markers.
//!
//! This is intentionally not a network retry. It clears failed enrichment
//! metadata after the operator has fixed provider keys or schema drift, so
//! status no longer presents old failures as missing-vector backfill work.

use serde_json::json;

use super::{DbContext, Finding, RepairError, RepairRule, RuleReport};

pub struct EnrichmentFailureReset;

fn count_failed(ctx: &DbContext) -> Result<usize, RepairError> {
    Ok(ctx.conn.query_row(
        "SELECT COUNT(*)
         FROM memories
         WHERE json_extract(metadata, '$.enrichment.status') = 'failed'",
        [],
        |row| row.get::<_, i64>(0).map(|n| n as usize),
    )?)
}

fn failure_breakdown(ctx: &DbContext) -> Result<Vec<Finding>, RepairError> {
    let mut stmt = ctx.conn.prepare(
        "SELECT
             COALESCE(json_extract(metadata, '$.enrichment.failed_stage'), 'unknown') AS stage,
             COALESCE(json_extract(metadata, '$.enrichment.last_error'), '') AS last_error,
             COUNT(*) AS n
         FROM memories
         WHERE json_extract(metadata, '$.enrichment.status') = 'failed'
         GROUP BY stage, last_error
         ORDER BY n DESC",
    )?;
    let rows = stmt.query_map([], |row| {
        let stage: String = row.get(0)?;
        let last_error: String = row.get(1)?;
        let count: i64 = row.get(2)?;
        Ok(
            Finding::new(format!("enrichment_failed_{stage}"), count as usize).with_detail(json!({
                "stage": stage,
                "last_error": last_error,
                "action": "clear_failed_marker",
            })),
        )
    })?;

    let mut out = Vec::new();
    for row in rows {
        out.push(row?);
    }
    Ok(out)
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
        report.findings = failure_breakdown(ctx)?;
        Ok(report)
    }

    fn apply(&self, ctx: &mut DbContext) -> Result<RuleReport, RepairError> {
        let mut report = self.dry_run(ctx)?;
        if report.findings.is_empty() {
            return Ok(report);
        }

        let tx = ctx.conn.transaction()?;
        let changed = tx.execute(
            "UPDATE memories
             SET metadata = json_remove(
                   CASE WHEN json_valid(metadata) THEN metadata ELSE '{}' END,
                   '$.enrichment.status',
                   '$.enrichment.failed_stage',
                   '$.enrichment.last_error',
                   '$.enrichment.last_failure_at'
                 ),
                 updated_at = strftime('%Y-%m-%dT%H:%M:%fZ', 'now')
             WHERE json_extract(metadata, '$.enrichment.status') = 'failed'",
            [],
        )?;
        tx.commit()?;

        report.applied = changed;
        let remaining = count_failed(ctx)?;
        if remaining > 0 {
            report.errors.push(format!(
                "{remaining} enrichment failure marker(s) remain after reset"
            ));
        }
        Ok(report)
    }
}
