//! R1 — FTS rebuild. Detects drift between `memories` and `memories_fts`,
//! and a missing `memories_fts` virtual table altogether (the quant case).
//! Also detects drift in the `memories_symbolic_fts` trigram projection and
//! rebuilds it in the same pass, reusing memcore's v22 full-rebuild helper
//! (#1335 oracle: repair writers must not desync the trigram index).

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

/// Same shape as `fts_state` but for the `memories_symbolic_fts` trigram
/// projection. Returns `None` for the symbolic count when the table doesn't
/// exist (pre-v22 DB, or mid-migration) — that is out of scope for R1, which
/// only reconciles drift on a projection that is already present.
fn symbolic_fts_state(ctx: &DbContext) -> Result<(i64, Option<i64>), RepairError> {
    let mem_count: i64 = ctx
        .conn
        .query_row("SELECT COUNT(*) FROM memories", [], |r| r.get(0))
        .map_err(RepairError::from)?;

    let exists: i64 = ctx
        .conn
        .query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='memories_symbolic_fts'",
            [],
            |r| r.get(0),
        )
        .map_err(RepairError::from)?;
    if exists == 0 {
        return Ok((mem_count, None));
    }

    let symbolic_count: i64 = ctx
        .conn
        .query_row("SELECT COUNT(*) FROM memories_symbolic_fts", [], |r| {
            r.get(0)
        })
        .map_err(RepairError::from)?;
    Ok((mem_count, Some(symbolic_count)))
}

/// Count orphaned FTS5 shadow tables (those whose virtual parent
/// `memories_fts` no longer exists). When > 0, a naive
/// `CREATE VIRTUAL TABLE memories_fts` will fail with
/// "table memories_fts_data already exists".
fn orphan_shadow_count(ctx: &DbContext) -> Result<i64, RepairError> {
    let count: i64 = ctx
        .conn
        .query_row(
            "SELECT COUNT(*) FROM sqlite_master \
             WHERE type='table' AND name LIKE 'memories_fts\\_%' ESCAPE '\\'",
            [],
            |r| r.get(0),
        )
        .map_err(RepairError::from)?;
    Ok(count)
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
        let orphan_shadows = orphan_shadow_count(ctx)?;
        match fts_opt {
            None => {
                let mut detail = json!({
                    "memories": mem,
                    "orphan_shadow_tables": orphan_shadows,
                });
                let kind = if orphan_shadows > 0 {
                    // Surface the actual blocking condition so the user sees
                    // what `apply` is going to clean up before the rebuild.
                    detail["note"] = json!(
                        "orphan FTS5 shadow tables detected; \
                         apply will DROP them before recreating memories_fts"
                    );
                    "fts_table_missing_with_orphan_shadows"
                } else {
                    "fts_table_missing"
                };
                r.findings.push(Finding::new(kind, 1).with_detail(detail));
            }
            Some(fts) if fts != mem => {
                let drift = (mem - fts).abs() as usize;
                r.findings
                    .push(Finding::new("fts_drift", drift).with_detail(json!({
                        "memories": mem,
                        "fts": fts,
                        "delta": mem - fts,
                    })));
            }
            _ => {}
        }
        let (mem, symbolic_opt) = symbolic_fts_state(ctx)?;
        if let Some(symbolic) = symbolic_opt {
            if symbolic != mem {
                let drift = (mem - symbolic).abs() as usize;
                r.findings.push(
                    Finding::new("symbolic_fts_drift", drift).with_detail(json!({
                        "memories": mem,
                        "symbolic_fts": symbolic,
                        "delta": mem - symbolic,
                    })),
                );
            }
        }
        Ok(r)
    }

    fn apply(&self, ctx: &mut DbContext) -> Result<RuleReport, RepairError> {
        let mut r = self.dry_run(ctx)?;
        if r.findings.is_empty() {
            return Ok(r);
        }
        // Drop + recreate atomically so a crash cannot leave the DB without FTS.
        //
        // We must defensively drop the FTS5 shadow tables as well: a previous
        // half-applied DROP / aborted CREATE can leave `memories_fts_data` (and
        // friends) orphaned without the parent virtual table, in which case
        // `CREATE VIRTUAL TABLE memories_fts` fails with
        // `error creating shadow table memories_fts_data: table ... already exists`
        // and the DB is stuck — the exact `project:quant` symptom.
        let tx = ctx.conn.transaction()?;
        tx.execute_batch("DROP TABLE IF EXISTS memories_fts;")?;
        for shadow in [
            "memories_fts_data",
            "memories_fts_idx",
            "memories_fts_docsize",
            "memories_fts_config",
            "memories_fts_content",
        ] {
            // These are real tables once the virtual parent is gone, so a plain
            // DROP TABLE works. IF EXISTS keeps the healthy path a no-op.
            tx.execute_batch(&format!("DROP TABLE IF EXISTS {shadow};"))?;
        }
        tx.execute_batch(FTS_CREATE)?;
        let inserted = tx.execute(FTS_INSERT, [])?;
        // Reuse memcore's v22 full-rebuild helper (no-op if the table is
        // absent) so the trigram projection is reconciled atomically with
        // `memories_fts` — same transaction, same crash-safety guarantee
        // (#1335 oracle: repair rebuild previously only covered memories_fts).
        memcore::db::migrations::rebuild_memories_symbolic_fts(&tx)?;
        tx.commit()?;
        let (mem, fts_opt) = fts_state(ctx)?;
        if fts_opt != Some(mem) {
            r.errors.push(format!(
                "post-rebuild count mismatch: memories={} fts={:?}",
                mem, fts_opt
            ));
        } else {
            r.applied = inserted;
        }
        let (mem, symbolic_opt) = symbolic_fts_state(ctx)?;
        if let Some(symbolic) = symbolic_opt {
            if symbolic != mem {
                r.errors.push(format!(
                    "post-rebuild symbolic count mismatch: memories={} symbolic_fts={}",
                    mem, symbolic
                ));
            }
        }
        Ok(r)
    }
}
