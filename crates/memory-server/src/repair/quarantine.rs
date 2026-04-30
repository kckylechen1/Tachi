//! R3 — Quarantine resolution.
//!
//! PR-3 v4 migration moved cross-DB-polluted rows to /_quarantine/cross-db/...
//! and stamped `metadata.quarantine = { reason, original_path, detected_at,
//! expected_db, actual_db }`. This module exposes:
//!   - sweep mode (used by `tachi repair`): just reports a count of
//!     quarantined rows per DB.
//!   - `tachi repair quarantine list`           — full inventory
//!   - `tachi repair quarantine restore --id …` — same-DB restore to original_path
//!   - `tachi repair quarantine restore-all --to-db <label>`
//!         — bulk cross-DB physical move (INSERT into dest → verify → DELETE from src)
//!   - `tachi repair quarantine purge --older-than <days>`

use std::path::PathBuf;

use chrono::{DateTime, Duration, Utc};
use rusqlite::{params, params_from_iter, Connection};
use serde_json::{json, Value};

use crate::manifest::Manifest;

use super::{
    inventory::{label_for, resolve_one, select_dbs},
    DbContext, Finding, RepairError, RepairExit, RepairRule, RuleReport,
};

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

// ─── Subcommands ─────────────────────────────────────────────────────────────

#[derive(Debug)]
struct QRow {
    id: String,
    path: String,
    original_path: String,
    expected_db: String,
    actual_db: String,
    detected_at: String,
    db_label: String,
    db_path: PathBuf,
}

fn collect_quarantined(manifest: &Manifest) -> Result<Vec<QRow>, Box<dyn std::error::Error>> {
    let mut out = Vec::new();
    for entry in select_dbs(manifest, None) {
        let conn = match Connection::open(&entry.path) {
            Ok(c) => c,
            Err(_) => continue,
        };
        let mut stmt = match conn.prepare(
            "SELECT id, path,
                    COALESCE(json_extract(metadata, '$.quarantine.original_path'), ''),
                    COALESCE(json_extract(metadata, '$.quarantine.expected_db'), ''),
                    COALESCE(json_extract(metadata, '$.quarantine.actual_db'), ''),
                    COALESCE(json_extract(metadata, '$.quarantine.detected_at'), '')
             FROM memories
             WHERE path LIKE '/_quarantine/%'",
        ) {
            Ok(s) => s,
            Err(_) => continue,
        };
        let label = label_for(&entry);
        let dbp = PathBuf::from(&entry.path);
        let rows = stmt
            .query_map([], |row| {
                Ok(QRow {
                    id: row.get(0)?,
                    path: row.get(1)?,
                    original_path: row.get(2)?,
                    expected_db: row.get(3)?,
                    actual_db: row.get(4)?,
                    detected_at: row.get(5)?,
                    db_label: label.clone(),
                    db_path: dbp.clone(),
                })
            })?
            .collect::<Result<Vec<_>, _>>()?;
        out.extend(rows);
    }
    Ok(out)
}

pub fn cmd_list(manifest: &Manifest, json_out: bool) -> Result<(), Box<dyn std::error::Error>> {
    let rows = collect_quarantined(manifest)?;
    if json_out {
        let body = json!({
            "total": rows.len(),
            "rows": rows.iter().map(|q| json!({
                "id": q.id,
                "db": q.db_label,
                "path": q.path,
                "original_path": q.original_path,
                "expected_db": q.expected_db,
                "actual_db": q.actual_db,
                "detected_at": q.detected_at,
            })).collect::<Vec<_>>()
        });
        println!("{}", serde_json::to_string_pretty(&body)?);
        return Ok(());
    }
    if rows.is_empty() {
        println!(
            "[OK] no quarantined rows found across {} manifest DB(s).",
            manifest.dbs.len()
        );
        return Ok(());
    }
    println!("Quarantined rows: {}", rows.len());
    for q in &rows {
        println!(
            "  {id}  in_db={db} expected={exp}  {path} <- {orig}",
            id = q.id,
            db = q.db_label,
            exp = q.expected_db,
            path = q.path,
            orig = q.original_path,
        );
    }
    Ok(())
}

