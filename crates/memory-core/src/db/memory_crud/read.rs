use rusqlite::{params, Connection};
use std::collections::HashMap;

use crate::error::MemoryError;
use crate::types::MemoryEntry;

use super::{row_to_entry, IN_BATCH_SIZE, MEMORY_SELECT_COLUMNS, MEMORY_SELECT_COLUMNS_QUALIFIED};

/// Fetch multiple entries by their IDs in one query.
/// Also hydrates vectors from memories_vec if available.
/// Handles batching internally to stay under SQLite's 999 parameter limit.
pub fn fetch_by_ids(
    conn: &Connection,
    ids: &[String],
    include_archived: bool,
) -> Result<HashMap<String, MemoryEntry>, MemoryError> {
    if ids.is_empty() {
        return Ok(HashMap::new());
    }

    let mut out = HashMap::new();
    let has_vector_table = conn
        .query_row(
            "SELECT 1 FROM sqlite_master WHERE name = 'memories_vec' LIMIT 1",
            [],
            |_| Ok(true),
        )
        .unwrap_or(false);

    for batch in ids.chunks(IN_BATCH_SIZE) {
        let placeholders = batch
            .iter()
            .enumerate()
            .map(|(i, _)| format!("?{}", i + 1))
            .collect::<Vec<_>>()
            .join(",");
        let mut sql = if has_vector_table {
            format!(
                "SELECT {MEMORY_SELECT_COLUMNS_QUALIFIED}, v.embedding
                 FROM memories m
                 LEFT JOIN memories_vec v ON v.id = m.id
                 WHERE m.id IN ({})",
                placeholders
            )
        } else {
            format!(
                "SELECT {MEMORY_SELECT_COLUMNS} FROM memories WHERE id IN ({})",
                placeholders
            )
        };
        if !include_archived {
            if has_vector_table {
                sql.push_str(" AND m.archived = 0");
            } else {
                sql.push_str(" AND archived = 0");
            }
        }

        let mut stmt = conn.prepare(&sql)?;
        let rows = stmt.query_map(rusqlite::params_from_iter(batch.iter()), |row| {
            let mut entry = row_to_entry(row)?;
            if has_vector_table {
                let blob: Option<Vec<u8>> = row.get(26)?;
                if let Some(blob) = blob {
                    if blob.len() % 4 != 0 {
                        return Err(rusqlite::Error::FromSqlConversionFailure(
                            26,
                            rusqlite::types::Type::Blob,
                            format!(
                                "invalid vector blob length for '{}': {}",
                                entry.id,
                                blob.len()
                            )
                            .into(),
                        ));
                    }
                    entry.vector = Some(
                        blob.chunks_exact(4)
                            .map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]]))
                            .collect(),
                    );
                }
            }
            Ok(entry)
        })?;

        for r in rows {
            let entry = r?;
            out.insert(entry.id.clone(), entry);
        }
    }

    Ok(out)
}

/// Fetch the most recent entries, up to `limit`. Returns sorted dynamically by inserted time.
pub fn get_all(
    conn: &Connection,
    limit: usize,
    include_archived: bool,
) -> Result<Vec<MemoryEntry>, MemoryError> {
    let sql = if include_archived {
        format!("SELECT {MEMORY_SELECT_COLUMNS} FROM memories ORDER BY timestamp DESC LIMIT ?")
    } else {
        format!(
            "SELECT {MEMORY_SELECT_COLUMNS} FROM memories WHERE archived = 0 ORDER BY timestamp DESC LIMIT ?"
        )
    };
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map(params![limit], row_to_entry)?;

    let mut out = Vec::new();
    for r in rows {
        out.push(r?);
    }
    Ok(out)
}

/// Fetch entries under a path prefix using SQL pushdown instead of full-table scans.
pub fn list_by_path(
    conn: &Connection,
    path_prefix: &str,
    limit: usize,
    include_archived: bool,
) -> Result<Vec<MemoryEntry>, MemoryError> {
    let mut normalized = path_prefix.trim().to_string();
    if normalized.is_empty() {
        normalized = "/".to_string();
    }
    if !normalized.starts_with('/') {
        normalized = format!("/{normalized}");
    }
    if normalized.len() > 1 {
        normalized = normalized.trim_end_matches('/').to_string();
    }
    let like_prefix = if normalized == "/" {
        "/%".to_string()
    } else {
        format!("{normalized}/%")
    };

    let sql = if include_archived {
        format!(
            "SELECT {MEMORY_SELECT_COLUMNS}
         FROM memories
         WHERE path = ?1 OR path LIKE ?2
         ORDER BY path ASC, timestamp DESC
         LIMIT ?3"
        )
    } else {
        format!(
            "SELECT {MEMORY_SELECT_COLUMNS}
         FROM memories
         WHERE (path = ?1 OR path LIKE ?2) AND archived = 0
         ORDER BY path ASC, timestamp DESC
         LIMIT ?3"
        )
    };

    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map(params![normalized, like_prefix, limit], row_to_entry)?;
    let mut out = Vec::new();
    for r in rows {
        out.push(r?);
    }
    Ok(out)
}

pub fn list_wiki_duplicate_candidates(
    conn: &Connection,
    path: &str,
    topic: &str,
    parent_path: &str,
    limit: usize,
) -> Result<Vec<MemoryEntry>, MemoryError> {
    let parent_like = format!("{}/%", parent_path.trim_end_matches('/'));
    let sql = format!(
        "SELECT {MEMORY_SELECT_COLUMNS}
         FROM memories
         WHERE archived = 0
           AND superseded_by IS NULL
           AND path LIKE '/wiki/%'
           AND (path = ?1 OR (?2 != '' AND topic = ?2) OR path = ?3 OR path LIKE ?4)
         ORDER BY CASE WHEN path = ?1 THEN 0 WHEN topic = ?2 THEN 1 ELSE 2 END,
                  path ASC,
                  timestamp DESC
         LIMIT ?5"
    );
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map(
        params![path, topic, parent_path, parent_like, limit],
        row_to_entry,
    )?;
    let mut out = Vec::new();
    for row in rows {
        out.push(row?);
    }
    Ok(out)
}

/// Find the canonical active wiki row for a path/topic pair.
pub fn find_active_wiki_entry_by_path_or_topic(
    conn: &Connection,
    path: &str,
    topic: &str,
) -> Result<Option<MemoryEntry>, MemoryError> {
    let sql = format!(
        r#"SELECT {MEMORY_SELECT_COLUMNS}
           FROM memories
           WHERE archived = 0
             AND superseded_by IS NULL
             AND (path = ?1 OR (topic = ?2 AND path LIKE '/wiki/%'))
           ORDER BY CASE WHEN path = ?1 THEN 0 ELSE 1 END, timestamp DESC
           LIMIT 1"#
    );
    let mut stmt = conn.prepare(&sql)?;
    let mut rows = stmt.query_map(params![path, topic], row_to_entry)?;
    match rows.next() {
        Some(row) => Ok(Some(row?)),
        None => Ok(None),
    }
}
