// vault_db.rs — database operations for vault

use super::common::now_utc_iso;
use crate::error::MemoryError;
use crate::vault::{VaultConfig, VaultEntry, VaultKeyRotation};
use rusqlite::{params, Connection};

fn parse_allowed_agents(raw: Option<String>) -> Result<Option<Vec<String>>, MemoryError> {
    Ok(raw
        .map(|value| serde_json::from_str(&value))
        .transpose()
        .map(|value| value.filter(|agents: &Vec<String>| !agents.is_empty()))?)
}

pub fn vault_get_config(conn: &Connection) -> Result<Option<VaultConfig>, MemoryError> {
    let mut stmt = conn.prepare(
        "SELECT salt, verifier, kdf_algorithm, kdf_params, cipher, created_at, updated_at 
         FROM vault_config WHERE id = 1",
    )?;

    let config = stmt.query_row([], |row| {
        Ok(VaultConfig {
            salt: row.get(0)?,
            verifier: row.get(1)?,
            kdf_algorithm: row.get(2)?,
            kdf_params: row.get(3)?,
            cipher: row.get(4)?,
            created_at: row.get(5)?,
            updated_at: row.get(6)?,
        })
    });

    match config {
        Ok(c) => Ok(Some(c)),
        Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
        Err(e) => Err(e.into()),
    }
}

pub fn vault_set_config(conn: &Connection, config: &VaultConfig) -> Result<(), MemoryError> {
    conn.execute(
        "INSERT INTO vault_config (id, salt, verifier, kdf_algorithm, kdf_params, cipher, created_at, updated_at)
         VALUES (1, ?1, ?2, ?3, ?4, ?5, ?6, ?7)
         ON CONFLICT(id) DO UPDATE SET
            salt = excluded.salt,
            verifier = excluded.verifier,
            kdf_algorithm = excluded.kdf_algorithm,
            kdf_params = excluded.kdf_params,
            cipher = excluded.cipher,
            updated_at = excluded.updated_at",
        params![
            config.salt,
            config.verifier,
            config.kdf_algorithm,
            config.kdf_params,
            config.cipher,
            config.created_at,
            config.updated_at,
        ],
    )?;
    Ok(())
}

pub fn vault_upsert_entry(conn: &Connection, entry: &VaultEntry) -> Result<(), MemoryError> {
    let now = now_utc_iso();
    let allowed_agents_json = entry
        .allowed_agents
        .as_ref()
        .filter(|agents| !agents.is_empty())
        .map(serde_json::to_string)
        .transpose()?;
    conn.execute(
        "INSERT INTO vault_entries (name, encrypted_value, nonce, secret_type, description, allowed_agents, created_at, updated_at, accessed_at, access_count)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)
         ON CONFLICT(name) DO UPDATE SET
            encrypted_value = excluded.encrypted_value,
            nonce = excluded.nonce,
            secret_type = excluded.secret_type,
            description = excluded.description,
            allowed_agents = excluded.allowed_agents,
            updated_at = excluded.updated_at",
        params![
            entry.name,
            entry.encrypted_value,
            entry.nonce,
            entry.secret_type,
            entry.description,
            allowed_agents_json,
            if entry.created_at.is_empty() { now.clone() } else { entry.created_at.clone() },
            now,
            entry.accessed_at.clone(),
            entry.access_count,
        ],
    )?;
    Ok(())
}

pub fn vault_get_entry(conn: &Connection, name: &str) -> Result<Option<VaultEntry>, MemoryError> {
    let mut stmt = conn.prepare(
        "SELECT name, encrypted_value, nonce, secret_type, description, allowed_agents, created_at, updated_at, accessed_at, access_count 
         FROM vault_entries WHERE name = ?1"
    )?;

    let entry = stmt.query_row(params![name], |row| {
        Ok(VaultEntry {
            name: row.get(0)?,
            encrypted_value: row.get(1)?,
            nonce: row.get(2)?,
            secret_type: row.get(3)?,
            description: row.get(4)?,
            allowed_agents: parse_allowed_agents(row.get(5)?).map_err(|e| {
                rusqlite::Error::FromSqlConversionFailure(
                    5,
                    rusqlite::types::Type::Text,
                    Box::new(e),
                )
            })?,
            created_at: row.get(6)?,
            updated_at: row.get(7)?,
            accessed_at: row.get(8)?,
            access_count: row.get(9)?,
        })
    });

    match entry {
        Ok(e) => Ok(Some(e)),
        Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
        Err(e) => Err(e.into()),
    }
}

pub fn vault_list_entries(conn: &Connection) -> Result<Vec<VaultEntry>, MemoryError> {
    let mut stmt = conn.prepare(
        "SELECT name, encrypted_value, nonce, secret_type, description, allowed_agents, created_at, updated_at, accessed_at, access_count 
         FROM vault_entries ORDER BY name"
    )?;

    let entries = stmt.query_map([], |row| {
        Ok(VaultEntry {
            name: row.get(0)?,
            encrypted_value: row.get(1)?,
            nonce: row.get(2)?,
            secret_type: row.get(3)?,
            description: row.get(4)?,
            allowed_agents: parse_allowed_agents(row.get(5)?).map_err(|e| {
                rusqlite::Error::FromSqlConversionFailure(
                    5,
                    rusqlite::types::Type::Text,
                    Box::new(e),
                )
            })?,
            created_at: row.get(6)?,
            updated_at: row.get(7)?,
            accessed_at: row.get(8)?,
            access_count: row.get(9)?,
        })
    })?;

    entries.collect::<Result<_, _>>().map_err(|e| e.into())
}

