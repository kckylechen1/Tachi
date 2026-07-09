use rusqlite::{params, Connection};
use std::collections::HashMap;

use crate::error::MemoryError;

use super::common::now_utc_iso;

// ─── Agent Known State (Context Diffing) ───────────────────────────────────���─

/// Get the known revisions for a set of memory IDs for a given agent.
/// Returns a map of memory_id → revision.
pub fn get_agent_known_revisions(
    conn: &Connection,
    agent_id: &str,
    memory_ids: &[String],
) -> Result<HashMap<String, i64>, MemoryError> {
    if memory_ids.is_empty() {
        return Ok(HashMap::new());
    }

    let placeholders: Vec<String> = (0..memory_ids.len())
        .map(|i| format!("?{}", i + 2))
        .collect();
    let sql = format!(
        "SELECT memory_id, revision FROM agent_known_state WHERE agent_id = ?1 AND memory_id IN ({})",
        placeholders.join(",")
    );

    let mut stmt = conn.prepare(&sql)?;
    let mut param_values: Vec<Box<dyn rusqlite::types::ToSql>> = Vec::new();
    param_values.push(Box::new(agent_id.to_string()));
    for id in memory_ids {
        param_values.push(Box::new(id.clone()));
    }
    let params_refs: Vec<&dyn rusqlite::types::ToSql> =
        param_values.iter().map(|b| b.as_ref()).collect();

    let rows = stmt.query_map(params_refs.as_slice(), |row| {
        Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
    })?;

    let mut map = HashMap::new();
    for row in rows {
        let (id, rev) = row?;
        map.insert(id, rev);
    }
    Ok(map)
}

/// Update the agent's known state for a set of memory entries.
/// Each entry is (memory_id, revision).
pub fn update_agent_known_state(
    conn: &Connection,
    agent_id: &str,
    entries: &[(String, i64)],
) -> Result<(), MemoryError> {
    if entries.is_empty() {
        return Ok(());
    }

    let now = now_utc_iso();
    let tx = conn.unchecked_transaction()?;
    {
        let mut stmt = tx.prepare(
            "INSERT INTO agent_known_state (agent_id, memory_id, revision, synced_at)
             VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT(agent_id, memory_id) DO UPDATE SET
                 revision = excluded.revision,
                 synced_at = excluded.synced_at",
        )?;
        for (memory_id, revision) in entries {
            stmt.execute(params![agent_id, memory_id, revision, &now])?;
        }
    }
    tx.commit()?;
    Ok(())
}
