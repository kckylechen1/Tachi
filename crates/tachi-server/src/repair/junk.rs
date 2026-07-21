//! R8 — deterministic junk memory cleanup.

use serde_json::json;

use super::{DbContext, Finding, RepairError, RepairRule, RuleReport};

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

const DUPLICATE_OLD_SQL: &str = r#"
    SELECT id FROM (
        SELECT
            id,
            ROW_NUMBER() OVER (
                PARTITION BY text, path, scope, COALESCE(domain, '')
                ORDER BY COALESCE(NULLIF(timestamp, ''), '') DESC, revision DESC, id DESC
            ) AS rn
        FROM memories
        WHERE TRIM(COALESCE(text, '')) <> ''
          AND LENGTH(TRIM(text)) >= 20
    )
    WHERE rn > 1
"#;

const RERANK_CACHE_SQL: &str = r#"
    SELECT id FROM memories
    WHERE id = 'foundry_recall_rerank_cache'
       OR id LIKE 'foundry:recall-cache:%'
       OR source = 'foundry_recall_rerank_cache'
       OR topic = 'foundry_recall_rerank_cache'
       OR topic = 'recall_rerank_cache'
       OR path = '/recall-cache'
       OR path LIKE '%/recall-cache'
       OR path LIKE '%/recall-cache/%'
       OR path LIKE '%foundry_recall_rerank_cache%'
       OR json_extract(metadata, '$.recall_rerank_cache') = 1
       OR json_extract(metadata, '$.cache_key') = 'foundry_recall_rerank_cache'
"#;

const EMPTY_JSON_TURN_SQL: &str = r#"
    SELECT id FROM memories
    WHERE TRIM(COALESCE(text, '')) IN ('{}', '[]')
      AND (
          category IN ('hermes_turn', 'other', 'fact')
          OR topic IN ('hermes_turn', 'turn', 'interaction')
          OR path LIKE '%hermes%'
          OR path LIKE '%turn%'
      )
"#;

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
        "Junk cleanup"
    }

    fn dry_run(&self, ctx: &mut DbContext) -> Result<RuleReport, RepairError> {
        let mut report = RuleReport::new(self.id(), self.name(), ctx.label.clone());
        push_count(
            &mut report,
            "duplicate_text_old_versions",
            count_query(ctx, DUPLICATE_OLD_SQL)?,
        );
        push_count(
            &mut report,
            "foundry_recall_rerank_cache",
            count_query(ctx, RERANK_CACHE_SQL)?,
        );
        push_count(
            &mut report,
            "empty_json_turns",
            count_query(ctx, EMPTY_JSON_TURN_SQL)?,
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
            &format!("INSERT OR IGNORE INTO cleanup_targets {DUPLICATE_OLD_SQL}"),
            [],
        )?;
        tx.execute(
            &format!("INSERT OR IGNORE INTO cleanup_targets {RERANK_CACHE_SQL}"),
            [],
        )?;
        tx.execute(
            &format!("INSERT OR IGNORE INTO cleanup_targets {EMPTY_JSON_TURN_SQL}"),
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
        if target_count > 0 {
            report.findings.push(
                Finding::new("deleted_memory_rows", target_count)
                    .with_detail(json!({"note": "FTS/vector/edge/access rows were cleaned first"})),
            );
        }
        Ok(report)
    }
}
