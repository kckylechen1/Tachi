//! R7 — Orphan reference cleanup.

use serde_json::json;

use super::{DbContext, Finding, RepairError, RepairRule, RuleReport};

pub struct OrphanRefs;

fn has_table(ctx: &DbContext, name: &str) -> bool {
    ctx.conn
        .query_row(
            "SELECT 1 FROM sqlite_master WHERE type='table' AND name=?1",
            [name],
            |r| r.get::<_, i64>(0),
        )
        .is_ok()
}

/// (table, column referencing memories.id, label-suffix)
const REFS: &[(&str, &str, &str)] = &[
    ("memory_edges", "source_id", "edges_source"),
    ("memory_edges", "target_id", "edges_target"),
    ("agent_known_state", "memory_id", "known_state"),
    ("processed_events", "memory_id", "processed_events"),
    ("access_history", "memory_id", "access_history"),
    ("memories_vec", "id", "vectors"),
];

fn count_broken_superseded_refs(ctx: &DbContext) -> Result<i64, RepairError> {
    Ok(ctx.conn.query_row(
        "SELECT COUNT(*)
         FROM memories m
         WHERE m.superseded_by IS NOT NULL
           AND trim(m.superseded_by) <> ''
           AND NOT EXISTS (SELECT 1 FROM memories target WHERE target.id = m.superseded_by)",
        [],
        |row| row.get(0),
    )?)
}

impl RepairRule for OrphanRefs {
    fn id(&self) -> &'static str {
        "R7"
    }
    fn name(&self) -> &'static str {
        "Orphan refs"
    }

    fn dry_run(&self, ctx: &mut DbContext) -> Result<RuleReport, RepairError> {
        let mut r = RuleReport::new(self.id(), self.name(), ctx.label.clone());
        for (table, col, label) in REFS {
            if !has_table(ctx, table) {
                continue;
            }
            // Count rows whose memory id no longer exists. Assume the column
            // exists if the table exists; on schema drift we skip silently.
            let sql = format!(
                "SELECT COUNT(*) FROM {table} WHERE {col} NOT IN (SELECT id FROM memories)"
            );
            let n: i64 = match ctx.conn.query_row(&sql, [], |row| row.get(0)) {
                Ok(v) => v,
                Err(_) => continue,
            };
            if n > 0 {
                r.findings.push(
                    Finding::new(format!("orphans_{label}"), n as usize)
                        .with_detail(json!({"table": table, "column": col})),
                );
            }
        }
        let broken_superseded = count_broken_superseded_refs(ctx)?;
        if broken_superseded > 0 {
            r.findings.push(
                Finding::new("orphans_memories_superseded_by", broken_superseded as usize)
                    .with_detail(json!({"table": "memories", "column": "superseded_by"})),
            );
        }
        Ok(r)
    }

    fn apply(&self, ctx: &mut DbContext) -> Result<RuleReport, RepairError> {
        let mut r = RuleReport::new(self.id(), self.name(), ctx.label.clone());
        let tx = ctx.conn.transaction()?;
        for (table, col, label) in REFS {
            // Existence check using the same conn (safe inside tx).
            let exists: bool = tx
                .query_row(
                    "SELECT 1 FROM sqlite_master WHERE type='table' AND name=?1",
                    [table],
                    |row| row.get::<_, i64>(0),
                )
                .is_ok();
            if !exists {
                continue;
            }
            let sql = format!("DELETE FROM {table} WHERE {col} NOT IN (SELECT id FROM memories)");
            let n = match tx.execute(&sql, []) {
                Ok(v) => v,
                Err(_) => continue,
            };
            if n > 0 {
                r.findings.push(Finding::new(format!("orphans_{label}"), n));
                r.applied += n;
            }
        }
        let superseded_n = tx.execute(
            "UPDATE memories
             SET superseded_by = NULL
             WHERE superseded_by IS NOT NULL
               AND trim(superseded_by) <> ''
               AND NOT EXISTS (SELECT 1 FROM memories target WHERE target.id = memories.superseded_by)",
            [],
        )?;
        if superseded_n > 0 {
            r.findings
                .push(Finding::new("orphans_memories_superseded_by", superseded_n));
            r.applied += superseded_n;
        }
        tx.commit()?;
        Ok(r)
    }
}
