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
               AND m.id NOT LIKE 'anchor:%'
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
              AND m.id NOT LIKE 'anchor:%'
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

/// Pull a bounded lexical candidate set for exact IDs, path slugs, keywords,
/// and short technical terms that FTS tokenization may miss.
///
/// The SQL predicate finds the symbolic match set, but it MUST NOT apply the
/// candidate cap by recency. A newer partial match is not more relevant than
/// an older row that covers more of the query. Score every matching row first,
/// then keep the best `limit`; timestamp is only the stable tie-breaker.
///
/// The caller provides the final ranker's expansion-aware query separately
/// from the raw match query. Its token coverage is evaluated in SQL, so the
/// database can still enforce the cap without materializing or scoring an
/// unbounded result set in Rust. Replacing this with `ORDER BY timestamp DESC
/// LIMIT` makes an older strong symbolic-only result structurally unable to
/// compete at all (tachi#1144).
pub fn search_symbolic_candidates(
    conn: &Connection,
    query: &str,
    limit: usize,
    include_archived: bool,
    include_superseded: bool,
    path_prefix: Option<&str>,
    as_of: Option<&str>,
) -> Result<Vec<MemoryEntry>, MemoryError> {
    search_symbolic_candidates_with_relevance(
        conn,
        query,
        query,
        limit,
        include_archived,
        include_superseded,
        path_prefix,
        as_of,
    )
}

/// In-crate variant used by hybrid search, where the final ranker's expanded
/// symbolic query must decide pre-cap eligibility. The public helper above
/// preserves its existing raw-query API for other consumers.
#[allow(clippy::too_many_arguments)] // matches the established raw-query helper plus relevance query
pub(crate) fn search_symbolic_candidates_with_relevance(
    conn: &Connection,
    query: &str,
    relevance_query: &str,
    limit: usize,
    include_archived: bool,
    include_superseded: bool,
    path_prefix: Option<&str>,
    as_of: Option<&str>,
) -> Result<Vec<MemoryEntry>, MemoryError> {
    let terms = symbolic_terms(query);
    let relevance_terms = symbolic_terms(relevance_query);

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
           AND (?4 IS NULL OR (COALESCE(NULLIF(valid_from, ''), timestamp) <= ?4 AND (valid_until IS NULL OR valid_until > ?4)))
           AND id NOT LIKE 'anchor:%'"
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
            term_clauses.push(symbolic_term_match_clause(idx));
        }
        sql.push_str(" AND (");
        sql.push_str(&term_clauses.join(" OR "));
        sql.push(')');
    }

    // Rank candidate eligibility by the same expansion-aware term coverage
    // the final symbolic rank uses, while keeping this selection SQL-bounded.
    // The raw `terms` predicate above intentionally stays unchanged: expansion
    // changes relevance among symbolic matches, not what this channel matches.
    let mut relevance_parts = Vec::new();
    for term in &relevance_terms {
        let pattern = format!("%{}%", escape_like_pattern(term));
        params.push(pattern.into());
        let idx = params.len();
        relevance_parts.push(format!(
            "CASE WHEN {} THEN 1 ELSE 0 END",
            symbolic_term_match_clause(idx)
        ));
    }
    let relevance_score = if relevance_parts.is_empty() {
        "0".to_string()
    } else {
        relevance_parts.join(" + ")
    };
    params.push((limit.max(1) as i64).into());
    let limit_idx = params.len();
    // `julianday` compares parsed instants, not the raw TEXT representation:
    // mixed RFC3339 precision/offset rows otherwise mis-order an equal-score
    // tie (tachi#718 CP2, tachi#1144).
    sql.push_str(&format!(
        " ORDER BY ({relevance_score}) DESC, julianday(timestamp) DESC, id ASC LIMIT ?{limit_idx}"
    ));

    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map(rusqlite::params_from_iter(params.iter()), row_to_entry)?;
    let mut out = Vec::new();
    for r in rows {
        out.push(r?);
    }
    Ok(out)
}

fn symbolic_terms(query: &str) -> Vec<String> {
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
    terms
}

fn symbolic_term_match_clause(parameter_index: usize) -> String {
    format!(
        "(id LIKE ?{parameter_index} ESCAPE '\\'
          OR path LIKE ?{parameter_index} ESCAPE '\\'
          OR summary LIKE ?{parameter_index} ESCAPE '\\'
          OR text LIKE ?{parameter_index} ESCAPE '\\'
          OR keywords LIKE ?{parameter_index} ESCAPE '\\'
          OR entities LIKE ?{parameter_index} ESCAPE '\\'
          OR topic LIKE ?{parameter_index} ESCAPE '\\')"
    )
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
