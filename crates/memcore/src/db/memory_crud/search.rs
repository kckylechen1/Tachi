use rusqlite::functions::FunctionFlags;
use rusqlite::types::Value;
use rusqlite::{params, Connection};
use std::collections::HashMap;

use crate::error::MemoryError;
use crate::namespace::{surface_sql_splice, Surface};
use crate::types::MemoryEntry;

use super::{
    normalize_utc_iso, row_to_entry, serialize_f32, simple_query_input, MEMORY_SELECT_COLUMNS,
    MEMORY_SELECT_COLUMNS_QUALIFIED,
};

/// sqlite-vec's vec0 virtual table picks its nearest-`k` window FIRST, from
/// `MATCH ?1 AND k = ?3` alone; every `m.*` predicate below (archived,
/// superseded, path, as_of, anchor) is an ordinary post-JOIN filter that
/// only runs after vec0 has already committed to that window. On an
/// archived-heavy table, rows that get filtered out here still consumed a
/// slot in vec0's top-k budget, so the caller can receive far fewer than
/// `top_k` LIVE rows (tachi#1245: measured 54% archived -> effective live
/// yield ~0.46*top_k, worsening monotonically as TTL-archived rows
/// accumulate). One of those post-JOIN predicates (`id NOT LIKE
/// 'anchor:%'`) is UNCONDITIONAL -- it is not gated by any caller flag -- so
/// there is no combination of `include_archived`/`include_superseded`/
/// `path_prefix`/`as_of` that provably makes post-JOIN filtering a no-op.
/// `search_vec` therefore always widens vec0's internal `k` and re-queries
/// whenever the current pass came up short of `top_k`, capped at a bounded
/// number of widen attempts -- see `run_search_vec_query` /
/// `VEC_OVERFETCH_MULTIPLIER` below. When nothing actually gets filtered out
/// (the common case), the first query already returns `top_k` rows and the
/// loop below exits immediately, so this costs exactly one query, same as
/// before tachi#1245.
const VEC_OVERFETCH_MULTIPLIER: usize = 4;
/// sqlite-vec 0.1.9 rejects vec0 KNN queries above this value
/// (`SQLITE_VEC_VEC0_K_MAX` in the pinned dependency). Keep the over-fetch
/// loop inside the dependency's executable domain instead of letting a narrow
/// post-JOIN filter turn an otherwise valid search into a runtime error.
const SQLITE_VEC_KNN_MAX_K: usize = 4096;
/// Caps the widen loop at three retries and the sqlite-vec KNN ceiling: enough
/// headroom to survive the measured 54%-archived corpus (needs ~2x) with
/// margin for corpora that are far more archived-heavy, while keeping a
/// near-fully-archived table from turning every query into an effectively
/// unbounded scan -- it degrades to "as many live rows as vec0 turns up in
/// four bounded passes", not "scan every row looking for a live one".
const VEC_OVERFETCH_MAX_ATTEMPTS: usize = 3;

