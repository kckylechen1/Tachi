use chrono::{DateTime, Utc};
use rusqlite::types::Value;
use rusqlite::{params_from_iter, Connection};
use std::collections::{HashMap, HashSet};

use crate::error::MemoryError;

use super::{now_utc_iso, IN_BATCH_SIZE};

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct AccessUpdate {
    pub access_count: i64,
    pub last_access: Option<String>,
}

/// FNV-1a 32-bit hash of a query string for query_diversity tracking.
fn fnv1a_hash(s: &str) -> String {
    let mut hash: u32 = 2_166_136_261;
    for byte in s.bytes() {
        hash ^= byte as u32;
        hash = hash.wrapping_mul(16_777_619);
    }
    format!("{hash:08x}")
}

fn numbered_placeholders(start: usize, count: usize) -> String {
    (start..start + count)
        .map(|idx| format!("?{idx}"))
        .collect::<Vec<_>>()
        .join(",")
}

fn values_clause(start: usize, row_count: usize, width: usize) -> String {
    (0..row_count)
        .map(|row| {
            let first = start + row * width;
            let placeholders = numbered_placeholders(first, width);
            format!("({placeholders})")
        })
        .collect::<Vec<_>>()
        .join(",")
}

fn unique_id_order(ids: &[String]) -> Vec<&str> {
    let mut seen = HashSet::with_capacity(ids.len());
    ids.iter()
        .map(String::as_str)
        .filter(|id| seen.insert(*id))
        .collect()
}

/// Bump access_count and last_access for a list of IDs (called after every search).
/// `fts_hits` are the IDs matched by the FTS channel (get recall_count incremented).
/// `query` is the raw query string; its FNV-1a hash is stored in access_history and
/// used to compute `query_diversity` (distinct queries that reached this memory).
/// Applies a promotion gate: tier -> "consolidated" when recall_count >= 3,
/// query_diversity >= 3, and (importance >= 0.8 OR query_diversity >= 3).
#[cfg(test)]
pub(crate) fn record_access(
    conn: &Connection,
    ids: &[String],
    fts_hits: &[String],
    query: Option<&str>,
) -> Result<(), MemoryError> {
    record_access_with_updates(conn, ids, fts_hits, query).map(|_| ())
}

