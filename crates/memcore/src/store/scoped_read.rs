//! Host-selected metadata filters for recent reads, applied before SQL LIMIT.
//!
//! This is a read primitive, not an authorization decision. Hosts must derive
//! filters from their admitted agent/session context and retain output admission.

use crate::db::{row_to_entry, MEMORY_SELECT_COLUMNS};
use crate::error::MemoryError;
use crate::{MemoryEntry, MemoryStore};
use rusqlite::{params_from_iter, types::Value};

const MAX_FILTERS: usize = 16;
const MAX_FILTER_VALUES: usize = 256;

/// Exact string membership of a top-level metadata field.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TextMembership {
    AnyOf,
    NoneOf,
}

/// All filters in a request are conjoined. Missing, null, and non-string fields
/// use `missing_value`; without a fallback they satisfy only `NoneOf`.
/// An empty `AnyOf` matches no rows; an empty `NoneOf` excludes no rows.
#[derive(Debug, Clone)]
pub struct MetadataTextFilter {
    pub field: String,
    pub values: Vec<String>,
    pub membership: TextMembership,
    pub missing_value: Option<String>,
}

/// A deterministic, read-only recent view over one literal path subtree.
///
/// Timestamp bounds are inclusive lexical comparisons of the stored timestamp,
/// matching the host recent-list contract. They are not validity/as-of bounds.
/// Like `list_by_path_recent`, this API does not implicitly hide superseded or
/// temporally inactive rows. Hosts retain their lifecycle and admission policy.
#[derive(Debug, Clone)]
pub struct RecentMemoryQuery {
    pub path_prefix: String,
    pub filters: Vec<MetadataTextFilter>,
    pub since: Option<String>,
    pub until: Option<String>,
    pub include_archived: bool,
    pub limit: usize,
}

impl RecentMemoryQuery {
    pub fn new(path_prefix: impl Into<String>, limit: usize) -> Self {
        Self {
            path_prefix: path_prefix.into(),
            filters: Vec::new(),
            since: None,
            until: None,
            include_archived: false,
            limit,
        }
    }
}

impl MemoryStore {
    /// Filter/limit an ID-only SQLite view, then hydrate at most `limit` bodies.
    /// The materialized CTE and hydration share one statement/snapshot. Ordering is
    /// `timestamp DESC, id ASC`; ties do not depend on insertion order.
    /// Uses the admitted handle, preserves the Wiki internal-row gate, and does
    /// not record access, mutate data, reopen a connection, or maintain a cache.
    /// SQL may still scan/sort the candidate metadata: this is not an O(limit)
    /// work or latency guarantee.
    pub fn list_recent_matching(
        &self,
        query: &RecentMemoryQuery,
    ) -> Result<Vec<MemoryEntry>, MemoryError> {
        let (sql, parameters) = recent_sql(query, self.is_wiki_corpus_store())?;
        let mut statement = self.conn.prepare(&sql)?;
        let rows = statement.query_map(params_from_iter(parameters.iter()), row_to_entry)?;
        rows.collect::<rusqlite::Result<Vec<_>>>()
            .map_err(Into::into)
    }
}

fn invalid_query() -> MemoryError {
    // Deliberately omit caller keys, paths and values from errors.
    rusqlite::Error::InvalidParameterName("invalid scoped recent query".into()).into()
}

fn bind(parameters: &mut Vec<Value>, value: Value) -> String {
    parameters.push(value);
    format!("?{}", parameters.len())
}

