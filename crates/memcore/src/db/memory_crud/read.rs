use rusqlite::{params, Connection, OptionalExtension};
use std::collections::HashMap;

use crate::error::MemoryError;
use crate::types::MemoryEntry;

use super::{
    row_to_entry, IN_BATCH_SIZE, MEMORY_EMBEDDING_COLUMN_INDEX, MEMORY_SELECT_COLUMNS,
    MEMORY_SELECT_COLUMNS_QUALIFIED,
};

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
                let blob: Option<Vec<u8>> = row.get(MEMORY_EMBEDDING_COLUMN_INDEX)?;
                if let Some(blob) = blob {
                    if blob.len() % 4 != 0 {
                        return Err(rusqlite::Error::FromSqlConversionFailure(
                            MEMORY_EMBEDDING_COLUMN_INDEX,
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
    let rows = stmt.query_map(params![limit as i64], row_to_entry)?;

    let mut out = Vec::new();
    for r in rows {
        out.push(r?);
    }
    Ok(out)
}

/// Return the id of an active (non-archived) row with the EXACT `path` and
/// `text`, excluding internal REM operation rows, via a direct SQL predicate
/// — no recency-window `LIMIT` to hide behind. #1041 F6: the previous dedup
/// check ran `list_by_path(path, 64,
/// false)` (exact path OR descendant paths, ordered `path ASC, timestamp
/// DESC`, capped at 64 rows) and THEN filtered in memory for an exact
/// path+text match. Once 64+ rows already exist under a path's descendant
/// family, a genuinely duplicate row can sort past the cutoff and never
/// reach the in-memory filter, so dedup silently misses it. Pushing the
/// exact-match predicate into SQL removes the window entirely (correctness
/// win) and is also cheaper (no descendant-path fetch, no full-row hydration
/// for rows the caller was only going to discard).
pub fn find_exact_path_text_id(
    conn: &Connection,
    path: &str,
    text: &str,
) -> Result<Option<String>, MemoryError> {
    conn.query_row(
        "SELECT id FROM memories WHERE path = ?1 AND text = ?2 AND archived = 0 \
         AND id NOT LIKE 'wiki-rem:%' \
         ORDER BY timestamp DESC LIMIT 1",
        params![path, text],
        |row| row.get::<_, String>(0),
    )
    .optional()
    .map_err(MemoryError::from)
}

/// Fetch entries under a path prefix using SQL pushdown instead of full-table scans.
///
/// tachi#1459: this route records nothing. `access_count`, `last_access`,
/// `recall_count` and `query_diversity` observe the search path only — they are
/// earned by `record_access_with_updates` from `hybrid_search`.
/// `gc_tables` can reconcile `query_diversity` from the search-written history,
/// but adds no non-search observation. Every row returned here is therefore
/// read without recording use. That is the existing behaviour and this note
/// does not change it; it is written down because those counters are read
/// downstream as evidence that a memory is unused (archive sweeps, tier
/// promotion, the `overlooked` scoring lever), and callers of this function are
/// exactly the population that evidence cannot see.
pub fn list_by_path(
    conn: &Connection,
    path_prefix: &str,
    limit: usize,
    include_archived: bool,
) -> Result<Vec<MemoryEntry>, MemoryError> {
    let (normalized, like_prefix) = normalize_path_prefix(path_prefix);

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
    let rows = stmt.query_map(params![normalized, like_prefix, limit as i64], row_to_entry)?;
    let mut out = Vec::new();
    for r in rows {
        out.push(r?);
    }
    Ok(out)
}

/// Fetch active, unsuperseded entries under a path prefix.
///
/// This intentionally does not change [`list_by_path`]: audit callers still
/// need the generic archived-only view so they can inspect active rows that
/// have a lifecycle edge. Default Wiki listing uses this narrower route
/// because a row with `superseded_by` is historical, not current truth.
pub fn list_by_path_active_unsuperseded(
    conn: &Connection,
    path_prefix: &str,
    limit: usize,
) -> Result<Vec<MemoryEntry>, MemoryError> {
    let (normalized, like_prefix) = normalize_path_prefix(path_prefix);
    let sql = format!(
        "SELECT {MEMORY_SELECT_COLUMNS}
         FROM memories
         WHERE (path = ?1 OR path LIKE ?2)
           AND archived = 0
           AND superseded_by IS NULL
         ORDER BY path ASC, timestamp DESC
         LIMIT ?3"
    );

    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map(params![normalized, like_prefix, limit as i64], row_to_entry)?;
    let mut out = Vec::new();
    for r in rows {
        out.push(r?);
    }
    Ok(out)
}

/// Fetch the Wiki corpus with its ownership predicate applied before LIMIT.
/// Migration/audit callers may retain active superseded history; archived
/// rows remain excluded.
pub fn list_user_facing_wiki_entries(
    conn: &Connection,
    path_prefix: &str,
    limit: usize,
    include_superseded: bool,
) -> Result<Vec<MemoryEntry>, MemoryError> {
    let (normalized, like_prefix) = normalize_path_prefix(path_prefix);
    let lifecycle = if include_superseded {
        "AND archived = 0"
    } else {
        "AND archived = 0 AND superseded_by IS NULL"
    };
    let wiki_predicate = crate::namespace::USER_FACING_WIKI_SQL_WHERE;
    let sql = format!(
        "SELECT {MEMORY_SELECT_COLUMNS}
         FROM memories
         WHERE (path = ?1 OR path LIKE ?2)
           {lifecycle}
           AND ({wiki_predicate})
         ORDER BY path ASC, timestamp DESC
         LIMIT ?3"
    );
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map(params![normalized, like_prefix, limit as i64], row_to_entry)?;
    let mut out = Vec::new();
    for row in rows {
        out.push(row?);
    }
    Ok(out)
}

/// Fetch entries under a path prefix, newest-first by `timestamp`.
///
/// Unlike [`list_by_path`], which orders `path ASC, timestamp DESC` (so that
/// sibling-path grouping wins over recency and a `LIMIT` can truncate before
/// ever reaching a more-recent entry that happens to sort under a
/// lexicographically later path — e.g. `/agent/checkpoints/2026-07-09` sorts
/// after `/agent/checkpoints/2026-05-30`), this orders purely by recency so
/// the `LIMIT` always keeps the truly newest rows. Callers that want a
/// recency-first view over a path prefix (recent checkpoints, recent kanban
/// entries, etc.) should use this instead of `list_by_path`.
///
/// tachi#1459: like [`list_by_path`], this route records nothing, so reads
/// through here leave the access counters and retained history unchanged.
pub fn list_by_path_recent(
    conn: &Connection,
    path_prefix: &str,
    limit: usize,
    include_archived: bool,
) -> Result<Vec<MemoryEntry>, MemoryError> {
    let (normalized, like_prefix) = normalize_path_prefix(path_prefix);

    let sql = if include_archived {
        format!(
            "SELECT {MEMORY_SELECT_COLUMNS}
         FROM memories
         WHERE path = ?1 OR path LIKE ?2
         ORDER BY timestamp DESC
         LIMIT ?3"
        )
    } else {
        format!(
            "SELECT {MEMORY_SELECT_COLUMNS}
         FROM memories
         WHERE (path = ?1 OR path LIKE ?2) AND archived = 0
         ORDER BY timestamp DESC
         LIMIT ?3"
        )
    };

    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map(params![normalized, like_prefix, limit as i64], row_to_entry)?;
    let mut out = Vec::new();
    for r in rows {
        out.push(r?);
    }
    Ok(out)
}

fn normalize_path_prefix(path_prefix: &str) -> (String, String) {
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
    (normalized, like_prefix)
}

/// One typed ownership predicate for rows ordinary Wiki reads and projection
/// deduplication may expose or retire.
pub fn is_reserved_wiki_internal_path(path: &str) -> bool {
    path == "/wiki/_log"
        || path.starts_with("/wiki/_log/")
        || crate::namespace::path_contains_recall_cache(path)
}

pub fn is_user_facing_wiki_entry(entry: &MemoryEntry) -> bool {
    crate::namespace::is_user_facing_wiki_entry(entry)
}

pub fn list_wiki_duplicate_candidates(
    conn: &Connection,
    path: &str,
    topic: &str,
    parent_path: &str,
    limit: Option<usize>,
) -> Result<Vec<MemoryEntry>, MemoryError> {
    let parent_like = format!("{}/%", parent_path.trim_end_matches('/'));
    let guide_corpus = path == "/guide" || path.starts_with("/guide/");
    let corpus_root = if guide_corpus { "/guide" } else { "/wiki" };
    let corpus_like = format!("{corpus_root}/%");
    let wiki_predicate = crate::namespace::USER_FACING_WIKI_SQL_WHERE;
    let sql = format!(
        "SELECT {MEMORY_SELECT_COLUMNS}
         FROM memories
         WHERE archived = 0
           AND superseded_by IS NULL
           AND id NOT LIKE 'wiki-rem:%'
           AND (path = ?6 OR path LIKE ?7)
           AND ({wiki_predicate})
           AND (
               (?8 = 1
                OR ((?1 = '/wiki/drafts' OR ?1 LIKE '/wiki/drafts/%')
                AND (path = '/wiki/drafts' OR path LIKE '/wiki/drafts/%'))
               OR ((?1 != '/wiki/drafts' AND ?1 NOT LIKE '/wiki/drafts/%')
                   AND path != '/wiki/drafts' AND path NOT LIKE '/wiki/drafts/%'))
           )
           AND (path = ?1 OR (?2 != '' AND topic = ?2) OR path = ?3 OR path LIKE ?4)
         ORDER BY CASE WHEN path = ?1 THEN 0 WHEN topic = ?2 THEN 1 ELSE 2 END,
                  path ASC,
                  timestamp DESC
         LIMIT ?5"
    );
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map(
        params![
            path,
            topic,
            parent_path,
            parent_like,
            limit.map(|value| value as i64).unwrap_or(-1),
            corpus_root,
            corpus_like,
            i64::from(guide_corpus)
        ],
        row_to_entry,
    )?;
    let mut out = Vec::new();
    for row in rows {
        out.push(row?);
    }
    Ok(out)
}

/// Find the canonical active Wiki or Guide row for one exact path.
///
/// Topic similarity is duplicate evidence, not row identity. It is handled by
/// the projection classifier after this exact update target has been chosen.
pub fn find_active_wiki_entry_by_path(
    conn: &Connection,
    path: &str,
) -> Result<Option<MemoryEntry>, MemoryError> {
    let wiki_predicate = crate::namespace::USER_FACING_WIKI_SQL_WHERE;
    let sql = format!(
        r#"SELECT {MEMORY_SELECT_COLUMNS}
           FROM memories
           WHERE archived = 0
             AND superseded_by IS NULL
             AND id NOT LIKE 'wiki-rem:%'
             AND ({wiki_predicate})
             AND path = ?1
           ORDER BY timestamp DESC
           LIMIT 1"#
    );
    let mut stmt = conn.prepare(&sql)?;
    let mut rows = stmt.query_map(params![path], row_to_entry)?;
    match rows.next() {
        Some(row) => Ok(Some(row?)),
        None => Ok(None),
    }
}

/// Find every active predecessor that Wiki ingest treats as the same page:
/// an exact path match, or a Wiki-domain row with the same topic. Exact-path
/// rows sort first so receipt preservation remains deterministic.
pub fn list_active_wiki_ingest_predecessors(
    conn: &Connection,
    path: &str,
    topic: &str,
) -> Result<Vec<MemoryEntry>, MemoryError> {
    let wiki_predicate = crate::namespace::USER_FACING_WIKI_SQL_WHERE;
    let sql = format!(
        r#"SELECT {MEMORY_SELECT_COLUMNS}
           FROM memories
           WHERE archived = 0
             AND superseded_by IS NULL
             AND ({wiki_predicate})
             AND (path = ?1 OR (
                 (path = '/wiki' OR path LIKE '/wiki/%')
                 AND lower(COALESCE(domain, '')) = 'wiki'
                 AND topic = ?2
             ))
           ORDER BY CASE WHEN path = ?1 THEN 0 ELSE 1 END,
                    timestamp DESC,
                    id ASC"#
    );
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map(params![path, topic], row_to_entry)?;
    rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
}
