//! R11 — Plan C split-brain repair.
//!
//! Plan C makes `<repo>/.tachi/tachi-memory.db` canonical and keeps
//! `<tachi_home>/projects/<repo>/tachi-memory.db` as a symlink alias.
//! Historical setups may have a regular SQLite file at the alias path (or
//! still carry the pre-#1132 `memory.db` filename); this rule merges
//! alias-only rows into the canonical DB and restores the symlink.

use std::collections::HashSet;
use std::path::Path;

use rusqlite::Transaction;
use serde_json::json;

use super::{backup_db, fts::FtsRebuild, DbContext, Finding, RepairError, RepairRule, RuleReport};

const ALIAS_SCHEMA: &str = "plan_c_alias";

pub struct PlanCRepair {
    pub backup_alias: bool,
}

#[derive(Default)]
struct PlanCMergeStats {
    memories: usize,
    memory_vectors: usize,
    memory_edges: usize,
    access_history: usize,
    agent_known_state: usize,
    processed_events: usize,
    derived_items: usize,
    fts_rows: usize,
}

impl PlanCMergeStats {
    fn applied_total(&self) -> usize {
        self.memories
            + self.memory_vectors
            + self.memory_edges
            + self.access_history
            + self.agent_known_state
            + self.processed_events
            + self.derived_items
            + self.fts_rows
    }
}

impl RepairRule for PlanCRepair {
    fn id(&self) -> &'static str {
        "R11"
    }

    fn name(&self) -> &'static str {
        "Plan C split-brain repair"
    }

    fn dry_run(&self, ctx: &mut DbContext) -> Result<RuleReport, RepairError> {
        let mut report = RuleReport::new(self.id(), self.name(), ctx.label.clone());
        if let Some(issue) = plan_c_split_brain_or_refuse(&ctx.path)? {
            report.findings.push(
                Finding::new("plan_c_split_brain", 1).with_detail(json!({
                    "project_name": issue.project_name,
                    "canonical_db": issue.canonical_db.display().to_string(),
                    "alias_db": issue.alias_db.display().to_string(),
                    "canonical_rows": issue.canonical_rows,
                    "alias_rows": issue.alias_rows,
                    "canonical_bytes": issue.canonical_bytes,
                    "alias_bytes": issue.alias_bytes,
                    "repair_command": format!("tachi repair --rule R11 --apply --db {}", ctx.label),
                    "policy": "canonical rows win on id conflict; alias-only rows are copied before the alias file is replaced with a symlink",
                })),
            );
        }
        Ok(report)
    }

    fn apply(&self, ctx: &mut DbContext) -> Result<RuleReport, RepairError> {
        let mut report = self.dry_run(ctx)?;
        let Some(issue) = plan_c_split_brain_or_refuse(&ctx.path)? else {
            return Ok(report);
        };

        #[cfg(not(unix))]
        {
            report.errors.push(
                "Plan C alias repair requires symlink support; this host is not Unix".to_string(),
            );
            return Ok(report);
        }

        #[cfg(unix)]
        {
            let alias_backup = if self.backup_alias {
                match backup_db(&issue.alias_db) {
                    Ok(path) => Some(path),
                    Err(err) => {
                        report.errors.push(format!(
                            "alias backup failed for {}: {err}",
                            issue.alias_db.display()
                        ));
                        return Ok(report);
                    }
                }
            } else {
                None
            };

            let mut stats = merge_alias_into_canonical(ctx, &issue.alias_db)?;
            if stats.memories > 0 {
                match FtsRebuild.apply(ctx) {
                    Ok(fts) => {
                        stats.fts_rows = fts.applied;
                    }
                    Err(err) => {
                        report
                            .errors
                            .push(format!("FTS rebuild after Plan C merge failed: {err}"));
                        return Ok(report);
                    }
                }
            }

            replace_alias_with_symlink(&issue.alias_db, &ctx.path)?;
            report.applied = stats.applied_total() + 1;
            report.findings.push(
                Finding::new("plan_c_alias_relinked", 1).with_detail(json!({
                    "project_name": issue.project_name,
                    "canonical_db": ctx.path.display().to_string(),
                    "alias_db": issue.alias_db.display().to_string(),
                    "alias_backup": alias_backup.map(|path| path.display().to_string()),
                    "merged": {
                        "memories": stats.memories,
                        "memory_vectors": stats.memory_vectors,
                        "memory_edges": stats.memory_edges,
                        "access_history": stats.access_history,
                        "agent_known_state": stats.agent_known_state,
                        "processed_events": stats.processed_events,
                        "derived_items": stats.derived_items,
                        "fts_rows": stats.fts_rows,
                    },
                    "conflict_policy": "existing canonical memory ids were kept; alias duplicates remain available in the backup",
                })),
            );
            Ok(report)
        }
    }
}

