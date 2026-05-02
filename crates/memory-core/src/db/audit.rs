use rusqlite::{params, Connection};

use crate::error::MemoryError;

// ─── Audit Log Operations ────────────────────────────────────────────────────

/// Insert an audit log entry for a proxy tool call.
#[allow(clippy::too_many_arguments)]
pub fn audit_log_insert(
    conn: &Connection,
    timestamp: &str,
    server_id: &str,
    tool_name: &str,
    args_hash: &str,
    success: bool,
    duration_ms: u64,
    error_kind: Option<&str>,
) -> Result<(), MemoryError> {
    conn.execute(
        "INSERT INTO audit_log (timestamp, server_id, tool_name, args_hash, success, duration_ms, error_kind, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?1)",
        params![timestamp, server_id, tool_name, args_hash, success as i32, duration_ms as i64, error_kind],
    )?;
    Ok(())
}

/// List recent audit log entries.
pub fn audit_log_list(
    conn: &Connection,
    limit: usize,
    server_filter: Option<&str>,
) -> Result<Vec<serde_json::Value>, MemoryError> {
    let (sql, param_values): (String, Vec<Box<dyn rusqlite::types::ToSql>>) = if let Some(server) =
        server_filter
    {
        (
            "SELECT timestamp, server_id, tool_name, success, duration_ms, error_kind FROM audit_log WHERE server_id = ?1 ORDER BY timestamp DESC LIMIT ?2".to_string(),
            vec![Box::new(server.to_string()), Box::new(limit as i64)],
        )
    } else {
        (
            "SELECT timestamp, server_id, tool_name, success, duration_ms, error_kind FROM audit_log ORDER BY timestamp DESC LIMIT ?1".to_string(),
            vec![Box::new(limit as i64)],
        )
    };

    let mut stmt = conn.prepare(&sql)?;
    let params_refs: Vec<&dyn rusqlite::types::ToSql> =
        param_values.iter().map(|b| b.as_ref()).collect();
    let rows = stmt.query_map(params_refs.as_slice(), |row| {
        Ok(serde_json::json!({
            "timestamp": row.get::<_, String>(0)?,
            "server_id": row.get::<_, String>(1)?,
            "tool_name": row.get::<_, String>(2)?,
            "success": row.get::<_, i32>(3)? != 0,
            "duration_ms": row.get::<_, i64>(4)?,
            "error_kind": row.get::<_, Option<String>>(5)?,
        }))
    })?;

    let mut entries = Vec::new();
    for row in rows {
        entries.push(row?);
    }
    Ok(entries)
}
