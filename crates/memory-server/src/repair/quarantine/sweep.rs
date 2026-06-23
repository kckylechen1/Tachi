use super::*;

pub struct QuarantineSweep;

impl RepairRule for QuarantineSweep {
    fn id(&self) -> &'static str {
        "R3"
    }
    fn name(&self) -> &'static str {
        "Quarantine sweep"
    }
    fn mutates(&self) -> bool {
        false
    }

    fn dry_run(&self, ctx: &mut DbContext) -> Result<RuleReport, RepairError> {
        let mut r = RuleReport::new(self.id(), self.name(), ctx.label.clone());
        let n: i64 = ctx.conn.query_row(
            "SELECT COUNT(*) FROM memories WHERE path LIKE '/_quarantine/%'",
            [],
            |row| row.get(0),
        )?;
        if n > 0 {
            // Group by expected_db for visibility.
            let mut stmt = ctx.conn.prepare(
                "SELECT COALESCE(json_extract(metadata, '$.quarantine.expected_db'), '<unknown>') AS exp,
                        COUNT(*)
                 FROM memories
                 WHERE path LIKE '/_quarantine/%'
                 GROUP BY exp",
            )?;
            let rows = stmt
                .query_map([], |row| {
                    Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
                })?
                .collect::<Result<Vec<_>, _>>()?;
            let detail: Value = rows
                .iter()
                .map(|(k, v)| json!({ "expected_db": k, "count": v }))
                .collect();
            r.findings
                .push(Finding::new("quarantined_rows", n as usize).with_detail(detail));
        }
        Ok(r)
    }

    fn apply(&self, ctx: &mut DbContext) -> Result<RuleReport, RepairError> {
        // Sweep doesn't auto-resolve; it just reports. Use the explicit
        // `quarantine restore-all` / `restore` / `purge` subcommands.
        let mut r = self.dry_run(ctx)?;
        if !r.findings.is_empty() {
            r.errors.push(
                "use `tachi repair quarantine restore-all --to-db <label> --apply` or `purge`"
                    .into(),
            );
        }
        Ok(r)
    }
}
