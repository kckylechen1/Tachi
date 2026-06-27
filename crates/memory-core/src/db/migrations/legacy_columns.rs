use rusqlite::{params, Connection};
use serde_json::json;

use crate::error::MemoryError;
use crate::types::apply_location_relocation;

pub(super) fn migrate_v6_fold_persons_into_entities(
    conn: &Connection,
) -> Result<usize, MemoryError> {
    if !table_has_column(conn, "memories", "persons")?
        || !table_has_column(conn, "memories", "entities")?
    {
        return Ok(0);
    }

    let mut stmt = conn.prepare("SELECT id, persons, entities FROM memories")?;
    let rows = stmt.query_map([], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, String>(2)?,
        ))
    })?;
    let mut updates = Vec::new();
    for row in rows {
        let (id, persons_raw, entities_raw) = row?;
        let persons: Vec<String> = serde_json::from_str(&persons_raw).unwrap_or_default();
        if persons.is_empty() {
            continue;
        }
        let mut entities: Vec<String> = serde_json::from_str(&entities_raw).unwrap_or_default();
        crate::types::fold_person_names_into_entities(&mut entities, persons);
        updates.push((id, serde_json::to_string(&entities).unwrap_or_default()));
    }

    if updates.is_empty() {
        return Ok(0);
    }

    conn.execute_batch("BEGIN IMMEDIATE")?;
    let result = (|| -> Result<(), MemoryError> {
        for (id, entities_json) in &updates {
            conn.execute(
                "UPDATE memories SET persons = '[]', entities = ?2 WHERE id = ?1",
                params![id, entities_json],
            )?;
        }
        Ok(())
    })();
    match result {
        Ok(()) => {
            conn.execute_batch("COMMIT")?;
            Ok(updates.len())
        }
        Err(e) => {
            let _ = conn.execute_batch("ROLLBACK");
            Err(e)
        }
    }
}

pub(super) fn migrate_v7_reconcile_legacy_memory_columns(
    conn: &Connection,
) -> Result<usize, MemoryError> {
    let mut actions = 0usize;

    if table_has_column(conn, "memories", "indexed_tags")? {
        conn.execute(
            "UPDATE memories
             SET keywords = indexed_tags
             WHERE (keywords IS NULL OR trim(keywords) IN ('', '[]'))
               AND indexed_tags IS NOT NULL
               AND trim(indexed_tags) NOT IN ('', '[]')",
            [],
        )?;
        actions += 1;
    }

    if table_has_column(conn, "memories", "domain_key")? {
        conn.execute(
            "UPDATE memories
             SET domain = domain_key
             WHERE (domain IS NULL OR trim(COALESCE(domain, '')) = '')
               AND domain_key IS NOT NULL
               AND trim(domain_key) <> ''",
            [],
        )?;
        actions += 1;
    }

    actions += migrate_v6_fold_persons_into_entities(conn)?;

    if !table_has_column(conn, "memories", "location")? {
        conn.execute(
            "ALTER TABLE memories ADD COLUMN location TEXT NOT NULL DEFAULT ''",
            [],
        )?;
        actions += 1;
    }

    if !table_has_column(conn, "memories", "domain")? {
        conn.execute("ALTER TABLE memories ADD COLUMN domain TEXT", [])?;
        actions += 1;
    }

    for column in ["indexed_tags", "domain_key"] {
        if table_has_column(conn, "memories", column)? {
            let column = quote_sql_identifier(column)?;
            conn.execute(&format!("ALTER TABLE memories DROP COLUMN {column}"), [])?;
            actions += 1;
        }
    }

    Ok(actions)
}

pub fn fold_and_drop_legacy_persons_column(conn: &Connection) -> Result<usize, MemoryError> {
    if !table_has_column(conn, "memories", "persons")? {
        return Ok(0);
    }
    let folded = migrate_v6_fold_persons_into_entities(conn)?;
    let column = quote_sql_identifier("persons")?;
    conn.execute(&format!("ALTER TABLE memories DROP COLUMN {column}"), [])?;
    Ok(folded + 1)
}

/// Relocate legacy `location` values, then drop the physical column.
pub fn migrate_v9_relocate_and_drop_location(
    conn: &Connection,
) -> Result<(usize, usize), MemoryError> {
    if !table_has_column(conn, "memories", "location")? {
        return Ok((0, 0));
    }
    let relocated = relocate_location_rows(conn)?;
    conn.execute("ALTER TABLE memories DROP COLUMN location", [])?;
    Ok((relocated, 1))
}

pub(super) fn relocate_location_rows(conn: &Connection) -> Result<usize, MemoryError> {
    const BATCH: usize = 500;
    let mut relocated = 0usize;
    let mut after_id = String::new();
    loop {
        let rows = fetch_location_batch(conn, &after_id, BATCH)?;
        if rows.is_empty() {
            break;
        }
        for (id, path, location, metadata_raw) in &rows {
            let mut metadata: serde_json::Value =
                serde_json::from_str(metadata_raw).unwrap_or_else(|_| json!({}));
            let new_path = apply_location_relocation(path, location, &mut metadata);
            let metadata_str = serde_json::to_string(&metadata).map_err(|e| {
                MemoryError::InvalidArg(format!("serialize metadata for {id}: {e}"))
            })?;
            conn.execute(
                "UPDATE memories SET path = ?1, metadata = ?2, location = '' WHERE id = ?3",
                params![new_path, metadata_str, id],
            )?;
            relocated += 1;
        }
        if let Some(last) = rows.last() {
            after_id = last.0.clone();
        }
    }
    Ok(relocated)
}

fn fetch_location_batch(
    conn: &Connection,
    after_id: &str,
    limit: usize,
) -> Result<Vec<(String, String, String, String)>, MemoryError> {
    let mut stmt = conn.prepare(
        "SELECT id, path, location, metadata FROM memories
         WHERE trim(location) <> '' AND id > ?1
         ORDER BY id
         LIMIT ?2",
    )?;
    let rows = stmt
        .query_map(params![after_id, limit as i64], |row| {
            Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?))
        })?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}

pub(crate) fn table_has_column(
    conn: &Connection,
    table: &str,
    column: &str,
) -> Result<bool, MemoryError> {
    let table = quote_sql_identifier(table)?;
    let sql = format!("SELECT 1 FROM pragma_table_info({table}) WHERE name = ?1 LIMIT 1");
    let exists = conn.query_row(&sql, [column], |_| Ok(())).is_ok();
    Ok(exists)
}

fn quote_sql_identifier(identifier: &str) -> Result<String, MemoryError> {
    if identifier.is_empty()
        || !identifier
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_')
    {
        return Err(MemoryError::InvalidArg(format!(
            "invalid SQL identifier: {identifier}"
        )));
    }
    Ok(format!("\"{identifier}\""))
}

pub(super) fn migrate_v5_drop_hypertachi_legacy_columns(
    conn: &Connection,
) -> Result<usize, MemoryError> {
    let mut dropped = 0usize;
    for column in ["indexed_tags", "domain_key"] {
        if table_has_column(conn, "memories", column)? {
            let column = quote_sql_identifier(column)?;
            conn.execute(&format!("ALTER TABLE memories DROP COLUMN {column}"), [])?;
            dropped += 1;
        }
    }
    Ok(dropped)
}
