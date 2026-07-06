use rusqlite::types::Value;
use rusqlite::{params, Connection};
use std::collections::HashMap;

use crate::error::MemoryError;
use crate::types::MemoryEntry;

use super::{
    normalize_utc_iso, row_to_entry, serialize_f32, simple_query_input, MEMORY_SELECT_COLUMNS,
};

/// KNN vector search via sqlite-vec.
/// Returns (doc_id -> cosine_distance) for the top `top_k` results.
/// Cosine *distance* is in [0, 2]; we convert to similarity [0, 1]:
///   similarity = 1 - distance/2
pub fn search_vec(
    conn: &Connection,
    query_vec: &[f32],
    top_k: usize,
    include_archived: bool,
    include_superseded: bool,
    path_prefix: Option<&str>,
    as_of: Option<&str>,
) -> Result<HashMap<String, f64>, MemoryError> {
    let blob = serialize_f32(query_vec);
    let path_like = path_prefix.map(|prefix| format!("{prefix}%"));
    let as_of_utc = as_of.map(normalize_utc_iso).transpose()?;
    let mut stmt = conn.prepare(
        r#"SELECT v.id, v.distance
           FROM memories_vec v
           JOIN memories m ON m.id = v.id
           WHERE v.embedding MATCH ?1
              AND k = ?3
              AND (?2 = 1 OR m.archived = 0)
               AND (?4 = 1 OR m.superseded_by IS NULL)
               AND (?5 IS NULL OR m.path LIKE ?5)
               AND (?6 IS NULL OR (COALESCE(NULLIF(m.valid_from, ''), m.timestamp) <= ?6 AND (m.valid_until IS NULL OR m.valid_until > ?6)))
             ORDER BY v.distance"#,
    )?;

    let rows = stmt.query_map(
        params![
            blob,
            include_archived as i64,
            top_k as i64,
            include_superseded as i64,
            path_like,
            as_of_utc.as_deref()
        ],
        |row| {
            let id: String = row.get(0)?;
            let dist: f64 = row.get(1)?;
            Ok((id, dist))
        },
    )?;

    let mut scores = HashMap::new();
    for r in rows {
        let (id, dist) = r?;
        // sqlite-vec returns L2 / cosine distance depending on vec0 config;
        // treat as cosine distance in [0, 2] -> similarity in [0, 1]
        let sim = (1.0 - dist / 2.0).clamp(0.0, 1.0);
        scores.insert(id, sim);
    }
    Ok(scores)
}

/// Full-text search using the FTS5 virtual table.
/// Returns (doc_id -> normalised BM25 score [0, 1]).
pub fn search_fts(
    conn: &Connection,
    query: &str,
    limit: usize,
    include_archived: bool,
    include_superseded: bool,
    path_prefix: Option<&str>,
    as_of: Option<&str>,
) -> Result<HashMap<String, f64>, MemoryError> {
    let safe_query = simple_query_input(query);

    if safe_query.is_empty() {
        return Ok(HashMap::new());
    }

    search_fts_match(
        conn,
        &safe_query,
        true,
        limit,
        include_archived,
        include_superseded,
        path_prefix,
        as_of,
    )
}

pub(crate) fn search_fts_raw_match(
    conn: &Connection,
    match_query: &str,
    limit: usize,
    include_archived: bool,
    include_superseded: bool,
    path_prefix: Option<&str>,
    as_of: Option<&str>,
) -> Result<HashMap<String, f64>, MemoryError> {
    if match_query.trim().is_empty() {
        return Ok(HashMap::new());
    }
    search_fts_match(
        conn,
        match_query,
        false,
        limit,
        include_archived,
        include_superseded,
        path_prefix,
        as_of,
    )
}