pub(crate) fn record_access_with_updates(
    conn: &Connection,
    ids: &[String],
    fts_hits: &[String],
    query: Option<&str>,
) -> Result<HashMap<String, AccessUpdate>, MemoryError> {
    if ids.is_empty() {
        return Ok(HashMap::new());
    }

    let now = now_utc_iso();
    let query_hash = query.map(fnv1a_hash).unwrap_or_default();
    // `record_access` is called from search paths that only hold `&Connection`.
    // The unchecked transaction keeps the access_count/history/recall updates
    // atomic without widening the public search API to require `&mut Connection`.
    let tx = conn.unchecked_transaction()?;

    let unique_ids = unique_id_order(ids);
    let mut existing_set = HashSet::with_capacity(unique_ids.len());
    for batch in unique_ids.chunks(IN_BATCH_SIZE) {
        let placeholders = numbered_placeholders(1, batch.len());
        let sql = format!("SELECT id FROM memories WHERE id IN ({placeholders})");
        let values = batch
            .iter()
            .map(|id| Value::Text((*id).to_string()))
            .collect::<Vec<_>>();
        let mut stmt = tx.prepare(&sql)?;
        let rows = stmt.query_map(params_from_iter(values.iter()), |row| {
            row.get::<_, String>(0)
        })?;
        for row in rows {
            existing_set.insert(row?);
        }
    }
    let existing_ids = unique_ids
        .into_iter()
        .filter(|id| existing_set.contains(*id))
        .collect::<Vec<_>>();
    if existing_ids.is_empty() {
        tx.commit()?;
        return Ok(HashMap::new());
    }

    for batch in existing_ids.chunks(IN_BATCH_SIZE) {
        let placeholders = numbered_placeholders(2, batch.len());
        let sql = format!(
            "UPDATE memories
             SET access_count = access_count + 1, last_access = ?1
             WHERE id IN ({placeholders})"
        );
        let mut values = Vec::with_capacity(batch.len() + 1);
        values.push(Value::Text(now.clone()));
        values.extend(batch.iter().map(|id| Value::Text((*id).to_string())));
        tx.execute(&sql, params_from_iter(values.iter()))?;
    }

    for batch in existing_ids.chunks(IN_BATCH_SIZE / 3) {
        let sql = format!(
            "INSERT INTO access_history (memory_id, accessed_at, query_hash) VALUES {}",
            values_clause(1, batch.len(), 3)
        );
        let mut values = Vec::with_capacity(batch.len() * 3);
        for id in batch {
            values.push(Value::Text((*id).to_string()));
            values.push(Value::Text(now.clone()));
            values.push(Value::Text(query_hash.clone()));
        }
        tx.execute(&sql, params_from_iter(values.iter()))?;
    }

    let fts_set = fts_hits.iter().map(String::as_str).collect::<HashSet<_>>();
    let recall_ids = existing_ids
        .iter()
        .copied()
        .filter(|id| fts_set.contains(*id))
        .collect::<Vec<_>>();
    for batch in recall_ids.chunks(IN_BATCH_SIZE) {
        let placeholders = numbered_placeholders(1, batch.len());
        let sql = format!(
            "UPDATE memories SET recall_count = recall_count + 1 WHERE id IN ({placeholders})"
        );
        let values = batch
            .iter()
            .map(|id| Value::Text((*id).to_string()))
            .collect::<Vec<_>>();
        tx.execute(&sql, params_from_iter(values.iter()))?;
    }

    if !query_hash.is_empty() {
        let mut first_hash_ids = Vec::new();
        for batch in existing_ids.chunks(IN_BATCH_SIZE - 1) {
            let placeholders = numbered_placeholders(1, batch.len());
            let hash_idx = batch.len() + 1;
            let sql = format!(
                "SELECT memory_id FROM access_history
                 WHERE memory_id IN ({placeholders}) AND query_hash = ?{hash_idx}
                 GROUP BY memory_id
                 HAVING COUNT(*) = 1"
            );
            let mut values = batch
                .iter()
                .map(|id| Value::Text((*id).to_string()))
                .collect::<Vec<_>>();
            values.push(Value::Text(query_hash.clone()));
            let mut stmt = tx.prepare(&sql)?;
            let rows = stmt.query_map(params_from_iter(values.iter()), |row| {
                row.get::<_, String>(0)
            })?;
            for row in rows {
                first_hash_ids.push(row?);
            }
        }
        for batch in first_hash_ids.chunks(IN_BATCH_SIZE) {
            let placeholders = numbered_placeholders(1, batch.len());
            let sql = format!(
                "UPDATE memories SET query_diversity = query_diversity + 1 WHERE id IN ({placeholders})"
            );
            let values = batch
                .iter()
                .map(|id| Value::Text(id.clone()))
                .collect::<Vec<_>>();
            tx.execute(&sql, params_from_iter(values.iter()))?;
        }
    }

    for batch in existing_ids.chunks(IN_BATCH_SIZE) {
        let placeholders = numbered_placeholders(1, batch.len());
        let sql = format!(
            "UPDATE memories SET tier = 'consolidated'
             WHERE id IN ({placeholders})
               AND tier = 'raw'
               AND recall_count >= 3
               AND query_diversity >= 3"
        );
        let values = batch
            .iter()
            .map(|id| Value::Text((*id).to_string()))
            .collect::<Vec<_>>();
        tx.execute(&sql, params_from_iter(values.iter()))?;
    }

    let mut updates = HashMap::with_capacity(existing_ids.len());
    for batch in existing_ids.chunks(IN_BATCH_SIZE) {
        let placeholders = numbered_placeholders(1, batch.len());
        let sql = format!(
            "SELECT id, access_count, last_access FROM memories WHERE id IN ({placeholders})"
        );
        let values = batch
            .iter()
            .map(|id| Value::Text((*id).to_string()))
            .collect::<Vec<_>>();
        let mut stmt = tx.prepare(&sql)?;
        let rows = stmt.query_map(params_from_iter(values.iter()), |row| {
            Ok((
                row.get::<_, String>(0)?,
                AccessUpdate {
                    access_count: row.get(1)?,
                    last_access: row.get(2)?,
                },
            ))
        })?;
        for row in rows {
            let (id, update) = row?;
            updates.insert(id, update);
        }
    }

    tx.commit()?;
    Ok(updates)
}

/// Fetch access timestamps for a set of memory IDs (for ACT-R base-level activation).
/// Returns a map from memory_id -> sorted list of seconds-since-epoch (age in seconds).
/// Handles batching internally to stay under SQLite's 999 parameter limit.
pub fn get_access_times(
    conn: &Connection,
    ids: &[String],
) -> Result<HashMap<String, Vec<f64>>, MemoryError> {
    if ids.is_empty() {
        return Ok(HashMap::new());
    }

    let now = Utc::now();
    let mut result: HashMap<String, Vec<f64>> = HashMap::new();

    for batch in ids.chunks(IN_BATCH_SIZE) {
        let placeholders: Vec<String> = batch
            .iter()
            .enumerate()
            .map(|(i, _)| format!("?{}", i + 1))
            .collect();
        let sql = format!(
            "SELECT memory_id, accessed_at FROM access_history WHERE memory_id IN ({}) ORDER BY accessed_at DESC",
            placeholders.join(", ")
        );
        let mut stmt = conn.prepare(&sql)?;
        let params_vec: Vec<&dyn rusqlite::ToSql> =
            batch.iter().map(|s| s as &dyn rusqlite::ToSql).collect();
        let rows = stmt.query_map(params_vec.as_slice(), |row| {
            let mem_id: String = row.get(0)?;
            let at: String = row.get(1)?;
            Ok((mem_id, at))
        })?;

        for row in rows {
            let (mem_id, at_str) = row?;
            if let Ok(dt) = at_str.parse::<DateTime<Utc>>() {
                let age_secs = (now - dt).num_seconds().max(1) as f64;
                result.entry(mem_id).or_default().push(age_secs);
            }
        }
    }

    Ok(result)
}
