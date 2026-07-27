//! Read-only candidate lookups for the kanban/handoff GC sweeps.
//!
//! These sweeps need just enough of a `memories` row to decide reapability
//! (id/path/category/timestamp/metadata, plus `archived` for handoff) before
//! deleting via the existing `MemoryStore::delete`. Metadata is returned as
//! the raw JSON text rather than a parsed `serde_json::Value` so the caller
//! keeps deciding — and wording — what a parse failure means (unchanged
//! behavior from when the SQL lived in tachi-server).

use rusqlite::Connection;

use crate::error::MemoryError;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PathPrefixMemoryRow {
    pub id: String,
    pub path: String,
    pub category: String,
    pub timestamp: String,
    pub metadata: String,
}

/// List `memories` rows whose `path` matches a SQL `LIKE` pattern (caller
/// supplies the full pattern, e.g. `"{prefix}%"`).
///
/// tachi#1459: this route records nothing. The access counters observe the
/// search path only — `record_access_with_updates`, from `hybrid_search`, is
/// their sole production observation path. `gc_tables` can reconcile
/// `query_diversity` from search-written history but adds no non-search use, so
/// a sweep over this function neither records an observation nor may treat zero
/// values as evidence that the rows it found are unused.
pub fn list_memories_by_path_prefix(
    conn: &Connection,
    path_like_pattern: &str,
) -> Result<Vec<PathPrefixMemoryRow>, MemoryError> {
    let mut stmt = conn.prepare(
        "SELECT id, path, category, timestamp, metadata
         FROM memories
         WHERE path LIKE ?1",
    )?;
    let rows = stmt.query_map([path_like_pattern], |row| {
        Ok(PathPrefixMemoryRow {
            id: row.get(0)?,
            path: row.get(1)?,
            category: row.get(2)?,
            timestamp: row.get(3)?,
            metadata: row.get(4)?,
        })
    })?;
    let mut out = Vec::new();
    for row in rows {
        out.push(row?);
    }
    Ok(out)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CategoryPathPrefixMemoryRow {
    pub id: String,
    pub timestamp: String,
    pub metadata: String,
    pub archived: bool,
}

/// List `memories` rows matching an exact `category` and a `path` `LIKE`
/// pattern (caller supplies the full pattern, e.g. `"{prefix}%"`).
pub fn list_memories_by_category_and_path_prefix(
    conn: &Connection,
    category: &str,
    path_like_pattern: &str,
) -> Result<Vec<CategoryPathPrefixMemoryRow>, MemoryError> {
    let mut stmt = conn.prepare(
        "SELECT id, timestamp, metadata, archived
         FROM memories
         WHERE category = ?1 AND path LIKE ?2",
    )?;
    let rows = stmt.query_map((category, path_like_pattern), |row| {
        Ok(CategoryPathPrefixMemoryRow {
            id: row.get(0)?,
            timestamp: row.get(1)?,
            metadata: row.get(2)?,
            archived: row.get(3)?,
        })
    })?;
    let mut out = Vec::new();
    for row in rows {
        out.push(row?);
    }
    Ok(out)
}
