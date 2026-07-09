use rusqlite::{params, Connection};

use crate::error::MemoryError;
use crate::hub::VirtualCapabilityBinding;

use super::common::now_utc_iso;

pub fn vc_upsert_binding(
    conn: &Connection,
    binding: &VirtualCapabilityBinding,
) -> Result<(), MemoryError> {
    let now = now_utc_iso();
    let metadata_json = serde_json::to_string(&binding.metadata)?;
    conn.execute(
        "INSERT INTO virtual_capability_bindings (
            vc_id, capability_id, priority, version_pin, enabled, metadata, created_at, updated_at
         )
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?7)
         ON CONFLICT(vc_id, capability_id) DO UPDATE SET
            priority = excluded.priority,
            version_pin = excluded.version_pin,
            enabled = excluded.enabled,
            metadata = excluded.metadata,
            updated_at = excluded.updated_at",
        params![
            &binding.vc_id,
            &binding.capability_id,
            binding.priority,
            binding.version_pin.map(i64::from),
            binding.enabled as i32,
            metadata_json,
            &now,
        ],
    )?;
    Ok(())
}

pub fn vc_list_bindings(
    conn: &Connection,
    vc_id: &str,
) -> Result<Vec<VirtualCapabilityBinding>, MemoryError> {
    let mut stmt = conn.prepare(
        "SELECT vc_id, capability_id, priority, version_pin, enabled, metadata, created_at, updated_at
         FROM virtual_capability_bindings
         WHERE vc_id = ?1
         ORDER BY priority ASC, capability_id ASC",
    )?;
    let rows = stmt.query_map(params![vc_id], |row| {
        let metadata_raw: String = row.get(5)?;
        let metadata: serde_json::Value = serde_json::from_str(&metadata_raw).map_err(|e| {
            rusqlite::Error::FromSqlConversionFailure(5, rusqlite::types::Type::Text, Box::new(e))
        })?;
        Ok(VirtualCapabilityBinding {
            vc_id: row.get(0)?,
            capability_id: row.get(1)?,
            priority: row.get(2)?,
            version_pin: row
                .get::<_, Option<i64>>(3)?
                .map(|v| v.try_into().unwrap_or(0)),
            enabled: row.get::<_, i32>(4)? != 0,
            metadata,
            created_at: row.get(6)?,
            updated_at: row.get(7)?,
        })
    })?;

    let mut bindings = Vec::new();
    for row in rows {
        bindings.push(row?);
    }
    Ok(bindings)
}
