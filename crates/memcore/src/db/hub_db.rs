use rusqlite::{params, Connection};

use crate::error::MemoryError;
use crate::hub::HubCapability;

use super::common::now_utc_iso;

pub fn hub_upsert(conn: &Connection, cap: &HubCapability) -> Result<(), MemoryError> {
    let now = now_utc_iso();
    conn.execute(
        "INSERT INTO hub_capabilities (
            id, type, name, version, description, definition, enabled,
            review_status, health_status, last_error, last_success_at, last_failure_at,
            fail_streak, active_version, exposure_mode,
            uses, successes, failures, avg_rating, last_used,
            created_at, updated_at
         )
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?19, ?20, ?21, ?22)
         ON CONFLICT(id) DO UPDATE SET
           type = excluded.type,
           name = excluded.name,
           version = excluded.version,
           description = excluded.description,
           definition = excluded.definition,
           enabled = excluded.enabled,
           review_status = excluded.review_status,
           health_status = excluded.health_status,
           last_error = excluded.last_error,
           last_success_at = excluded.last_success_at,
           last_failure_at = excluded.last_failure_at,
           fail_streak = excluded.fail_streak,
           active_version = excluded.active_version,
           exposure_mode = excluded.exposure_mode,
           uses = excluded.uses,
           successes = excluded.successes,
           failures = excluded.failures,
           avg_rating = excluded.avg_rating,
           last_used = excluded.last_used,
           updated_at = excluded.updated_at",
        params![
            &cap.id,
            &cap.cap_type,
            &cap.name,
            cap.version,
            &cap.description,
            &cap.definition,
            cap.enabled as i32,
            &cap.review_status,
            &cap.health_status,
            cap.last_error.as_deref(),
            cap.last_success_at.as_deref(),
            cap.last_failure_at.as_deref(),
            cap.fail_streak as i64,
            cap.active_version.as_deref(),
            &cap.exposure_mode,
            cap.uses as i64,
            cap.successes as i64,
            cap.failures as i64,
            cap.avg_rating,
            cap.last_used.as_deref(),
            &now,
            &now,
        ],
    )?;
    Ok(())
}

/// Get a single hub capability by ID.
pub fn hub_get(conn: &Connection, id: &str) -> Result<Option<HubCapability>, MemoryError> {
    let mut stmt = conn.prepare(
        "SELECT id, type, name, version, description, definition, enabled,
                review_status, health_status, last_error, last_success_at, last_failure_at,
                fail_streak, active_version, exposure_mode,
                uses, successes, failures, avg_rating, last_used, created_at, updated_at
         FROM hub_capabilities WHERE id = ?1",
    )?;
    let result = stmt.query_row(params![id], |row| Ok(hub_cap_from_row(row)));
    match result {
        Ok(cap) => Ok(Some(cap)),
        Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
        Err(e) => Err(MemoryError::from(e)),
    }
}

/// List hub capabilities, optionally filtered by type and enabled status.
pub fn hub_list(
    conn: &Connection,
    cap_type: Option<&str>,
    enabled_only: bool,
) -> Result<Vec<HubCapability>, MemoryError> {
    hub_list_limited(conn, cap_type, enabled_only, usize::MAX)
}

/// List hub capabilities with the row ceiling applied by SQLite.
pub fn hub_list_limited(
    conn: &Connection,
    cap_type: Option<&str>,
    enabled_only: bool,
    limit: usize,
) -> Result<Vec<HubCapability>, MemoryError> {
    let mut sql = String::from(
        "SELECT id, type, name, version, description, definition, enabled,
                review_status, health_status, last_error, last_success_at, last_failure_at,
                fail_streak, active_version, exposure_mode,
                uses, successes, failures, avg_rating, last_used, created_at, updated_at
         FROM hub_capabilities WHERE 1=1",
    );
    let mut param_values: Vec<Box<dyn rusqlite::types::ToSql>> = Vec::new();

    if let Some(t) = cap_type {
        sql.push_str(" AND type = ?");
        param_values.push(Box::new(t.to_string()));
    }
    if enabled_only {
        sql.push_str(" AND enabled = 1");
    }
    sql.push_str(" ORDER BY name ASC LIMIT ?");
    param_values.push(Box::new(i64::try_from(limit).unwrap_or(i64::MAX)));

    let mut stmt = conn.prepare(&sql)?;
    let params_refs: Vec<&dyn rusqlite::types::ToSql> =
        param_values.iter().map(|b| b.as_ref()).collect();
    let rows = stmt.query_map(params_refs.as_slice(), |row| Ok(hub_cap_from_row(row)))?;

    let mut caps = Vec::new();
    for row in rows {
        caps.push(row?);
    }
    Ok(caps)
}

pub fn hub_search(
    conn: &Connection,
    query: &str,
    cap_type: Option<&str>,
) -> Result<Vec<HubCapability>, MemoryError> {
    hub_search_limited(conn, query, cap_type, usize::MAX)
}

