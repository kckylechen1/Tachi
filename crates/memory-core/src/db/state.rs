use rusqlite::{params, Connection};
use uuid::Uuid;

use crate::error::MemoryError;

use super::common::now_utc_iso;

// ─── Hard State Operations ─────────────────────────────────────────────────────

/// Set a key-value pair in the hard_state table. INSERT OR UPDATE with version bump.
pub fn set_state(
    conn: &Connection,
    namespace: &str,
    key: &str,
    value_json: &str,
) -> Result<u32, MemoryError> {
    let now = now_utc_iso();
    conn.execute(
        "INSERT INTO hard_state (namespace, key, value_json, version, created_at, updated_at)
         VALUES (?1, ?2, ?3, 1, ?4, ?4)
         ON CONFLICT(namespace, key) DO UPDATE SET
           value_json = excluded.value_json,
           version = hard_state.version + 1,
           updated_at = excluded.updated_at",
        params![namespace, key, value_json, &now],
    )?;
    let version: u32 = conn.query_row(
        "SELECT version FROM hard_state WHERE namespace = ?1 AND key = ?2",
        params![namespace, key],
        |row| row.get(0),
    )?;
    Ok(version)
}

/// Get a key-value pair from the hard_state table.
pub fn get_state(
    conn: &Connection,
    namespace: &str,
    key: &str,
) -> Result<Option<(String, u32)>, MemoryError> {
    let mut stmt = conn
        .prepare("SELECT value_json, version FROM hard_state WHERE namespace = ?1 AND key = ?2")?;
    let result = stmt.query_row(params![namespace, key], |row| {
        let val: String = row.get(0)?;
        let ver: u32 = row.get(1)?;
        Ok((val, ver))
    });
    match result {
        Ok(pair) => Ok(Some(pair)),
        Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
        Err(e) => Err(MemoryError::from(e)),
    }
}

/// Save a derived item (causal extraction, distilled rule, etc.)
#[allow(clippy::too_many_arguments)]
pub fn save_derived(
    conn: &Connection,
    text: &str,
    path: &str,
    summary: &str,
    importance: f64,
    source: &str,
    scope: &str,
    metadata: &serde_json::Value,
) -> Result<String, MemoryError> {
    let id = Uuid::new_v4().to_string();
    let now = now_utc_iso();
    let metadata_json = serde_json::to_string(metadata)?;

    conn.execute(
        r#"INSERT INTO derived_items (id, text, path, summary, importance, source, scope, metadata, created_at)
           VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)"#,
        params![id, text, path, summary, importance, source, scope, metadata_json, now],
    )?;

    Ok(id)
}

/// Count derived items by source and path prefix.
pub fn count_derived_by_source(
    conn: &Connection,
    source: &str,
    path_prefix: &str,
) -> Result<u64, MemoryError> {
    let like_pattern = format!("{}%", path_prefix);
    let count: i64 = conn.query_row(
        "SELECT COUNT(*) FROM derived_items WHERE source = ?1 AND path LIKE ?2",
        params![source, like_pattern],
        |row| row.get(0),
    )?;
    Ok(count as u64)
}

/// List derived items by source and path prefix.
pub fn list_derived_by_source(
    conn: &Connection,
    source: &str,
    path_prefix: &str,
    limit: usize,
) -> Result<Vec<serde_json::Value>, MemoryError> {
    let like_pattern = format!("{}%", path_prefix);
    let mut stmt = conn.prepare(
        "SELECT id, text, path, summary, importance, source, scope, metadata, created_at
         FROM derived_items
         WHERE source = ?1 AND path LIKE ?2
         ORDER BY created_at DESC
         LIMIT ?3",
    )?;

    let rows = stmt.query_map(params![source, like_pattern, limit as i64], |row| {
        Ok(serde_json::json!({
            "id": row.get::<_, String>(0)?,
            "text": row.get::<_, String>(1)?,
            "path": row.get::<_, String>(2)?,
            "summary": row.get::<_, String>(3)?,
            "importance": row.get::<_, f64>(4)?,
            "source": row.get::<_, String>(5)?,
            "scope": row.get::<_, String>(6)?,
            "metadata": row.get::<_, String>(7)?,
            "created_at": row.get::<_, String>(8)?,
        }))
    })?;

    let mut out = Vec::new();
    for r in rows {
        out.push(r?);
    }
    Ok(out)
}