fn plan_c_split_brain_or_refuse(
    local_db: &Path,
) -> Result<Option<crate::path_utils::PlanCSplitBrain>, RepairError> {
    match crate::path_utils::inspect_plan_c_alias_for_local_db(local_db) {
        crate::path_utils::PlanCAliasInspection::SplitBrain(issue) => Ok(Some(issue)),
        crate::path_utils::PlanCAliasInspection::Integrity(issue) => Err(RepairError::Io(
            std::io::Error::other(issue.warning_message()),
        )),
        crate::path_utils::PlanCAliasInspection::Absent
        | crate::path_utils::PlanCAliasInspection::MatchingSymlink => Ok(None),
    }
}

#[cfg(unix)]
fn replace_alias_with_symlink(alias_db: &Path, canonical_db: &Path) -> Result<(), RepairError> {
    if alias_db.is_symlink() || alias_db.exists() {
        std::fs::remove_file(alias_db)?;
    }
    std::os::unix::fs::symlink(canonical_db, alias_db)?;
    Ok(())
}

#[cfg(unix)]
fn merge_alias_into_canonical(
    ctx: &mut DbContext,
    alias_db: &Path,
) -> Result<PlanCMergeStats, RepairError> {
    let alias_path = alias_db.to_string_lossy().to_string();
    ctx.conn
        .execute("ATTACH DATABASE ?1 AS plan_c_alias", [&alias_path])?;

    let merge_result = merge_attached_alias(ctx);
    let detach_result = ctx
        .conn
        .execute_batch("DETACH DATABASE plan_c_alias")
        .map_err(RepairError::from);

    match (merge_result, detach_result) {
        (Err(err), _) => Err(err),
        (Ok(_), Err(err)) => Err(err),
        (Ok(stats), Ok(())) => Ok(stats),
    }
}

#[cfg(unix)]
fn merge_attached_alias(ctx: &mut DbContext) -> Result<PlanCMergeStats, RepairError> {
    let tx = ctx.conn.transaction()?;
    tx.execute_batch(
        "DROP TABLE IF EXISTS temp.plan_c_imported_ids;
         CREATE TEMP TABLE plan_c_imported_ids(id TEXT PRIMARY KEY);",
    )?;
    let stats = {
        let mut stats = PlanCMergeStats::default();
        stats.memories = seed_and_copy_missing_memories(&tx)?;
        stats.memory_vectors = copy_common_rows(
            &tx,
            "memories_vec",
            &["id"],
            "EXISTS (SELECT 1 FROM temp.plan_c_imported_ids i WHERE i.id = a.id)",
        )?;
        stats.memory_edges = copy_common_rows(
            &tx,
            "memory_edges",
            &["source_id", "target_id", "relation"],
            "(a.source_id IN (SELECT id FROM temp.plan_c_imported_ids)
                OR a.target_id IN (SELECT id FROM temp.plan_c_imported_ids))
             AND EXISTS (SELECT 1 FROM main.memories m WHERE m.id = a.source_id)
             AND EXISTS (SELECT 1 FROM main.memories m WHERE m.id = a.target_id)",
        )?;
        stats.access_history = copy_common_rows(
            &tx,
            "access_history",
            &["memory_id"],
            "EXISTS (SELECT 1 FROM temp.plan_c_imported_ids i WHERE i.id = a.memory_id)",
        )?;
        stats.agent_known_state = copy_common_rows(
            &tx,
            "agent_known_state",
            &["agent_id", "memory_id"],
            "EXISTS (SELECT 1 FROM temp.plan_c_imported_ids i WHERE i.id = a.memory_id)",
        )?;
        stats.processed_events = copy_common_rows(
            &tx,
            "processed_events",
            &["event_hash", "worker"],
            "NOT EXISTS (
                SELECT 1 FROM main.processed_events m
                WHERE m.event_hash = a.event_hash AND m.worker = a.worker
            )",
        )?;
        stats.derived_items = copy_common_rows(
            &tx,
            "derived_items",
            &["id"],
            "NOT EXISTS (SELECT 1 FROM main.derived_items m WHERE m.id = a.id)",
        )?;
        stats
    };
    tx.execute_batch("DROP TABLE IF EXISTS temp.plan_c_imported_ids;")?;
    tx.commit()?;
    Ok(stats)
}