fn recent_sql(
    query: &RecentMemoryQuery,
    wiki_corpus_store: bool,
) -> Result<(String, Vec<Value>), MemoryError> {
    let limit = i64::try_from(query.limit).map_err(|_| invalid_query())?;
    if query.filters.len() > MAX_FILTERS {
        return Err(invalid_query());
    }
    let mut value_count = 0usize;
    for filter in &query.filters {
        // Bind the JSON key as well as every value. Restrict fields to literal
        // top-level keys; dotted paths and arbitrary SQL are not a query surface.
        if filter.field.is_empty()
            || filter.field.len() > 128
            || !filter
                .field
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
        {
            return Err(invalid_query());
        }
        value_count = value_count
            .checked_add(filter.values.len())
            .filter(|count| *count <= MAX_FILTER_VALUES)
            .ok_or_else(invalid_query)?;
    }

    let mut prefix = query.path_prefix.trim().trim_end_matches('/').to_string();
    if !prefix.starts_with('/') {
        prefix.insert(0, '/');
    }
    let (lower, upper) = if prefix == "/" {
        ("/".to_string(), "0".to_string())
    } else {
        (format!("{prefix}/"), format!("{prefix}0"))
    };
    let mut parameters = Vec::new();
    let exact = bind(&mut parameters, Value::Text(prefix.clone()));
    let lower = bind(&mut parameters, Value::Text(lower));
    let upper = bind(&mut parameters, Value::Text(upper));
    // Binary ranges treat '_' and '%' literally and avoid LIKE's case folding.
    let mut sql = format!(
        "WITH selected_memory_ids AS MATERIALIZED (SELECT id FROM memories \
         WHERE (path COLLATE BINARY = {exact} OR \
         (path COLLATE BINARY >= {lower} AND path COLLATE BINARY < {upper}))"
    );
    if !query.include_archived {
        sql.push_str(" AND archived = 0");
    }
    if wiki_corpus_store {
        let allow_internal = crate::namespace::path_prefix_opts_into_recall_cache(Some(&prefix));
        let predicate = crate::namespace::user_facing_wiki_sql_where(false, allow_internal);
        sql.push_str(&format!(" AND ({predicate})"));
    }
    if let Some(since) = &query.since {
        let value = bind(&mut parameters, Value::Text(since.clone()));
        sql.push_str(&format!(" AND timestamp COLLATE BINARY >= {value}"));
    }
    if let Some(until) = &query.until {
        let value = bind(&mut parameters, Value::Text(until.clone()));
        sql.push_str(&format!(" AND timestamp COLLATE BINARY <= {value}"));
    }
    for filter in &query.filters {
        if filter.values.is_empty() {
            if filter.membership == TextMembership::AnyOf {
                sql.push_str(" AND 0");
            }
            continue;
        }
        let field = bind(&mut parameters, Value::Text(filter.field.clone()));
        // The final duplicate object key wins, matching serde_json::Value. A
        // scalar/array, malformed JSON, or a non-string member has no string.
        // json_each's traversal IDs keep duplicate members in document order.
        let mut expression = format!(
            "SELECT CASE WHEN member.type = 'text' THEN member.value END \
             FROM json_each(CASE WHEN json_valid(metadata) THEN \
             CASE WHEN json_type(metadata) = 'object' THEN metadata ELSE '{{}}' END \
             ELSE '{{}}' END) AS member \
             WHERE member.key COLLATE BINARY = {field} \
             ORDER BY member.id DESC LIMIT 1"
        );
        if let Some(missing) = &filter.missing_value {
            let missing = bind(&mut parameters, Value::Text(missing.clone()));
            expression = format!("COALESCE(({expression}), {missing})");
        }
        let values = filter
            .values
            .iter()
            .map(|value| bind(&mut parameters, Value::Text(value.clone())))
            .collect::<Vec<_>>()
            .join(", ");
        let membership = format!("({expression}) COLLATE BINARY IN ({values})");
        match filter.membership {
            TextMembership::AnyOf => sql.push_str(&format!(" AND COALESCE(({membership}), 0)")),
            TextMembership::NoneOf => {
                sql.push_str(&format!(" AND NOT COALESCE(({membership}), 0)"));
            }
        }
    }
    let limit = bind(&mut parameters, Value::Integer(limit));
    sql.push_str(&format!(
        " ORDER BY timestamp COLLATE BINARY DESC, id COLLATE BINARY ASC LIMIT {limit}) \
         SELECT {MEMORY_SELECT_COLUMNS} FROM memories \
         WHERE id IN (SELECT id FROM selected_memory_ids) \
         ORDER BY timestamp COLLATE BINARY DESC, id COLLATE BINARY ASC"
    ));
    Ok((sql, parameters))
}