/// Search hub capabilities with the row ceiling applied by SQLite.
pub fn hub_search_limited(
    conn: &Connection,
    query: &str,
    cap_type: Option<&str>,
    limit: usize,
) -> Result<Vec<HubCapability>, MemoryError> {
    let terms = query
        .split_whitespace()
        .map(|term| term.trim().to_ascii_lowercase())
        .filter(|term| !term.is_empty())
        .collect::<Vec<_>>();
    let mut sql = String::from(
        "SELECT id, type, name, version, description, definition, enabled,
                review_status, health_status, last_error, last_success_at, last_failure_at,
                fail_streak, active_version, exposure_mode,
                uses, successes, failures, avg_rating, last_used, created_at, updated_at
         FROM hub_capabilities
         WHERE ",
    );
    let mut param_values: Vec<Box<dyn rusqlite::types::ToSql>> = Vec::new();

    if terms.is_empty() {
        sql.push_str("1 = 1");
    } else {
        let mut clauses = Vec::new();
        for term in terms {
            // Escape SQL LIKE wildcards so a query containing `%` or `_`
            // (or a literal `\`) matches only itself instead of every row.
            // Pair with `ESCAPE '\'` in the LIKE clause below.
            let escaped = term
                .replace('\\', "\\\\")
                .replace('%', "\\%")
                .replace('_', "\\_");
            clauses.push(
                "(lower(id) LIKE ? ESCAPE '\\' OR lower(name) LIKE ? ESCAPE '\\' OR lower(description) LIKE ? ESCAPE '\\')"
                    .to_string(),
            );
            let pattern = format!("%{}%", escaped);
            param_values.push(Box::new(pattern.clone()));
            param_values.push(Box::new(pattern.clone()));
            param_values.push(Box::new(pattern));
        }
        sql.push_str(&clauses.join(" AND "));
    }

    if let Some(t) = cap_type {
        sql.push_str(" AND type = ?");
        param_values.push(Box::new(t.to_string()));
    }
    sql.push_str(" ORDER BY uses DESC, name ASC LIMIT ?");
    param_values.push(Box::new(i64::try_from(limit).unwrap_or(i64::MAX)));

    let mut stmt = conn.prepare(&sql)?;
    let params_refs: Vec<&dyn rusqlite::types::ToSql> =
        param_values.iter().map(|b| b.as_ref()).collect();
    let rows = stmt.query_map(params_refs.as_slice(), |row| Ok(hub_cap_from_row(row)))?;

    let mut caps = Vec::new();
    for row in rows {
        caps.push(row?);
    }
    Ok(caps)
}

/// Enable or disable a hub capability. Returns true if the row was found.
pub fn hub_set_enabled(conn: &Connection, id: &str, enabled: bool) -> Result<bool, MemoryError> {
    let now = now_utc_iso();
    conn.execute(
        "UPDATE hub_capabilities SET enabled = ?1, updated_at = ?2 WHERE id = ?3",
        params![enabled as i32, &now, id],
    )?;
    Ok(conn.changes() > 0)
}

pub fn hub_set_review(
    conn: &Connection,
    id: &str,
    review_status: &str,
    enabled: Option<bool>,
) -> Result<bool, MemoryError> {
    let now = now_utc_iso();
    match enabled {
        Some(flag) => {
            conn.execute(
                "UPDATE hub_capabilities
                 SET review_status = ?1,
                     enabled = ?2,
                     updated_at = ?3
                 WHERE id = ?4",
                params![review_status, flag as i32, &now, id],
            )?;
        }
        None => {
            conn.execute(
                "UPDATE hub_capabilities
                 SET review_status = ?1,
                     updated_at = ?2
                 WHERE id = ?3",
                params![review_status, &now, id],
            )?;
        }
    }
    Ok(conn.changes() > 0)
}

pub fn hub_set_active_version_route(
    conn: &Connection,
    alias_id: &str,
    active_capability_id: &str,
) -> Result<(), MemoryError> {
    let now = now_utc_iso();
    conn.execute(
        "INSERT INTO hub_version_routes (alias_id, active_capability_id, updated_at)
         VALUES (?1, ?2, ?3)
         ON CONFLICT(alias_id) DO UPDATE SET
             active_capability_id = excluded.active_capability_id,
             updated_at = excluded.updated_at",
        params![alias_id, active_capability_id, &now],
    )?;
    Ok(())
}

pub fn hub_get_active_version_route(
    conn: &Connection,
    alias_id: &str,
) -> Result<Option<String>, MemoryError> {
    let mut stmt = conn.prepare(
        "SELECT active_capability_id
         FROM hub_version_routes
         WHERE alias_id = ?1",
    )?;
    let result = stmt.query_row(params![alias_id], |row| row.get::<_, String>(0));
    match result {
        Ok(target) => Ok(Some(target)),
        Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
        Err(e) => Err(MemoryError::from(e)),
    }
}