pub fn vault_list_entries_by_type(
    conn: &Connection,
    secret_type: &str,
) -> Result<Vec<VaultEntry>, MemoryError> {
    let mut stmt = conn.prepare(
        "SELECT name, encrypted_value, nonce, secret_type, description, allowed_agents, created_at, updated_at, accessed_at, access_count 
         FROM vault_entries WHERE secret_type = ?1 ORDER BY name"
    )?;

    let entries = stmt.query_map(params![secret_type], |row| {
        Ok(VaultEntry {
            name: row.get(0)?,
            encrypted_value: row.get(1)?,
            nonce: row.get(2)?,
            secret_type: row.get(3)?,
            description: row.get(4)?,
            allowed_agents: parse_allowed_agents(row.get(5)?).map_err(|e| {
                rusqlite::Error::FromSqlConversionFailure(
                    5,
                    rusqlite::types::Type::Text,
                    Box::new(e),
                )
            })?,
            created_at: row.get(6)?,
            updated_at: row.get(7)?,
            accessed_at: row.get(8)?,
            access_count: row.get(9)?,
        })
    })?;

    entries.collect::<Result<_, _>>().map_err(|e| e.into())
}

pub fn vault_delete_entry(conn: &Connection, name: &str) -> Result<bool, MemoryError> {
    let rows = conn.execute("DELETE FROM vault_entries WHERE name = ?1", params![name])?;
    Ok(rows > 0)
}

pub fn vault_touch_entry(conn: &Connection, name: &str) -> Result<i64, MemoryError> {
    let now = now_utc_iso();
    // Use RETURNING so the caller can report the actual post-touch count
    // without a follow-up SELECT (which races concurrent gets and yields a
    // stale "previous + 1" value).
    let mut stmt = conn.prepare(
        "UPDATE vault_entries
            SET accessed_at = ?1, access_count = access_count + 1
          WHERE name = ?2
        RETURNING access_count",
    )?;
    let count: i64 = stmt.query_row(params![now, name], |row| row.get(0))?;
    Ok(count)
}

pub fn vault_insert_audit(
    conn: &Connection,
    timestamp: &str,
    operation: &str,
    secret_name: Option<&str>,
    success: bool,
    detail: Option<&str>,
) -> Result<(), MemoryError> {
    conn.execute(
        "INSERT INTO vault_audit (timestamp, operation, secret_name, success, detail)
         VALUES (?1, ?2, ?3, ?4, ?5)",
        params![timestamp, operation, secret_name, success as i64, detail],
    )?;
    Ok(())
}

pub fn vault_count_entries(conn: &Connection) -> Result<i64, MemoryError> {
    let count: i64 = conn.query_row("SELECT COUNT(*) FROM vault_entries", [], |row| row.get(0))?;
    Ok(count)
}

// Key rotation operations

pub fn vault_get_rotation(
    conn: &Connection,
    prefix: &str,
) -> Result<Option<VaultKeyRotation>, MemoryError> {
    let mut stmt = conn.prepare(
        "SELECT prefix, current_index, total_keys, rotation_strategy, created_at, updated_at 
         FROM vault_key_rotations WHERE prefix = ?1",
    )?;

    let rotation = stmt.query_row(params![prefix], |row| {
        Ok(VaultKeyRotation {
            prefix: row.get(0)?,
            current_index: row.get(1)?,
            total_keys: row.get(2)?,
            rotation_strategy: row.get(3)?,
            created_at: row.get(4)?,
            updated_at: row.get(5)?,
        })
    });

    match rotation {
        Ok(r) => Ok(Some(r)),
        Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
        Err(e) => Err(e.into()),
    }
}

pub fn vault_set_rotation(
    conn: &Connection,
    rotation: &VaultKeyRotation,
) -> Result<(), MemoryError> {
    conn.execute(
        "INSERT INTO vault_key_rotations (prefix, current_index, total_keys, rotation_strategy, created_at, updated_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)
         ON CONFLICT(prefix) DO UPDATE SET
            current_index = excluded.current_index,
            total_keys = excluded.total_keys,
            rotation_strategy = excluded.rotation_strategy,
            updated_at = excluded.updated_at",
        params![
            rotation.prefix,
            rotation.current_index,
            rotation.total_keys,
            rotation.rotation_strategy,
            rotation.created_at,
            rotation.updated_at,
        ],
    )?;
    Ok(())
}

pub fn vault_list_rotations(conn: &Connection) -> Result<Vec<VaultKeyRotation>, MemoryError> {
    let mut stmt = conn.prepare(
        "SELECT prefix, current_index, total_keys, rotation_strategy, created_at, updated_at 
         FROM vault_key_rotations ORDER BY prefix",
    )?;

    let rotations = stmt.query_map([], |row| {
        Ok(VaultKeyRotation {
            prefix: row.get(0)?,
            current_index: row.get(1)?,
            total_keys: row.get(2)?,
            rotation_strategy: row.get(3)?,
            created_at: row.get(4)?,
            updated_at: row.get(5)?,
        })
    })?;

    rotations.collect::<Result<_, _>>().map_err(|e| e.into())
}

pub fn vault_delete_rotation(conn: &Connection, prefix: &str) -> Result<bool, MemoryError> {
    let rows = conn.execute(
        "DELETE FROM vault_key_rotations WHERE prefix = ?1",
        params![prefix],
    )?;
    Ok(rows > 0)
}

pub fn vault_entry_exists(conn: &Connection, name: &str) -> Result<bool, MemoryError> {
    let result = conn.query_row(
        "SELECT 1 FROM vault_entries WHERE name = ?1 LIMIT 1",
        params![name],
        |_row| Ok(true),
    );
    match result {
        Ok(exists) => Ok(exists),
        Err(rusqlite::Error::QueryReturnedNoRows) => Ok(false),
        Err(e) => Err(e.into()),
    }
}
