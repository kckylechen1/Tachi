use std::path::Path;

use rusqlite::{params, Connection};

use crate::error::MemoryError;

use super::{now_utc_iso, SANITY_QUARANTINE_FRACTION};

/// Callers run this inside the caller's own transactional boundary — see
/// `run_data_migrations`'s outer transaction (#984 F1) — so this no longer
/// opens its own nested transaction (SQLite forbids nested top-level `BEGIN`;
/// the outer transaction already gives this loop atomicity).
pub(super) fn migrate_v4_quarantine_cross_db(
    conn: &Connection,
    db_label: &str,
    current_db_path: &Path,
) -> Result<(usize, bool), MemoryError> {
    let total: i64 = conn.query_row("SELECT COUNT(*) FROM memories", [], |r| r.get(0))?;
    if total == 0 {
        return Ok((0, false));
    }

    let canonical_self = std::fs::canonicalize(current_db_path)
        .ok()
        .map(|p| p.display().to_string())
        .unwrap_or_else(|| current_db_path.display().to_string());

    let candidate_count = count_cross_db_candidates(conn, &canonical_self)?;
    if candidate_count == 0 {
        return Ok((0, false));
    }

    // 50%-row sanity guard.
    let total_f = total as f64;
    if (candidate_count as f64) / total_f > SANITY_QUARANTINE_FRACTION {
        eprintln!(
            "warning: v4_quarantine_cross_db_rows: would move {} of {} rows (>50%) in db_label={} path={}; aborting migration",
            candidate_count, total, db_label, canonical_self
        );
        return Ok((0, true));
    }

    let detected_at = now_utc_iso();
    let mut moved = 0usize;
    let mut after_id = String::new();
    loop {
        let rows = fetch_cross_db_candidate_batch(conn, &after_id, 500)?;
        if rows.is_empty() {
            break;
        }
        after_id = rows
            .last()
            .map(|(id, _, _, _)| id.clone())
            .unwrap_or(after_id);
        for (id, original_path, metadata_str, prov_db_path) in rows {
            if let Some(expected_db) = mismatched_provenance_db(&prov_db_path, &canonical_self) {
                let mut meta: serde_json::Value =
                    serde_json::from_str(&metadata_str).unwrap_or_else(|_| serde_json::json!({}));
                if !meta.is_object() {
                    meta = serde_json::json!({});
                }
                let original_suffix = if original_path.starts_with('/') {
                    original_path.clone()
                } else {
                    format!("/{original_path}")
                };
                let new_path = format!("/_quarantine/cross-db{original_suffix}");
                let q = serde_json::json!({
                    "reason": "cross_db_pollution",
                    "original_path": original_path,
                    "detected_at": detected_at,
                    "expected_db": expected_db,
                    "actual_db": canonical_self,
                });
                if let Some(obj) = meta.as_object_mut() {
                    obj.insert("quarantine".into(), q);
                }
                let new_meta = serde_json::to_string(&meta)?;
                conn.execute(
                    "UPDATE memories SET path = ?1, metadata = ?2 WHERE id = ?3",
                    params![new_path, new_meta, id],
                )?;
                let quarantine_path = format!("/_quarantine/cross-db{original_suffix}");
                if let Err(e) = conn.execute(
                    "UPDATE memories_fts SET path = ?1 WHERE id = ?2",
                    params![&quarantine_path, id],
                ) {
                    eprintln!("warning: failed to update FTS for quarantined row {id}: {e}");
                }
                if let Err(e) = conn.execute(
                    "UPDATE memories_symbolic_fts SET path = ?1 WHERE id = ?2",
                    params![&quarantine_path, id],
                ) {
                    eprintln!(
                        "warning: failed to update symbolic FTS for quarantined row {id}: {e}"
                    );
                }
                moved += 1;
            }
        }
    }
    Ok((moved, false))
}

fn count_cross_db_candidates(
    conn: &Connection,
    canonical_self: &str,
) -> Result<usize, MemoryError> {
    let mut count = 0usize;
    let mut after_id = String::new();
    loop {
        let rows = fetch_cross_db_candidate_batch(conn, &after_id, 500)?;
        if rows.is_empty() {
            break;
        }
        after_id = rows
            .last()
            .map(|(id, _, _, _)| id.clone())
            .unwrap_or(after_id);
        for (_, _, _, prov_db_path) in rows {
            if mismatched_provenance_db(&prov_db_path, canonical_self).is_some() {
                count += 1;
            }
        }
    }
    Ok(count)
}

fn fetch_cross_db_candidate_batch(
    conn: &Connection,
    after_id: &str,
    limit: usize,
) -> Result<Vec<(String, String, String, String)>, MemoryError> {
    let mut stmt = conn.prepare(
        "SELECT id, path, metadata,
                COALESCE(json_extract(metadata, '$.provenance.db_path'), '')
         FROM memories
         WHERE id > ?1
           AND path NOT LIKE '/_quarantine%'
           AND metadata IS NOT NULL
           AND json_extract(metadata, '$.provenance.db_path') IS NOT NULL
         ORDER BY id
         LIMIT ?2",
    )?;
    let rows = stmt
        .query_map(params![after_id, limit as i64], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
            ))
        })?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}

fn mismatched_provenance_db(prov_db_path: &str, canonical_self: &str) -> Option<String> {
    if prov_db_path.is_empty() {
        return None;
    }
    let prov_canonical = std::fs::canonicalize(prov_db_path)
        .ok()
        .map(|p| p.display().to_string())
        .unwrap_or_else(|| prov_db_path.to_string());
    if prov_canonical != canonical_self {
        Some(prov_canonical)
    } else {
        None
    }
}