pub fn hub_record_call_outcome(
    conn: &Connection,
    id: &str,
    success: bool,
    error_kind: Option<&str>,
    open_threshold: u32,
) -> Result<(), MemoryError> {
    let now = now_utc_iso();
    if success {
        conn.execute(
            "UPDATE hub_capabilities
             SET health_status = 'healthy',
                 last_error = NULL,
                 last_success_at = ?1,
                 fail_streak = 0,
                 updated_at = ?1
             WHERE id = ?2",
            params![&now, id],
        )?;
        return Ok(());
    }

    let current_streak: i64 = conn.query_row(
        "SELECT fail_streak FROM hub_capabilities WHERE id = ?1",
        params![id],
        |row| row.get(0),
    )?;
    let next_streak = current_streak.saturating_add(1);
    let next_health = if next_streak as u32 >= open_threshold.max(1) {
        "open"
    } else {
        "degraded"
    };

    conn.execute(
        "UPDATE hub_capabilities
         SET health_status = ?1,
             last_error = ?2,
             last_failure_at = ?3,
             fail_streak = ?4,
             updated_at = ?3
         WHERE id = ?5",
        params![next_health, error_kind, &now, next_streak, id],
    )?;
    Ok(())
}

/// Record feedback for a hub capability invocation.
pub fn hub_record_feedback(
    conn: &Connection,
    id: &str,
    success: bool,
    rating: Option<f64>,
) -> Result<bool, MemoryError> {
    let now = now_utc_iso();
    conn.execute(
        "UPDATE hub_capabilities SET
           uses = uses + 1,
           successes = successes + ?1,
           failures = failures + ?2,
           last_used = ?3,
           updated_at = ?3
         WHERE id = ?4",
        params![success as i32, (!success) as i32, &now, id,],
    )?;
    let matched = conn.changes() > 0;
    if !matched {
        return Ok(false);
    }

    if let Some(r) = rating {
        let rating = r.clamp(0.0, 5.0);
        conn.execute(
            "UPDATE hub_capabilities SET
               avg_rating = CASE
                 WHEN uses <= 1 THEN ?1
                 ELSE avg_rating + (?1 - avg_rating) / uses
               END
             WHERE id = ?2",
            params![rating, id],
        )?;
    }

    Ok(true)
}

/// Helper: build HubCapability from a row (tolerant of unexpected data).
fn hub_cap_from_row(row: &rusqlite::Row) -> HubCapability {
    HubCapability {
        id: row.get(0).unwrap_or_default(),
        cap_type: row.get(1).unwrap_or_default(),
        name: row.get(2).unwrap_or_default(),
        version: row.get::<_, u32>(3).unwrap_or(1),
        description: row.get(4).unwrap_or_default(),
        definition: row.get(5).unwrap_or_default(),
        enabled: row.get::<_, i32>(6).unwrap_or(1) != 0,
        review_status: row.get(7).unwrap_or_else(|_| "approved".to_string()),
        health_status: row.get(8).unwrap_or_else(|_| "healthy".to_string()),
        last_error: row.get(9).unwrap_or(None),
        last_success_at: row.get(10).unwrap_or(None),
        last_failure_at: row.get(11).unwrap_or(None),
        fail_streak: row.get::<_, i64>(12).unwrap_or(0).max(0) as u32,
        active_version: row.get(13).unwrap_or(None),
        exposure_mode: row.get(14).unwrap_or_else(|_| "direct".to_string()),
        uses: row.get::<_, i64>(15).unwrap_or(0) as u64,
        successes: row.get::<_, i64>(16).unwrap_or(0) as u64,
        failures: row.get::<_, i64>(17).unwrap_or(0) as u64,
        avg_rating: row.get(18).unwrap_or(0.0),
        last_used: row.get(19).unwrap_or(None),
        created_at: row.get(20).unwrap_or_default(),
        updated_at: row.get(21).unwrap_or_default(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn capability(index: usize) -> HubCapability {
        HubCapability {
            id: format!("skill:limited-{index}"),
            cap_type: "skill".to_string(),
            name: format!("Limited capability {index}"),
            version: 1,
            description: "limited capability fixture".to_string(),
            definition: "{}".to_string(),
            enabled: true,
            review_status: "approved".to_string(),
            health_status: "healthy".to_string(),
            last_error: None,
            last_success_at: None,
            last_failure_at: None,
            fail_streak: 0,
            active_version: None,
            exposure_mode: "direct".to_string(),
            uses: index as u64,
            successes: 0,
            failures: 0,
            avg_rating: 0.0,
            last_used: None,
            created_at: String::new(),
            updated_at: String::new(),
        }
    }

    #[test]
    fn hub_list_and_search_apply_limits_inside_sql() {
        crate::db::enable_simple_auto_extension().expect("enable simple tokenizer");
        let conn = Connection::open_in_memory().expect("open hub test database");
        crate::db::init_schema(&conn).expect("initialize hub test schema");
        for index in 0..3 {
            hub_upsert(&conn, &capability(index)).expect("seed hub capability");
        }

        assert_eq!(hub_list(&conn, None, true).unwrap().len(), 3);
        assert_eq!(hub_list_limited(&conn, None, true, 2).unwrap().len(), 2);
        assert!(hub_list_limited(&conn, None, true, 0).unwrap().is_empty());
        assert_eq!(
            hub_search_limited(&conn, "limited capability", None, 1)
                .unwrap()
                .len(),
            1
        );
    }
}