pub fn cmd_restore(
    manifest: &Manifest,
    id: &str,
    apply: bool,
    json_out: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    let rows = collect_quarantined(manifest)?;
    let target = match rows.iter().find(|q| q.id == id) {
        Some(t) => t,
        None => {
            return Err(format!("no quarantined row with id={id}").into());
        }
    };
    if target.original_path.is_empty() {
        return Err(format!("row {id} has no original_path in metadata").into());
    }
    let action = json!({
        "id": id,
        "db": target.db_label,
        "from": target.path,
        "to": target.original_path,
        "apply": apply,
    });
    if !apply {
        if json_out {
            println!("{}", serde_json::to_string_pretty(&action)?);
        } else {
            println!(
                "[!] DRY-RUN: would restore {} in {}: {} -> {}",
                id, target.db_label, target.path, target.original_path
            );
        }
        return Ok(());
    }
    let mut conn = Connection::open(&target.db_path)?;
    let tx = conn.transaction()?;
    tx.execute(
        "UPDATE memories
         SET path = ?1,
             metadata = json_remove(metadata, '$.quarantine')
         WHERE id = ?2",
        params![target.original_path, id],
    )?;
    if let Err(e) = tx.execute(
        "UPDATE memories_fts SET path = ?1 WHERE id = ?2",
        params![target.original_path, id],
    ) {
        eprintln!("warning: failed to update FTS for restored quarantine row {id}: {e}");
    }
    tx.commit()?;
    if json_out {
        println!("{}", serde_json::to_string_pretty(&action)?);
    } else {
        println!(
            "[OK] restored {} in {}: {} -> {}",
            id, target.db_label, target.path, target.original_path
        );
    }
    Ok(())
}

pub fn cmd_restore_all(
    manifest: &Manifest,
    to_db: &str,
    apply: bool,
    json_out: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    let dest_entry = match resolve_one(manifest, to_db) {
        Some(e) => e,
        None => {
            return Err(
                format!("destination '{to_db}' did not resolve to a unique manifest DB").into(),
            )
        }
    };
    let dest_canonical = std::fs::canonicalize(&dest_entry.path)
        .map(|p| p.display().to_string())
        .unwrap_or_else(|_| dest_entry.path.clone());

    let rows = collect_quarantined(manifest)?;

    // Filter: rows whose expected_db canonicalizes to the destination.
    let mut moves: Vec<&QRow> = rows
        .iter()
        .filter(|q| {
            let canon = std::fs::canonicalize(&q.expected_db)
                .map(|p| p.display().to_string())
                .unwrap_or_else(|_| q.expected_db.clone());
            canon == dest_canonical
        })
        .collect();

    // Skip rows already in the destination DB (would be same-DB restore — call cmd_restore).
    moves.retain(|q| {
        let q_canon = std::fs::canonicalize(&q.db_path)
            .map(|p| p.display().to_string())
            .unwrap_or_else(|_| q.db_path.display().to_string());
        q_canon != dest_canonical
    });

    if moves.is_empty() {
        let msg = format!(
            "[OK] no quarantined rows match expected_db={} (resolved={})",
            to_db, dest_canonical
        );
        if json_out {
            println!(
                "{}",
                json!({"moved": 0, "dest": dest_entry.path, "apply": apply, "note": msg})
            );
        } else {
            println!("{msg}");
        }
        return Ok(());
    }

    if !apply {
        if json_out {
            let body = json!({
                "dest": dest_entry.path,
                "candidates": moves.len(),
                "apply": false,
                "rows": moves.iter().map(|q| json!({
                    "id": q.id,
                    "from_db": q.db_label,
                    "original_path": q.original_path,
                })).collect::<Vec<_>>(),
            });
            println!("{}", serde_json::to_string_pretty(&body)?);
        } else {
            println!(
                "[!] DRY-RUN: would move {} quarantined row(s) -> {}",
                moves.len(),
                dest_entry.path
            );
            for q in &moves {
                println!(
                    "       {id}  src={src} -> dest={dst} (path={orig})",
                    id = q.id,
                    src = q.db_label,
                    dst = label_for(&dest_entry),
                    orig = q.original_path
                );
            }
            println!("Re-run with --apply to perform the cross-DB move.");
        }
        return Ok(());
    }

    // ── apply ──
    let mut dest_conn = Connection::open(&dest_entry.path)?;
    let mut moved = 0usize;
    let mut errors: Vec<String> = Vec::new();

    // Group source rows by source DB so we open each src conn once.
    use std::collections::HashMap;
    let mut by_src: HashMap<PathBuf, Vec<&QRow>> = HashMap::new();
    for q in &moves {
        by_src.entry(q.db_path.clone()).or_default().push(*q);
    }

    for (src_path, group) in by_src {
        let mut src_conn = match Connection::open(&src_path) {
            Ok(c) => c,
            Err(e) => {
                errors.push(format!("open src {}: {e}", src_path.display()));
                continue;
            }
        };
        for q in group {
            if let Err(e) = move_one(&mut src_conn, &mut dest_conn, q) {
                errors.push(format!("move {}: {e}", q.id));
            } else {
                moved += 1;
            }
        }
    }

    let body = json!({
        "dest": dest_entry.path,
        "moved": moved,
        "errors": errors,
        "apply": true,
    });
    if json_out {
        println!("{}", serde_json::to_string_pretty(&body)?);
    } else {
        println!(
            "[OK] moved {} quarantined row(s) -> {}",
            moved, dest_entry.path
        );
        for e in &errors {
            println!("  [X] {e}");
        }
    }
    if !errors.is_empty() {
        return Err(Box::new(RepairExit::new(2)));
    }
    Ok(())
}