#[allow(clippy::too_many_arguments)]
fn search_fts_match(
    conn: &Connection,
    match_query: &str,
    use_simple_query: bool,
    limit: usize,
    include_archived: bool,
    include_superseded: bool,
    path_prefix: Option<&str>,
    as_of: Option<&str>,
) -> Result<HashMap<String, f64>, MemoryError> {
    let as_of_utc = as_of.map(normalize_utc_iso).transpose()?;
    let path_like = path_prefix.map(|prefix| format!("{prefix}%"));
    let match_operand = if use_simple_query {
        "simple_query(?1)"
    } else {
        "?1"
    };
    // The ordinary path uses simple_query() for automatic CJK segmentation.
    // Raw match mode is only for internally constructed, sanitized FTS expressions.
    let mut stmt = conn.prepare(&format!(
        r#"SELECT memories_fts.id, -bm25(memories_fts) AS score
           FROM memories_fts
           JOIN memories m ON m.id = memories_fts.id
           WHERE memories_fts MATCH {match_operand}
              AND (?2 = 1 OR m.archived = 0)
              AND (?4 = 1 OR m.superseded_by IS NULL)
              AND (?5 IS NULL OR m.path LIKE ?5)
              AND (?6 IS NULL OR (COALESCE(NULLIF(m.valid_from, ''), m.timestamp) <= ?6 AND (m.valid_until IS NULL OR m.valid_until > ?6)))
             ORDER BY bm25(memories_fts)
            LIMIT ?3"#,
    ))?;

    let rows = stmt.query_map(
        params![
            match_query,
            include_archived as i64,
            limit as i64,
            include_superseded as i64,
            path_like,
            as_of_utc.as_deref()
        ],
        |row| {
            let id: String = row.get(0)?;
            let score: f64 = row.get(1)?;
            Ok((id, score))
        },
    )?;

    let mut raw: Vec<(String, f64)> = rows.collect::<Result<_, _>>()?;
    if raw.is_empty() {
        return Ok(HashMap::new());
    }

    // Normalise BM25 scores to [0, 1] based on the max in this result set.
    let max_score = raw
        .iter()
        .map(|(_, s)| *s)
        .fold(f64::NEG_INFINITY, f64::max);
    let max_score = if max_score <= 0.0 { 1.0 } else { max_score };

    Ok(raw
        .drain(..)
        .map(|(id, s)| (id, (s / max_score).clamp(0.0, 1.0)))
        .collect())
}

/// Pull a small lexical candidate set for exact IDs, path slugs, keywords, and
/// short technical terms that FTS tokenization may miss.
pub fn search_symbolic_candidates(
    conn: &Connection,
    query: &str,
    limit: usize,
    include_archived: bool,
    include_superseded: bool,
    path_prefix: Option<&str>,
    as_of: Option<&str>,
) -> Result<Vec<MemoryEntry>, MemoryError> {
    let mut terms: Vec<String> = crate::scorer::tokenize(query)
        .into_iter()
        .filter(|term| term.len() >= 3)
        .collect();
    terms.extend(
        query
            .split_whitespace()
            .map(|term| {
                term.trim_matches(|c: char| {
                    !c.is_alphanumeric() && !matches!(c, '-' | '_' | '/' | '.')
                })
                .to_ascii_lowercase()
            })
            .filter(|term| term.len() >= 3),
    );
    terms.sort();
    terms.dedup();
    terms.sort_by_key(|term| std::cmp::Reverse(term.len()));
    terms.truncate(12);

    if terms.is_empty() && path_prefix.is_none() {
        return Ok(Vec::new());
    }

    let as_of_utc = as_of.map(normalize_utc_iso).transpose()?;
    let path_like = path_prefix.map(|prefix| format!("{prefix}%"));
    let mut sql = format!(
        "SELECT {MEMORY_SELECT_COLUMNS} FROM memories
         WHERE (?1 = 1 OR archived = 0)
           AND (?2 = 1 OR superseded_by IS NULL)
           AND (?3 IS NULL OR path LIKE ?3)
           AND (?4 IS NULL OR (COALESCE(NULLIF(valid_from, ''), timestamp) <= ?4 AND (valid_until IS NULL OR valid_until > ?4)))"
    );

    let mut params: Vec<Value> = vec![
        (include_archived as i64).into(),
        (include_superseded as i64).into(),
        path_like.into(),
        as_of_utc.clone().into(),
    ];

    if !terms.is_empty() {
        let mut term_clauses = Vec::new();
        for term in &terms {
            let pattern = format!("%{}%", escape_like_pattern(term));
            params.push(pattern.into());
            let idx = params.len();
            term_clauses.push(format!(
                "(id LIKE ?{idx} ESCAPE '\\'
                  OR path LIKE ?{idx} ESCAPE '\\'
                  OR summary LIKE ?{idx} ESCAPE '\\'
                  OR text LIKE ?{idx} ESCAPE '\\'
                  OR keywords LIKE ?{idx} ESCAPE '\\'
                  OR entities LIKE ?{idx} ESCAPE '\\'
                  OR topic LIKE ?{idx} ESCAPE '\\')"
            ));
        }
        sql.push_str(" AND (");
        sql.push_str(&term_clauses.join(" OR "));
        sql.push(')');
    }

    params.push((limit.max(1) as i64).into());
    let limit_idx = params.len();
    sql.push_str(&format!(" ORDER BY timestamp DESC LIMIT ?{limit_idx}"));

    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map(rusqlite::params_from_iter(params.iter()), row_to_entry)?;
    let mut out = Vec::new();
    for r in rows {
        out.push(r?);
    }
    Ok(out)
}

fn escape_like_pattern(value: &str) -> String {
    let mut escaped = String::with_capacity(value.len());
    for ch in value.chars() {
        if matches!(ch, '\\' | '%' | '_') {
            escaped.push('\\');
        }
        escaped.push(ch);
    }
    escaped
}
