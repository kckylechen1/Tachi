use super::legacy::rewrite_legacy_expected_db;
use super::rows::{collect_quarantined, QRow};
use super::*;

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
    // Path is a symbolic-indexed column — refresh the trigram projection from
    // the live memories row rather than a partial UPDATE (#1335 oracle).
    // Propagate sync failure so the memories UPDATE cannot commit without a
    // valid symbolic row (#1335 oracle NOT-READY).
    memcore::db::sync_memories_symbolic_fts(&tx, id)?;
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
    //
    // B5: `expected_db` may point at a stale path that no longer exists on
    // disk (e.g. the historical `memory-hybrid-bridge` location). We first
    // run it through `rewrite_legacy_expected_db` so the canonicalize step
    // resolves against the modern home of the same DB before comparing.
    let mut moves: Vec<&QRow> = rows
        .iter()
        .filter(|q| {
            let rewritten = rewrite_legacy_expected_db(&q.expected_db);
            let canon = std::fs::canonicalize(&rewritten)
                .map(|p| p.display().to_string())
                .unwrap_or(rewritten);
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
    // Restored inserts must land in the trigram index or symbolic recall
    // silently misses them (#1335 oracle).
    memcore::db::sync_memories_symbolic_fts(&dest_tx, &id_value)?;
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
    // Propagate symbolic delete failure so source-row DELETE cannot commit
    // while leaving a stale trigram projection (#1335 oracle NOT-READY).
    memcore::db::delete_memories_symbolic_fts(&src_tx, &q.id)?;
    src_tx.commit()?;

    Ok(())
}