/// Cross-DB physical move of one quarantined row.
/// Strategy: SELECT FROM source → INSERT into dest → verify → DELETE source.
/// This is intentionally loss-safe, not cross-DB atomic: a crash after the
/// destination commit but before the source delete can duplicate a row.
fn move_one(
    src_conn: &mut Connection,
    dest_conn: &mut Connection,
    q: &QRow,
) -> Result<(), Box<dyn std::error::Error>> {
    // Pull all known columns from the source row. Use a flexible approach:
    // read column names dynamically so we handle schema drift gracefully.
    let cols: Vec<String> = {
        let mut stmt = src_conn.prepare("PRAGMA table_info(memories)")?;
        let v = stmt
            .query_map([], |row| row.get::<_, String>(1))?
            .collect::<Result<Vec<_>, _>>()?;
        v
    };
    if cols.is_empty() {
        return Err("source memories table has no columns".into());
    }
    let col_list = cols.join(", ");
    let placeholders = (1..=cols.len())
        .map(|i| format!("?{i}"))
        .collect::<Vec<_>>()
        .join(", ");

    let select_sql = format!("SELECT {} FROM memories WHERE id = ?1", col_list);
    let mut stmt = src_conn.prepare(&select_sql)?;
    let values: Vec<rusqlite::types::Value> = stmt.query_row([&q.id], |row| {
        let mut v = Vec::with_capacity(cols.len());
        for i in 0..cols.len() {
            v.push(row.get::<_, rusqlite::types::Value>(i)?);
        }
        Ok(v)
    })?;
    drop(stmt);

    // Restore original_path and strip the quarantine block from metadata BEFORE insert.
    let mut values = values;
    let id_idx = cols.iter().position(|c| c == "id");
    let path_idx = cols.iter().position(|c| c == "path");
    let meta_idx = cols.iter().position(|c| c == "metadata");
    if let Some(p) = path_idx {
        values[p] = rusqlite::types::Value::Text(q.original_path.clone());
    }
    if let Some(m) = meta_idx {
        if let rusqlite::types::Value::Text(ref s) = values[m] {
            if let Ok(mut v) = serde_json::from_str::<Value>(s) {
                if let Some(obj) = v.as_object_mut() {
                    obj.remove("quarantine");
                }
                values[m] = rusqlite::types::Value::Text(v.to_string());
            }
        }
    }

    // Dest transaction: insert + verify count.
    let dest_tx = dest_conn.transaction()?;
    let insert_sql = format!(
        "INSERT INTO memories ({}) VALUES ({})",
        col_list, placeholders
    );
    let id_value = id_idx
        .and_then(|i| match &values[i] {
            rusqlite::types::Value::Text(s) => Some(s.clone()),
            _ => None,
        })
        .unwrap_or_else(|| q.id.clone());
    dest_tx.execute(&insert_sql, params_from_iter(values.iter()))?;
    dest_tx.execute("DELETE FROM memories_fts WHERE id = ?1", params![&id_value])?;
    dest_tx.execute(
        "INSERT INTO memories_fts(id, path, summary, text, keywords, entities)
         SELECT id, path, summary, text,
                trim(replace(replace(replace(keywords, '[', ' '), ']', ' '), '\"', ' ')),
                trim(replace(replace(replace(entities, '[', ' '), ']', ' '), '\"', ' '))
         FROM memories WHERE id = ?1",
        params![&id_value],
    )?;
    let n: i64 = dest_tx.query_row(
        "SELECT COUNT(*) FROM memories WHERE id = ?1",
        params![&id_value],
        |row| row.get(0),
    )?;
    if n != 1 {
        return Err(format!("post-insert verify failed (count={n})").into());
    }
    dest_tx.commit()?;

    // Source transaction: delete.
    let src_tx = src_conn.transaction()?;
    src_tx.execute("DELETE FROM memories WHERE id = ?1", params![&q.id])?;
    if let Err(e) = src_tx.execute("DELETE FROM memories_fts WHERE id = ?1", params![&q.id]) {
        eprintln!(
            "warning: failed to delete FTS row for moved quarantine row {}: {e}",
            q.id
        );
    }
    src_tx.commit()?;

    Ok(())
}