#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::{params, Connection};
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    // Deliberately a SQL-predicate fixture, not a counterfeit kernel schema.
    // The portable integration suite separately drives actual MemoryStore
    // open/upsert/row decoding. Raw metadata here exercises legacy JSON forms
    // that serde_json::Value cannot emit through a normal current writer.
    fn connection() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE memories (
                id TEXT PRIMARY KEY, path TEXT, timestamp TEXT,
                metadata TEXT, archived INTEGER, text TEXT
            );",
        )
        .unwrap();
        conn
    }

    fn query_ids(conn: &Connection, query: &RecentMemoryQuery) -> Vec<String> {
        let (sql, values) = recent_sql(query, false).unwrap();
        let sql = sql.replacen(MEMORY_SELECT_COLUMNS, "id", 1);
        let mut stmt = conn.prepare(&sql).unwrap();
        let rows = stmt
            .query_map(params_from_iter(values.iter()), |row| row.get(0))
            .unwrap();
        rows.collect::<rusqlite::Result<Vec<_>>>().unwrap()
    }

    #[test]
    fn legacy_json_matches_last_string_member_and_explicit_missing_fallback() {
        let conn = connection();
        for (id, metadata) in [
            ("last-wanted", r#"{"agent":"foreign","agent":"wanted"}"#),
            ("last-foreign", r#"{"agent":"wanted","agent":"foreign"}"#),
            ("last-number", r#"{"agent":"wanted","agent":42}"#),
            ("malformed", "{bad"),
            ("array", r#"["wanted"]"#),
            ("scalar", r#""wanted""#),
        ] {
            conn.execute(
                "INSERT INTO memories VALUES (?1, '/agents/a', '2026', ?2, 0, '')",
                params![id, metadata],
            )
            .unwrap();
        }
        let mut query = RecentMemoryQuery::new("/agents", 10);
        query.filters.push(MetadataTextFilter {
            field: "agent".into(),
            values: vec!["wanted".into()],
            membership: TextMembership::AnyOf,
            missing_value: None,
        });
        assert_eq!(query_ids(&conn, &query), ["last-wanted"]);
        query.filters[0].missing_value = Some("wanted".into());
        assert_eq!(
            query_ids(&conn, &query),
            ["array", "last-number", "last-wanted", "malformed", "scalar"]
        );
    }

    #[test]
    fn body_projection_runs_only_for_the_limited_eligible_ids() {
        let conn = connection();
        for index in 0..200 {
            conn.execute(
                "INSERT INTO memories VALUES (?1, '/agents/a', ?2, ?3, 0, ?4)",
                params![
                    format!("row-{index:03}"),
                    format!("{index:04}"),
                    if index < 100 {
                        r#"{"agent":"own"}"#
                    } else {
                        r#"{"agent":"foreign"}"#
                    },
                    "x".repeat(2048),
                ],
            )
            .unwrap();
        }
        let hydrated = Arc::new(AtomicUsize::new(0));
        let observed = Arc::clone(&hydrated);
        conn.create_scalar_function(
            "observe_body",
            1,
            rusqlite::functions::FunctionFlags::SQLITE_UTF8,
            move |ctx| {
                observed.fetch_add(1, Ordering::Relaxed);
                ctx.get::<String>(0)
            },
        )
        .unwrap();
        let mut query = RecentMemoryQuery::new("/agents", 3);
        query.filters.push(MetadataTextFilter {
            field: "agent".into(),
            values: vec!["own".into()],
            membership: TextMembership::AnyOf,
            missing_value: None,
        });
        let (sql, values) = recent_sql(&query, false).unwrap();
        let sql = sql.replacen(MEMORY_SELECT_COLUMNS, "id, observe_body(text)", 1);
        let mut stmt = conn.prepare(&sql).unwrap();
        let rows: Vec<(String, String)> = stmt
            .query_map(params_from_iter(values.iter()), |row| {
                Ok((row.get(0)?, row.get(1)?))
            })
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap();
        assert_eq!(
            rows.iter().map(|row| row.0.as_str()).collect::<Vec<_>>(),
            ["row-099", "row-098", "row-097"]
        );
        assert_eq!(hydrated.load(Ordering::Relaxed), 3);
        assert!(rows.iter().all(|row| row.1.len() == 2048));
    }
}