#[cfg(unix)]
fn seed_and_copy_missing_memories(tx: &Transaction<'_>) -> Result<usize, RepairError> {
    if !has_table(tx, "main", "memories")? || !has_table(tx, ALIAS_SCHEMA, "memories")? {
        return Ok(0);
    }
    tx.execute(
        "INSERT OR IGNORE INTO temp.plan_c_imported_ids(id)
         SELECT a.id
         FROM plan_c_alias.memories a
         WHERE NOT EXISTS (SELECT 1 FROM main.memories m WHERE m.id = a.id)",
        [],
    )?;
    copy_common_rows(
        tx,
        "memories",
        &["id", "path", "text", "timestamp"],
        "EXISTS (SELECT 1 FROM temp.plan_c_imported_ids i WHERE i.id = a.id)",
    )
}

#[cfg(unix)]
fn copy_common_rows(
    tx: &Transaction<'_>,
    table: &str,
    required_columns: &[&str],
    where_sql: &str,
) -> Result<usize, RepairError> {
    if !has_table(tx, "main", table)? || !has_table(tx, ALIAS_SCHEMA, table)? {
        return Ok(0);
    }
    let main_columns = table_columns(tx, "main", table)?;
    let alias_columns = table_columns(tx, ALIAS_SCHEMA, table)?;
    let alias_set: HashSet<&str> = alias_columns.iter().map(String::as_str).collect();
    let columns: Vec<String> = main_columns
        .into_iter()
        .filter(|column| alias_set.contains(column.as_str()))
        .collect();
    if !required_columns
        .iter()
        .all(|column| columns.iter().any(|candidate| candidate == column))
    {
        return Ok(0);
    }
    let column_list = columns
        .iter()
        .map(|column| quote_ident(column))
        .collect::<Vec<_>>()
        .join(", ");
    let select_list = columns
        .iter()
        .map(|column| format!("a.{}", quote_ident(column)))
        .collect::<Vec<_>>()
        .join(", ");
    let sql = format!(
        "INSERT OR IGNORE INTO main.{table} ({column_list})
         SELECT {select_list}
         FROM plan_c_alias.{table} a
         WHERE {where_sql}",
        table = quote_ident(table)
    );
    Ok(tx.execute(&sql, [])?)
}

#[cfg(unix)]
fn has_table(tx: &Transaction<'_>, schema: &str, table: &str) -> Result<bool, RepairError> {
    let sql = format!(
        "SELECT 1 FROM {}.sqlite_master WHERE type IN ('table', 'view') AND name = ?1 LIMIT 1",
        quote_ident(schema)
    );
    Ok(tx
        .query_row(&sql, [table], |row| row.get::<_, i64>(0))
        .is_ok())
}

#[cfg(unix)]
fn table_columns(
    tx: &Transaction<'_>,
    schema: &str,
    table: &str,
) -> Result<Vec<String>, RepairError> {
    let sql = format!(
        "PRAGMA {}.table_info({})",
        quote_ident(schema),
        quote_ident(table)
    );
    let mut stmt = tx.prepare(&sql)?;
    let rows = stmt.query_map([], |row| row.get::<_, String>(1))?;
    let mut columns = Vec::new();
    for row in rows {
        columns.push(row?);
    }
    Ok(columns)
}

#[cfg(unix)]
fn quote_ident(ident: &str) -> String {
    format!("\"{}\"", ident.replace('"', "\"\""))
}
