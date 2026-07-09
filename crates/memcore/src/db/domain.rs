use rusqlite::{params, Connection};

use crate::error::MemoryError;
use crate::types::DomainConfig;

use super::common::now_utc_iso;

// ─── Domain CRUD ─────────────────────────────────────────────────────────────

/// Register or update a domain configuration.
pub fn register_domain(conn: &Connection, domain: &DomainConfig) -> Result<(), MemoryError> {
    let now = now_utc_iso();
    let metadata_json = serde_json::to_string(&domain.metadata)?;
    conn.execute(
        r#"INSERT INTO domains (name, description, gc_threshold_days, default_retention, default_path_prefix, metadata, created_at, updated_at)
           VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
           ON CONFLICT(name) DO UPDATE SET
               description = excluded.description,
               gc_threshold_days = excluded.gc_threshold_days,
               default_retention = excluded.default_retention,
               default_path_prefix = excluded.default_path_prefix,
               metadata = excluded.metadata,
               updated_at = excluded.updated_at"#,
        params![
            domain.name,
            domain.description,
            domain.gc_threshold_days,
            domain.default_retention,
            domain.default_path_prefix,
            metadata_json,
            &now,
            &now,
        ],
    )?;
    Ok(())
}

/// Fetch a domain configuration by name.
pub fn get_domain(conn: &Connection, name: &str) -> Result<Option<DomainConfig>, MemoryError> {
    let mut stmt = conn.prepare(
        "SELECT name, description, gc_threshold_days, default_retention, default_path_prefix, metadata, created_at, updated_at
         FROM domains WHERE name = ?1",
    )?;
    let result = stmt.query_row(params![name], |row| {
        let metadata_str: String = row.get(5)?;
        let metadata: serde_json::Value =
            serde_json::from_str(&metadata_str).unwrap_or(serde_json::json!({}));
        Ok(DomainConfig {
            name: row.get(0)?,
            description: row.get(1)?,
            gc_threshold_days: row.get(2)?,
            default_retention: row.get(3)?,
            default_path_prefix: row.get(4)?,
            metadata,
            created_at: row.get(6)?,
            updated_at: row.get(7)?,
        })
    });

    match result {
        Ok(d) => Ok(Some(d)),
        Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
        Err(e) => Err(MemoryError::from(e)),
    }
}

/// List all registered domain configurations.
pub fn list_domains(conn: &Connection) -> Result<Vec<DomainConfig>, MemoryError> {
    let mut stmt = conn.prepare(
        "SELECT name, description, gc_threshold_days, default_retention, default_path_prefix, metadata, created_at, updated_at
         FROM domains ORDER BY name ASC",
    )?;
    let rows = stmt.query_map([], |row| {
        let metadata_str: String = row.get(5)?;
        let metadata: serde_json::Value =
            serde_json::from_str(&metadata_str).unwrap_or(serde_json::json!({}));
        Ok(DomainConfig {
            name: row.get(0)?,
            description: row.get(1)?,
            gc_threshold_days: row.get(2)?,
            default_retention: row.get(3)?,
            default_path_prefix: row.get(4)?,
            metadata,
            created_at: row.get(6)?,
            updated_at: row.get(7)?,
        })
    })?;

    let mut out = Vec::new();
    for r in rows {
        out.push(r?);
    }
    Ok(out)
}

/// Delete a domain configuration by name. Returns true if deleted.
pub fn delete_domain(conn: &Connection, name: &str) -> Result<bool, MemoryError> {
    conn.execute("DELETE FROM domains WHERE name = ?1", params![name])?;
    Ok(conn.changes() > 0)
}