/// KNN vector search via sqlite-vec.
/// Returns (doc_id -> cosine_distance) for the top `top_k` results.
/// Cosine *distance* is in [0, 2]; we convert to similarity [0, 1]:
///   similarity = 1 - distance/2
#[allow(clippy::too_many_arguments)]
pub fn search_vec(
    conn: &Connection,
    query_vec: &[f32],
    top_k: usize,
    include_archived: bool,
    include_superseded: bool,
    path_prefix: Option<&str>,
    as_of: Option<&str>,
    surface: Option<Surface>,
) -> Result<HashMap<String, f64>, MemoryError> {
    let blob = serialize_f32(query_vec);
    let path_like = path_prefix.map(|prefix| format!("{prefix}%"));
    let as_of_utc = as_of.map(normalize_utc_iso).transpose()?;

    let mut k_fetch = top_k.min(SQLITE_VEC_KNN_MAX_K);
    let mut rows = run_search_vec_query(
        conn,
        &blob,
        k_fetch,
        include_archived,
        include_superseded,
        path_like.as_deref(),
        as_of_utc.as_deref(),
        surface,
    )?;

    // Widen unconditionally whenever the current pass came up short --
    // there is no flag combination that provably rules out post-JOIN
    // filtering (the anchor exclusion is always active), so a fast-path
    // guard keyed on caller flags would be wrong by construction (it must
    // also treat the anchor filter as always-active, which makes it
    // redundant with just trying and checking the actual result length).
    // When nothing was filtered out, `rows.len() >= top_k` already holds
    // after the first query above and this loop is a no-op -- zero extra
    // queries, identical to pre-tachi#1245 cost.
    if top_k > 0 {
        for _ in 0..VEC_OVERFETCH_MAX_ATTEMPTS {
            if rows.len() >= top_k {
                break;
            }
            let widened_k = k_fetch
                .saturating_mul(VEC_OVERFETCH_MULTIPLIER)
                .min(SQLITE_VEC_KNN_MAX_K);
            if widened_k == k_fetch {
                break;
            }
            k_fetch = widened_k;
            rows = run_search_vec_query(
                conn,
                &blob,
                k_fetch,
                include_archived,
                include_superseded,
                path_like.as_deref(),
                as_of_utc.as_deref(),
                surface,
            )?;
        }
    }

    // Budget contract: never return more than the caller asked for. Each
    // widen pass re-queries (rather than appending) so `rows` is always the
    // single, fully distance-ordered result for the current `k_fetch`;
    // truncating here just applies the caller's cap to that ordering.
    rows.truncate(top_k);

    let mut scores = HashMap::new();
    for (id, dist) in rows {
        // sqlite-vec returns L2 / cosine distance depending on vec0 config;
        // treat as cosine distance in [0, 2] -> similarity in [0, 1]
        let sim = (1.0 - dist / 2.0).clamp(0.0, 1.0);
        scores.insert(id, sim);
    }
    Ok(scores)
}

/// Single vec0 KNN query at a given `k`, with the post-JOIN predicates
/// applied in SQL exactly as before tachi#1245 -- only `k` varies across
/// widen attempts. Returns rows ordered ascending by distance (vec0 +
/// `ORDER BY v.distance` guarantee this).
#[allow(clippy::too_many_arguments)]
fn run_search_vec_query(
    conn: &Connection,
    blob: &[u8],
    k: usize,
    include_archived: bool,
    include_superseded: bool,
    path_like: Option<&str>,
    as_of_utc: Option<&str>,
    surface: Option<Surface>,
) -> Result<Vec<(String, f64)>, MemoryError> {
    // `surface` gates an extra `AND (...)` predicate mirroring [`Surface`]'s
    // Rust classifier (`surface_sql_splice`). `None` produces an empty
    // string -- the query text is byte-identical to before this parameter
    // existed, preserving the fused-pool behavior exactly.
    let surface_clause = surface_sql_splice(surface, true);
    let mut stmt = conn.prepare(&format!(
        r#"SELECT v.id, v.distance
           FROM memories_vec v
           JOIN memories m ON m.id = v.id
           WHERE v.embedding MATCH ?1
              AND k = ?3
              AND (?2 = 1 OR m.archived = 0)
               AND (?4 = 1 OR m.superseded_by IS NULL)
               AND (?5 IS NULL OR m.path LIKE ?5)
               AND (?6 IS NULL OR (COALESCE(NULLIF(m.valid_from, ''), m.timestamp) <= ?6 AND (m.valid_until IS NULL OR m.valid_until > ?6)))
               AND m.id NOT LIKE 'anchor:%'{surface_clause}
             ORDER BY v.distance"#,
    ))?;

    let rows = stmt.query_map(
        params![
            blob,
            include_archived as i64,
            k as i64,
            include_superseded as i64,
            path_like,
            as_of_utc
        ],
        |row| {
            let id: String = row.get(0)?;
            let dist: f64 = row.get(1)?;
            Ok((id, dist))
        },
    )?;

    let mut out = Vec::new();
    for r in rows {
        out.push(r?);
    }
    Ok(out)
}

