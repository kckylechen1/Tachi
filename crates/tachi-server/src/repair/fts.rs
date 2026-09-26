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

// One rule for every FTS projection writer and drift count (tachi#1993):
// a NULL-id memory is never projected. `memories.id` is `TEXT PRIMARY KEY`
// without NOT NULL, so legacy rows can carry NULL; such a row can never join
// back to an FTS hit (`m.id = <fts>.id`), and memcore's open-time orphan pass
// deletes every NULL-id projection row. Projecting it here would make
// repair -> open -> repair oscillate, with a generation bump each time.
const FTS_INSERT: &str = r#"
    INSERT INTO memories_fts (id, path, summary, text, keywords, entities)
    SELECT
        id, path, summary, text,
        trim(replace(replace(replace(keywords, '[', ' '), ']', ' '), '"', ' ')),
        trim(replace(replace(replace(entities, '[', ' '), ']', ' '), '"', ' '))
    FROM memories
    WHERE id IS NOT NULL
"#;

/// Membership drift between the live memories and one FTS projection.
///
/// Net row counts can cancel: a NULL-id or orphan projection row offsets a
/// live memory with no projection, so `COUNT(*)` on both sides matches while
/// that memory stays unsearchable (tachi#2000 review, astra r2). Every class
/// is therefore counted on its own, and any non-zero class is drift. All
/// three at zero implies `rows == memories`, so this catches everything the
/// old net-count comparison did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ProjectionDrift {
    /// Live (non-NULL-id) memories: what the projection should hold, once
    /// each (see `FTS_INSERT`).
    memories: i64,
    /// Raw projection row count.
    rows: i64,
    /// Live memory ids with no projection row.
    missing: i64,
    /// Projection rows whose id is NULL or names no live memory: exactly the
    /// rows memcore's open-time orphan pass deletes.
    invalid: i64,
    /// Live ids projected more than once.
    duplicate: i64,
}

impl ProjectionDrift {
    fn drifted(&self) -> bool {
        self.missing > 0 || self.invalid > 0 || self.duplicate > 0
    }

    fn count(&self) -> usize {
        (self.missing + self.invalid + self.duplicate) as usize
    }
}

fn table_exists(ctx: &DbContext, table: &str) -> Result<bool, RepairError> {
    let exists: i64 = ctx
        .conn
        .query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name=?1",
            [table],
            |r| r.get(0),
        )
        .map_err(RepairError::from)?;
    Ok(exists > 0)
}

fn live_memory_count(ctx: &DbContext) -> Result<i64, RepairError> {
    ctx.conn
        .query_row(
            "SELECT COUNT(*) FROM memories WHERE id IS NOT NULL",
            [],
            |r| r.get(0),
        )
        .map_err(RepairError::from)
}

/// Membership drift of `table` (`memories_fts` or `memories_symbolic_fts`)
/// against the live memories; `None` when the table does not exist. Every
/// subquery is uncorrelated and carries `WHERE id IS NOT NULL`, so a NULL id
/// on either side can neither poison `NOT IN` nor force a per-row scan of
/// the FTS table (#1974 / #1985).
fn projection_drift(ctx: &DbContext, table: &str) -> Result<Option<ProjectionDrift>, RepairError> {
    if !table_exists(ctx, table)? {
        return Ok(None);
    }
    let count = |sql: String| -> Result<i64, RepairError> {
        ctx.conn
            .query_row(&sql, [], |r| r.get(0))
            .map_err(RepairError::from)
    };
    Ok(Some(ProjectionDrift {
        memories: live_memory_count(ctx)?,
        rows: count(format!("SELECT COUNT(*) FROM {table}"))?,
        missing: count(format!(
            "SELECT COUNT(*) FROM memories \
             WHERE id IS NOT NULL \
               AND id NOT IN (SELECT id FROM {table} WHERE id IS NOT NULL)"
        ))?,
        invalid: count(format!(
            "SELECT COUNT(*) FROM {table} \
             WHERE id IS NULL \
                OR id NOT IN (SELECT id FROM memories WHERE id IS NOT NULL)"
        ))?,
        duplicate: count(format!(
            "SELECT COUNT(*) FROM ( \
               SELECT id FROM {table} \
               WHERE id IN (SELECT id FROM memories WHERE id IS NOT NULL) \
               GROUP BY id HAVING COUNT(*) > 1)"
        ))?,
    }))
}

fn drift_finding(kind: &str, rows_key: &str, drift: &ProjectionDrift) -> Finding {
    let mut detail = json!({
        "memories": drift.memories,
        "delta": drift.memories - drift.rows,
        "missing": drift.missing,
        "invalid": drift.invalid,
        "duplicate": drift.duplicate,
    });
    detail[rows_key] = json!(drift.rows);
    Finding::new(kind, drift.count()).with_detail(detail)
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
        let orphan_shadows = orphan_shadow_count(ctx)?;
        match projection_drift(ctx, "memories_fts")? {
            None => {
                let mut detail = json!({
                    "memories": live_memory_count(ctx)?,
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
            Some(drift) if drift.drifted() => {
                r.findings.push(drift_finding("fts_drift", "fts", &drift));
            }
            Some(_) => {}
        }
        // A missing symbolic table (pre-v22 DB, or mid-migration) is out of
        // scope for R1, which only reconciles drift on a projection that is
        // already present.
        if let Some(drift) = projection_drift(ctx, "memories_symbolic_fts")? {
            if drift.drifted() {
                r.findings
                    .push(drift_finding("symbolic_fts_drift", "symbolic_fts", &drift));
            }
        }
        Ok(r)
    }

    fn apply(&self, ctx: &mut DbContext) -> Result<RuleReport, RepairError> {
        let mut r = self.dry_run(ctx)?;
        if r.findings.is_empty() {
            return Ok(r);
        }
        let symbolic_fts_present = table_exists(ctx, "memories_symbolic_fts")?;
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
        if !symbolic_fts_present {
            // Legacy DBs without the symbolic projection still changed the
            // primary FTS projection and therefore need one atomic bump.
            memcore::db::bump_search_generation(&tx)?;
        }
        tx.commit()?;
        match projection_drift(ctx, "memories_fts")? {
            Some(drift) if !drift.drifted() => r.applied = inserted,
            drift => r
                .errors
                .push(format!("post-rebuild memories_fts drift: {drift:?}")),
        }
        if let Some(drift) = projection_drift(ctx, "memories_symbolic_fts")? {
            if drift.drifted() {
                r.errors.push(format!(
                    "post-rebuild memories_symbolic_fts drift: {drift:?}"
                ));
            }
        }
        Ok(r)
    }
}
