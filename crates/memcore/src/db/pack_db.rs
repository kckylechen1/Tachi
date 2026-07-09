// pack_db.rs — Pack registry CRUD operations
//
// All functions take &Connection (or &mut Connection) as first param,
// following the same pattern as hub_db.rs and virtual_capability.rs.

use rusqlite::{params, Connection};

use crate::error::MemoryError;
use crate::pack::{AgentProjection, Pack};

use super::common::now_utc_iso;

// ─── Pack CRUD ──────────────────────────────────────────────────────────────

/// Insert or update a pack in the registry.
pub fn pack_upsert(conn: &Connection, pack: &Pack) -> Result<(), MemoryError> {
    let now = now_utc_iso();
    conn.execute(
        r#"INSERT INTO packs (id, name, source, version, description, skill_count, enabled, local_path, metadata, installed_at, updated_at)
           VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)
           ON CONFLICT(id) DO UPDATE SET
             name = excluded.name,
             source = excluded.source,
             version = excluded.version,
             description = excluded.description,
             skill_count = excluded.skill_count,
             enabled = excluded.enabled,
             local_path = excluded.local_path,
             metadata = excluded.metadata,
             updated_at = excluded.updated_at"#,
        params![
            pack.id,
            pack.name,
            pack.source,
            pack.version,
            pack.description,
            pack.skill_count,
            pack.enabled as i32,
            pack.local_path,
            pack.metadata,
            if pack.installed_at.is_empty() { &now } else { &pack.installed_at },
            now,
        ],
    )?;
    Ok(())
}

/// Get a pack by ID.
pub fn pack_get(conn: &Connection, id: &str) -> Result<Option<Pack>, MemoryError> {
    let mut stmt = conn.prepare(
        r#"SELECT id, name, source, version, description, skill_count, enabled, local_path, metadata, installed_at, updated_at
           FROM packs WHERE id = ?1"#,
    )?;
    let mut rows = stmt.query(params![id])?;
    match rows.next()? {
        Some(row) => Ok(Some(Pack {
            id: row.get(0)?,
            name: row.get(1)?,
            source: row.get(2)?,
            version: row.get(3)?,
            description: row.get(4)?,
            skill_count: row.get(5)?,
            enabled: row.get::<_, i32>(6)? != 0,
            local_path: row.get(7)?,
            metadata: row.get(8)?,
            installed_at: row.get(9)?,
            updated_at: row.get(10)?,
        })),
        None => Ok(None),
    }
}

/// List all packs, optionally filtering by enabled status.
pub fn pack_list(conn: &Connection, enabled_only: bool) -> Result<Vec<Pack>, MemoryError> {
    let sql = if enabled_only {
        "SELECT id, name, source, version, description, skill_count, enabled, local_path, metadata, installed_at, updated_at FROM packs WHERE enabled = 1 ORDER BY name ASC"
    } else {
        "SELECT id, name, source, version, description, skill_count, enabled, local_path, metadata, installed_at, updated_at FROM packs ORDER BY name ASC"
    };
    let mut stmt = conn.prepare(sql)?;
    let rows = stmt.query_map([], |row| {
        Ok(Pack {
            id: row.get(0)?,
            name: row.get(1)?,
            source: row.get(2)?,
            version: row.get(3)?,
            description: row.get(4)?,
            skill_count: row.get(5)?,
            enabled: row.get::<_, i32>(6)? != 0,
            local_path: row.get(7)?,
            metadata: row.get(8)?,
            installed_at: row.get(9)?,
            updated_at: row.get(10)?,
        })
    })?;
    rows.collect::<rusqlite::Result<Vec<Pack>>>()
        .map_err(Into::into)
}

/// Delete a pack by ID. Returns true if found and deleted.
pub fn pack_delete(conn: &Connection, id: &str) -> Result<bool, MemoryError> {
    // Also delete associated projections
    conn.execute(
        "DELETE FROM agent_projections WHERE pack_id = ?1",
        params![id],
    )?;
    let deleted = conn.execute("DELETE FROM packs WHERE id = ?1", params![id])?;
    Ok(deleted > 0)
}

// ─── Agent Projection CRUD ──────────────────────────────────────────────────

/// Upsert an agent projection record.
pub fn projection_upsert(conn: &Connection, proj: &AgentProjection) -> Result<(), MemoryError> {
    let now = now_utc_iso();
    conn.execute(
        r#"INSERT INTO agent_projections (agent, pack_id, enabled, projected_path, skill_count, synced_at)
           VALUES (?1, ?2, ?3, ?4, ?5, ?6)
           ON CONFLICT(agent, pack_id) DO UPDATE SET
             enabled = excluded.enabled,
             projected_path = excluded.projected_path,
             skill_count = excluded.skill_count,
             synced_at = excluded.synced_at"#,
        params![
            proj.agent,
            proj.pack_id,
            proj.enabled as i32,
            proj.projected_path,
            proj.skill_count,
            now,
        ],
    )?;
    Ok(())
}

/// List projections for a specific agent (or all agents if agent is None).
pub fn projection_list(
    conn: &Connection,
    agent: Option<&str>,
    pack_id: Option<&str>,
) -> Result<Vec<AgentProjection>, MemoryError> {
    let mut sql = "SELECT agent, pack_id, enabled, projected_path, skill_count, synced_at FROM agent_projections".to_string();
    let mut where_clauses = Vec::new();

    if agent.is_some() {
        where_clauses.push("agent = ?1");
    }
    if pack_id.is_some() {
        where_clauses.push(if agent.is_some() {
            "pack_id = ?2"
        } else {
            "pack_id = ?1"
        });
    }

    if !where_clauses.is_empty() {
        sql.push_str(" WHERE ");
        sql.push_str(&where_clauses.join(" AND "));
    }

    let order_by = match (agent, pack_id) {
        (Some(_), None) => " ORDER BY pack_id ASC",
        (None, Some(_)) => " ORDER BY agent ASC",
        _ => " ORDER BY agent ASC, pack_id ASC",
    };
    sql.push_str(order_by);

    let mut stmt = conn.prepare(&sql)?;
    let params = rusqlite::params_from_iter(agent.into_iter().chain(pack_id));

    let rows = stmt.query_map(params, |row| {
        Ok(AgentProjection {
            agent: row.get(0)?,
            pack_id: row.get(1)?,
            enabled: row.get::<_, i32>(2)? != 0,
            projected_path: row.get(3)?,
            skill_count: row.get(4)?,
            synced_at: row.get(5)?,
        })
    })?;

    rows.collect::<rusqlite::Result<Vec<AgentProjection>>>()
        .map_err(Into::into)
}
