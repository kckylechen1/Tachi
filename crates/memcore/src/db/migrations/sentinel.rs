use rusqlite::{params, Connection};

use crate::error::MemoryError;

use super::{now_utc_iso, MIGRATION_NS};

pub(crate) fn was_run(conn: &Connection, key: &str) -> Result<bool, MemoryError> {
    let count: i64 = conn.query_row(
        "SELECT COUNT(*) FROM hard_state WHERE namespace = ?1 AND key = ?2",
        params![MIGRATION_NS, key],
        |row| row.get(0),
    )?;
    Ok(count > 0)
}

pub(crate) fn mark_run(conn: &Connection, key: &str) -> Result<(), MemoryError> {
    let now = now_utc_iso();
    let value_json = format!("{{\"ran_at\":\"{}\"}}", now);
    conn.execute(
        "INSERT INTO hard_state (namespace, key, value_json, version, created_at, updated_at)
         VALUES (?1, ?2, ?3, 1, ?4, ?4)
         ON CONFLICT(namespace, key) DO UPDATE SET
             value_json = excluded.value_json,
             updated_at = excluded.updated_at,
             version    = hard_state.version + 1",
        params![MIGRATION_NS, key, value_json, now],
    )?;
    Ok(())
}
