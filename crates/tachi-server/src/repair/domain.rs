//! R9 — deterministic domain normalization/backfill.

use rusqlite::{Transaction, TransactionBehavior};
use serde_json::json;

use super::{DbContext, Finding, RepairError, RepairRule, RuleReport};
use crate::memory_row_defaults::repair_target;

pub struct DomainRepair;

type Candidate = (String, String, String);

fn collect_candidates(tx: &Transaction<'_>) -> Result<Vec<Candidate>, RepairError> {
    let mut stmt = tx.prepare(
        "SELECT id, path, category, source, domain
         FROM memories
         ORDER BY id",
    )?;
    let rows = stmt.query_map([], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, String>(2)?,
            row.get::<_, String>(3)?,
            row.get::<_, Option<String>>(4)?,
        ))
    })?;

    let mut out = Vec::new();
    for row in rows {
        let (id, path, category, source, domain) = row?;
        if let Some(target) = repair_target(domain.as_deref(), &path, &category, &source) {
            match memcore::db::refuse_retired_sticky_row_within_tx(tx, &id, "domain-repaired") {
                Ok(()) => out.push((id, target, path)),
                Err(memcore::MemoryError::InvalidArg(_)) => {}
                Err(error) => return Err(error.into()),
            }
        }
    }
    Ok(out)
}

impl RepairRule for DomainRepair {
    fn id(&self) -> &'static str {
        "R9"
    }

    fn name(&self) -> &'static str {
        "Domain repair"
    }

    fn dry_run(&self, ctx: &mut DbContext) -> Result<RuleReport, RepairError> {
        let mut report = RuleReport::new(self.id(), self.name(), ctx.label.clone());
        let tx = ctx
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let candidates = collect_candidates(&tx)?;
        if !candidates.is_empty() {
            let mut by_target = std::collections::BTreeMap::<String, usize>::new();
            for (_, target, _) in &candidates {
                *by_target.entry(target.clone()).or_default() += 1;
            }
            report.findings.push(
                Finding::new("domain_repaired", candidates.len()).with_detail(json!({
                    "targets": by_target
                })),
            );
        }
        tx.commit()?;
        Ok(report)
    }

    fn apply(&self, ctx: &mut DbContext) -> Result<RuleReport, RepairError> {
        let mut report = RuleReport::new(self.id(), self.name(), ctx.label.clone());
        let tx = ctx
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let candidates = collect_candidates(&tx)?;
        if candidates.is_empty() {
            tx.commit()?;
            return Ok(report);
        }
        let mut by_target = std::collections::BTreeMap::<String, usize>::new();
        for (id, target, _) in &candidates {
            tx.execute(
                "UPDATE memories SET domain = ?2 WHERE id = ?1",
                rusqlite::params![id, target],
            )?;
            *by_target.entry(target.clone()).or_default() += 1;
        }
        tx.commit()?;

        report.applied = candidates.len();
        report.findings.push(
            Finding::new("domain_repaired", candidates.len()).with_detail(json!({
                "applied_targets": by_target
            })),
        );
        Ok(report)
    }
}