pub fn cmd_purge(
    manifest: &Manifest,
    older_than_days: u64,
    apply: bool,
    json_out: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    let cutoff: DateTime<Utc> = Utc::now() - Duration::days(older_than_days as i64);
    let cutoff_iso = cutoff.to_rfc3339();
    let rows = collect_quarantined(manifest)?;

    let stale: Vec<&QRow> = rows
        .iter()
        .filter(|q| !q.detected_at.is_empty() && q.detected_at < cutoff_iso)
        .collect();

    if stale.is_empty() {
        let msg = format!(
            "[OK] no quarantined rows older than {} day(s) (cutoff={})",
            older_than_days, cutoff_iso
        );
        if json_out {
            println!(
                "{}",
                json!({"purged": 0, "cutoff": cutoff_iso, "apply": apply})
            );
        } else {
            println!("{msg}");
        }
        return Ok(());
    }

    if !apply {
        if json_out {
            let body = json!({
                "cutoff": cutoff_iso,
                "candidates": stale.len(),
                "apply": false,
            });
            println!("{}", serde_json::to_string_pretty(&body)?);
        } else {
            println!(
                "[!] DRY-RUN: would purge {} quarantined row(s) older than {} day(s)",
                stale.len(),
                older_than_days
            );
        }
        return Ok(());
    }

    // Group by src DB and delete.
    use std::collections::HashMap;
    let mut by_src: HashMap<PathBuf, Vec<&QRow>> = HashMap::new();
    for q in &stale {
        by_src.entry(q.db_path.clone()).or_default().push(*q);
    }
    let mut purged = 0usize;
    for (db_path, group) in by_src {
        let mut conn = Connection::open(&db_path)?;
        let tx = conn.transaction()?;
        for q in group {
            tx.execute("DELETE FROM memories WHERE id = ?1", params![&q.id])?;
            if let Err(e) = tx.execute("DELETE FROM memories_fts WHERE id = ?1", params![&q.id]) {
                eprintln!(
                    "warning: failed to delete FTS row for purged quarantine row {}: {e}",
                    q.id
                );
            }
            purged += 1;
        }
        tx.commit()?;
    }
    if json_out {
        println!(
            "{}",
            json!({"purged": purged, "cutoff": cutoff_iso, "apply": true})
        );
    } else {
        println!("[OK] purged {} quarantined row(s).", purged);
    }
    Ok(())
}
