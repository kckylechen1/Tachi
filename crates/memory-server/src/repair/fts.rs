//! R1 — FTS rebuild. Detects drift between `memories` and `memories_fts`,
//! and a missing `memories_fts` virtual table altogether (the quant case).

use serde_json::json;

use super::{DbContext, Finding, RepairError, RepairRule, RuleReport};

pub struct FtsRebuild;

const FTS_CREATE: &str = r#"
    CREATE VIRTUAL TABLE memories_fts USING fts5(
        id UNINDEXED,
        path,
        summary,
        text,
        keywords,
        entities,
        tokenize = 'simple'
    );
"#;

const FTS_INSERT: &str = r#"
    INSERT INTO memories_fts (id, path, summary, text, keywords, entities)
    SELECT
        id, path, summary, text,
        trim(replace(replace(replace(keywords, '[', ' '), ']', ' '), '"', ' ')),
        trim(replace(replace(replace(entities, '[', ' '), ']', ' '), '"', ' '))
    FROM memories
"#;

fn fts_state(ctx: &DbContext) -> Result<(i64, Option<i64>), RepairError> {
    let mem_count: i64 = ctx
        .conn
        .query_row("SELECT COUNT(*) FROM memories", [], |r| r.get(0))
        .map_err(RepairError::from)?;

    // Detect existence of memories_fts virtual table.
    let exists: i64 = ctx
        .conn
        .query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='memories_fts'",
            [],
            |r| r.get(0),
        )
        .map_err(RepairError::from)?;

    if exists == 0 {
        return Ok((mem_count, None));
    }

    let fts_count: i64 = ctx
        .conn
        .query_row("SELECT COUNT(*) FROM memories_fts", [], |r| r.get(0))
        .map_err(RepairError::from)?;
    Ok((mem_count, Some(fts_count)))
}

impl RepairRule for FtsRebuild {
    fn id(&self) -> &'static str {
        "R1"
    }

    fn name(&self) -> &'static str {
        "FTS rebuild"
    }

    fn dry_run(&self, ctx: &mut DbContext) -> Result<RuleReport, RepairError> {
        let mut r = RuleReport::new(self.id(), self.name(), ctx.label.clone());
        let (mem, fts_opt) = fts_state(ctx)?;
        match fts_opt {
            None => {
                r.findings.push(
                    Finding::new("fts_table_missing", 1).with_detail(json!({
                        "memories": mem,
                    })),
                );
            }
            Some(fts) if fts != mem => {
                let drift = (mem - fts).abs() as usize;
                r.findings.push(
                    Finding::new("fts_drift", drift).with_detail(json!({
                        "memories": mem,
                        "fts": fts,
                        "delta": mem - fts,
                    })),
                );
            }
            _ => {}
        }
        Ok(r)
    }

    fn apply(&self, ctx: &mut DbContext) -> Result<RuleReport, RepairError> {
        let mut r = self.dry_run(ctx)?;
        if r.findings.is_empty() {
            return Ok(r);
        }
        // Drop + recreate.
        ctx.conn
            .execute_batch("DROP TABLE IF EXISTS memories_fts;")?;
        ctx.conn.execute_batch(FTS_CREATE)?;
        let inserted = ctx.conn.execute(FTS_INSERT, [])?;
        let (mem, fts_opt) = fts_state(ctx)?;
        if fts_opt != Some(mem) {
            r.errors.push(format!(
                "post-rebuild count mismatch: memories={} fts={:?}",
                mem, fts_opt
            ));
        } else {
            r.applied = inserted;
        }
        Ok(r)
    }
}