/// Full-text search using the FTS5 virtual table.
/// Returns (doc_id -> normalised BM25 score [0, 1]).
#[allow(clippy::too_many_arguments)]
pub fn search_fts(
    conn: &Connection,
    query: &str,
    limit: usize,
    include_archived: bool,
    include_superseded: bool,
    path_prefix: Option<&str>,
    as_of: Option<&str>,
    surface: Option<Surface>,
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
        surface,
    )
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn search_fts_raw_match(
    conn: &Connection,
    match_query: &str,
    limit: usize,
    include_archived: bool,
    include_superseded: bool,
    path_prefix: Option<&str>,
    as_of: Option<&str>,
    surface: Option<Surface>,
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
        surface,
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
    surface: Option<Surface>,
) -> Result<HashMap<String, f64>, MemoryError> {
    let as_of_utc = as_of.map(normalize_utc_iso).transpose()?;
    let path_like = path_prefix.map(|prefix| format!("{prefix}%"));
    let match_operand = if use_simple_query {
        "simple_query(?1)"
    } else {
        "?1"
    };
    // `surface` gates an extra `AND (...)` predicate mirroring [`Surface`]'s
    // Rust classifier (`surface_sql_splice`). `None` produces an empty
    // string -- the query text is byte-identical to before this parameter
    // existed, preserving the fused-pool behavior exactly.
    let surface_clause = surface_sql_splice(surface, true);
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
              AND m.id NOT LIKE 'anchor:%'{surface_clause}
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
#[allow(clippy::too_many_arguments)]
pub fn search_symbolic_candidates(
    conn: &Connection,
    query: &str,
    limit: usize,
    include_archived: bool,
    include_superseded: bool,
    path_prefix: Option<&str>,
    as_of: Option<&str>,
    surface: Option<Surface>,
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
        surface,
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
    surface: Option<Surface>,
) -> Result<Vec<MemoryEntry>, MemoryError> {
    let terms = symbolic_terms(query);

    if terms.is_empty() && path_prefix.is_none() {
        return Ok(Vec::new());
    }

    register_symbolic_score_function(conn)?;

    let as_of_utc = as_of.map(normalize_utc_iso).transpose()?;
    let path_like = path_prefix.map(|prefix| format!("{prefix}%"));

    // Prefer the trigram index when every term is trigram-eligible (#1331).
    // SQLite's FTS5 trigram tokenizer requires ≥3 Unicode characters per
    // MATCH token; `symbolic_terms` still admits ≥3 UTF-8 *bytes* (so a
    // single CJK character enters the term list). Short-grapheme queries
    // must keep the pre-#1331 LIKE table-scan path or MATCH returns empty
    // while LIKE would have hit. Path-prefix-only queries keep the ordinary
    // `memories` path. Legacy fixtures without the virtual table also fall
    // back to the LIKE scan.
    if !terms.is_empty()
        && memories_symbolic_fts_available(conn)
        && terms_trigram_match_eligible(&terms)
    {
        return search_symbolic_via_trigram(
            conn,
            &terms,
            relevance_query,
            limit,
            include_archived,
            include_superseded,
            path_like.as_deref(),
            as_of_utc.as_deref(),
            surface,
        );
    }

    search_symbolic_via_table_scan(
        conn,
        &terms,
        relevance_query,
        limit,
        include_archived,
        include_superseded,
        path_like.as_deref(),
        as_of_utc.as_deref(),
        surface,
    )
}

fn memories_symbolic_fts_available(conn: &Connection) -> bool {
    conn.query_row(
        "SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = 'memories_symbolic_fts'",
        [],
        |_| Ok(()),
    )
    .is_ok()
}

/// Production SQL body for trigram-accelerated symbolic retrieval (#1331).
///
/// Shared with the receipts harness EXPLAIN assertion so the plan test cannot
/// drift from a handwritten mirror. `{columns}` is substituted with either the
/// full [`MEMORY_SELECT_COLUMNS_QUALIFIED`] list (runtime) or `m.id` (plan).
/// `{surface_clause}` is substituted via [`surface_sql_splice`] -- empty
/// string when `surface` is `None`, so callers that pass `None` (including
/// the receipts EXPLAIN plan test) get the byte-identical pre-surface query
/// shape.
pub const SYMBOLIC_TRIGRAM_SELECT_SQL_TEMPLATE: &str = "SELECT {columns}
         FROM memories_symbolic_fts
         JOIN memories m ON m.id = memories_symbolic_fts.id
         WHERE (?1 = 1 OR m.archived = 0)
           AND (?2 = 1 OR m.superseded_by IS NULL)
           AND (?3 IS NULL OR m.path LIKE ?3)
           AND (?4 IS NULL OR (COALESCE(NULLIF(m.valid_from, ''), m.timestamp) <= ?4 AND (m.valid_until IS NULL OR m.valid_until > ?4)))
           AND m.id NOT LIKE 'anchor:%'
           AND memories_symbolic_fts MATCH ?5{surface_clause}
         ORDER BY tachi_symbolic_score(?6, m.id, m.path, m.topic, m.summary, m.text, m.keywords, m.entities) DESC, julianday(m.timestamp) DESC, m.id ASC
         LIMIT ?7";

/// Build the production trigram SELECT statement for the given column list
/// and surface scope. `surface = None` reproduces the pre-surface query text.
pub fn symbolic_trigram_select_sql(columns: &str, surface: Option<Surface>) -> String {
    SYMBOLIC_TRIGRAM_SELECT_SQL_TEMPLATE
        .replace("{columns}", columns)
        .replace("{surface_clause}", &surface_sql_splice(surface, true))
}

/// Trigram-accelerated symbolic candidate retrieval (#1331).
///
/// Uses FTS5 `MATCH` on `memories_symbolic_fts` (trigram tokenizer). Quoted
/// phrase MATCH is contiguous substring search across the same columns the
/// table-scan path LIKEs — equivalent for terms with ≥3 Unicode characters
/// (gated by [`terms_trigram_match_eligible`]), without `ESCAPE` (which
/// disables the trigram LIKE optimization and falls back to a virtual-table
/// scan). Scoring and the final row payload still read from `memories`,
/// preserving #1154's relevance-first ORDER BY / LIMIT contract.
#[allow(clippy::too_many_arguments)]
fn search_symbolic_via_trigram(
    conn: &Connection,
    terms: &[String],
    relevance_query: &str,
    limit: usize,
    include_archived: bool,
    include_superseded: bool,
    path_like: Option<&str>,
    as_of_utc: Option<&str>,
    surface: Option<Surface>,
) -> Result<Vec<MemoryEntry>, MemoryError> {
    let match_query = symbolic_trigram_match_query(terms);
    let sql = symbolic_trigram_select_sql(MEMORY_SELECT_COLUMNS_QUALIFIED, surface);

    let params: Vec<Value> = vec![
        (include_archived as i64).into(),
        (include_superseded as i64).into(),
        path_like.map(str::to_owned).into(),
        as_of_utc.map(str::to_owned).into(),
        match_query.into(),
        relevance_query.to_owned().into(),
        (limit.max(1) as i64).into(),
    ];

    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map(rusqlite::params_from_iter(params.iter()), row_to_entry)?;
    let mut out = Vec::new();
    for r in rows {
        out.push(r?);
    }
    Ok(out)
}

/// Build an FTS5 trigram MATCH expression that OR's quoted phrases.
/// Quoting keeps `%` / `_` / FTS operators inside the term literal so
/// eligibility matches the table-scan path's escaped LIKE semantics.
fn symbolic_trigram_match_query(terms: &[String]) -> String {
    terms
        .iter()
        .map(|term| format!("\"{}\"", term.replace('"', "\"\"")))
        .collect::<Vec<_>>()
        .join(" OR ")
}

/// Pre-#1331 full-table LIKE scan. Kept as the path-prefix-only path and as
/// a fallback when `memories_symbolic_fts` is absent (legacy test fixtures).
#[allow(clippy::too_many_arguments)]
fn search_symbolic_via_table_scan(
    conn: &Connection,
    terms: &[String],
    relevance_query: &str,
    limit: usize,
    include_archived: bool,
    include_superseded: bool,
    path_like: Option<&str>,
    as_of_utc: Option<&str>,
    surface: Option<Surface>,
) -> Result<Vec<MemoryEntry>, MemoryError> {
    // Unqualified (`qualified = false`): this path SELECTs from bare
    // `memories`, no `m.` join alias.
    let surface_clause = surface_sql_splice(surface, false);
    let mut sql = format!(
        "SELECT {MEMORY_SELECT_COLUMNS} FROM memories
         WHERE (?1 = 1 OR archived = 0)
           AND (?2 = 1 OR superseded_by IS NULL)
           AND (?3 IS NULL OR path LIKE ?3)
           AND (?4 IS NULL OR (COALESCE(NULLIF(valid_from, ''), timestamp) <= ?4 AND (valid_until IS NULL OR valid_until > ?4)))
           AND id NOT LIKE 'anchor:%'{surface_clause}"
    );

    let mut params: Vec<Value> = vec![
        (include_archived as i64).into(),
        (include_superseded as i64).into(),
        path_like.map(str::to_owned).into(),
        as_of_utc.map(str::to_owned).into(),
    ];

    if !terms.is_empty() {
        let mut term_clauses = Vec::new();
        for term in terms {
            let pattern = format!("%{}%", escape_like_pattern(term));
            params.push(pattern.into());
            let idx = params.len();
            term_clauses.push(symbolic_term_match_clause(idx));
        }
        sql.push_str(" AND (");
        sql.push_str(&term_clauses.join(" OR "));
        sql.push(')');
    }

    // The pre-cap score is the final ranker's exact token scorer, not a SQL
    // LIKE-count approximation. The scalar function keeps ordering and LIMIT
    // in SQLite while preventing substring coverage from changing eligibility.
    params.push(relevance_query.to_owned().into());
    let relevance_query_idx = params.len();
    params.push((limit.max(1) as i64).into());
    let limit_idx = params.len();
    // `julianday` compares parsed instants, not the raw TEXT representation:
    // mixed RFC3339 precision/offset rows otherwise mis-order an equal-score
    // tie (tachi#718 CP2, tachi#1144).
    sql.push_str(&format!(
        " ORDER BY tachi_symbolic_score(?{relevance_query_idx}, id, path, topic, summary, text, keywords, entities) DESC, julianday(timestamp) DESC, id ASC LIMIT ?{limit_idx}"
    ));

    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map(rusqlite::params_from_iter(params.iter()), row_to_entry)?;
    let mut out = Vec::new();
    for r in rows {
        out.push(r?);
    }
    Ok(out)
}

fn register_symbolic_score_function(conn: &Connection) -> Result<(), MemoryError> {
    conn.create_scalar_function(
        "tachi_symbolic_score",
        8,
        FunctionFlags::SQLITE_UTF8 | FunctionFlags::SQLITE_DETERMINISTIC,
        |context| {
            let query = context.get::<String>(0)?;
            let id = context.get::<String>(1)?;
            let path = context.get::<String>(2)?;
            let topic = context.get::<String>(3)?;
            let summary = context.get::<String>(4)?;
            let text = context.get::<String>(5)?;
            // `row_to_entry` treats a legacy NULL JSON column as an empty
            // array. Mirror that fallback so candidate selection cannot fail
            // before final ranking sees the same row.
            let keywords = context.get::<Option<String>>(6)?.unwrap_or_default();
            let entities = context.get::<Option<String>>(7)?.unwrap_or_default();
            Ok(crate::scorer::symbolic_score_stored_entry(
                &query,
                &[&id, &path, &topic, &summary, &text],
                &keywords,
                &entities,
            ))
        },
    )?;
    Ok(())
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

/// FTS5 trigram MATCH needs ≥3 Unicode characters per term. Byte-length
/// gating in [`symbolic_terms`] is necessary but not sufficient (one CJK
/// character is 3 UTF-8 bytes / 1 char).
fn terms_trigram_match_eligible(terms: &[String]) -> bool {
    !terms.is_empty() && terms.iter().all(|term| term.chars().count() >= 3)
}

fn symbolic_term_match_clause(parameter_index: usize) -> String {
    symbolic_term_match_clause_on_alias("", parameter_index)
}

fn symbolic_term_match_clause_on_alias(alias: &str, parameter_index: usize) -> String {
    let prefix = if alias.is_empty() {
        String::new()
    } else {
        format!("{alias}.")
    };
    format!(
        "({prefix}id LIKE ?{parameter_index} ESCAPE '\\'
          OR {prefix}path LIKE ?{parameter_index} ESCAPE '\\'
          OR {prefix}summary LIKE ?{parameter_index} ESCAPE '\\'
          OR {prefix}text LIKE ?{parameter_index} ESCAPE '\\'
          OR {prefix}keywords LIKE ?{parameter_index} ESCAPE '\\'
          OR {prefix}entities LIKE ?{parameter_index} ESCAPE '\\'
          OR {prefix}topic LIKE ?{parameter_index} ESCAPE '\\')"
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
