//! R8 — conservative ephemeral recall-cache cleanup.
//!
//! Exact duplicates are deliberately outside this rule. Operators must use
//! `tachi repair dedupe exact/apply`, which archives losers through a bound
//! plan, receipt, CAS, and restore path instead of physically deleting them.
//! Empty JSON turn shapes have no canonical producer marker, so R8 retains
//! them rather than treating category, topic, or path substrings as deletion
//! authority.

use super::{DbContext, Finding, RepairError, RepairRule, RuleReport};
use memcore::namespace::FOUNDRY_RECALL_CACHE_SOURCE;

pub struct JunkCleanup;

fn has_table(ctx: &DbContext, name: &str) -> bool {
    ctx.conn
        .query_row(
            "SELECT 1 FROM sqlite_master WHERE type='table' AND name=?1",
            [name],
            |row| row.get::<_, i64>(0),
        )
        .is_ok()
}

// Invariant: R8 may physically delete only active, non-protected rows that
// carry no evidence of being recalled or genuinely used. `scored_count` is
// intentionally absent because it is scorer-only instrumentation, not recall
// or use evidence. Keep every physical-delete class behind this same guard.
const EPHEMERAL_JUNK_GUARDS_SQL: &str = r#"
    archived = 0
    AND superseded_by IS NULL
    AND COALESCE(retention_policy, '') NOT IN ('pinned', 'permanent')
    AND COALESCE(access_count, 0) = 0
    AND COALESCE(recall_count, 0) = 0
    AND COALESCE(query_diversity, 0) = 0
    AND last_access IS NULL
    AND last_use_at IS NULL
    AND NOT EXISTS (
        SELECT 1 FROM access_history
        WHERE access_history.memory_id = memories.id
    )
"#;

fn rerank_cache_sql() -> String {
    format!(
        "SELECT id FROM memories WHERE source = '{FOUNDRY_RECALL_CACHE_SOURCE}' AND ({EPHEMERAL_JUNK_GUARDS_SQL})"
    )
}

fn count_query(ctx: &DbContext, sql: &str) -> Result<usize, RepairError> {
    let count_sql = format!("SELECT COUNT(*) FROM ({sql})");
    Ok(ctx
        .conn
        .query_row(&count_sql, [], |row| row.get::<_, i64>(0))? as usize)
}

fn push_count(report: &mut RuleReport, kind: &str, count: usize) {
    if count > 0 {
        report.findings.push(Finding::new(kind, count));
    }
}

impl RepairRule for JunkCleanup {
    fn id(&self) -> &'static str {
        "R8"
    }

    fn name(&self) -> &'static str {
        "Ephemeral recall-cache cleanup; exact duplicates use repair dedupe exact/apply"
    }

    fn dry_run(&self, ctx: &mut DbContext) -> Result<RuleReport, RepairError> {
        let mut report = RuleReport::new(self.id(), self.name(), ctx.label.clone());
        push_count(
            &mut report,
            "foundry_recall_rerank_cache",
            count_query(ctx, &rerank_cache_sql())?,
        );
        Ok(report)
    }

    fn apply(&self, ctx: &mut DbContext) -> Result<RuleReport, RepairError> {
        let mut report = self.dry_run(ctx)?;
        let has_memories_fts = has_table(ctx, "memories_fts");
        let has_memories_symbolic_fts = has_table(ctx, "memories_symbolic_fts");
        let has_memories_vec = has_table(ctx, "memories_vec");
        let has_access_history = has_table(ctx, "access_history");
        let has_memory_edges = has_table(ctx, "memory_edges");
        let tx = ctx.conn.transaction()?;
        tx.execute_batch("CREATE TEMP TABLE cleanup_targets(id TEXT PRIMARY KEY);")?;
        tx.execute(
            &format!(
                "INSERT OR IGNORE INTO cleanup_targets {}",
                rerank_cache_sql()
            ),
            [],
        )?;
        let target_count = tx.query_row("SELECT COUNT(*) FROM cleanup_targets", [], |row| {
            row.get::<_, i64>(0)
        })? as usize;
        if has_memories_fts {
            tx.execute(
                "DELETE FROM memories_fts WHERE id IN (SELECT id FROM cleanup_targets)",
                [],
            )?;
        }
        if has_memories_symbolic_fts {
            // Keep trigram projection in lockstep with memories deletes (#1335).
            tx.execute(
                "DELETE FROM memories_symbolic_fts WHERE id IN (SELECT id FROM cleanup_targets)",
                [],
            )?;
        }
        if has_memories_vec {
            tx.execute(
                "DELETE FROM memories_vec WHERE id IN (SELECT id FROM cleanup_targets)",
                [],
            )?;
        }
        if has_access_history {
            tx.execute(
                "DELETE FROM access_history WHERE memory_id IN (SELECT id FROM cleanup_targets)",
                [],
            )?;
        }
        if has_memory_edges {
            tx.execute(
                "DELETE FROM memory_edges WHERE source_id IN (SELECT id FROM cleanup_targets) OR target_id IN (SELECT id FROM cleanup_targets)",
                [],
            )?;
        }
        tx.execute(
            "DELETE FROM memories WHERE id IN (SELECT id FROM cleanup_targets)",
            [],
        )?;
        tx.execute("DROP TABLE cleanup_targets", [])?;
        tx.commit()?;
        report.applied = target_count;
        Ok(report)
    }
}
