//! Vector coverage and dimension health for a memory DB, computed read-only
//! from `memories` / `memories_vec`. Consumed by the recall-quality marker and
//! by `status_ops`.

use memcore::vector_backfill::vector_backfill_eligible_where;
use memcore::VectorBackfillScope;
use rusqlite::OptionalExtension;

pub(crate) const EXPECTED_EMBEDDING_DIM: usize = 1024;

#[derive(Debug, Clone, serde::Serialize)]
pub(crate) struct EnrichmentFailureSummary {
    pub(crate) stage: String,
    pub(crate) last_error: String,
    pub(crate) count: usize,
}

#[derive(Debug, Default)]
pub(crate) struct VectorHealth {
    pub(crate) total: usize,
    pub(crate) with_vec: usize,
    pub(crate) missing: usize,
    pub(crate) orphans: usize,
    pub(crate) coverage: f64,
    pub(crate) dimension: Option<usize>,
    /// None means queue membership was not observed, not that the queue is empty.
    pub(crate) pending_enrichment: Option<usize>,
    pub(crate) enrichment_failed_recent: usize,
    pub(crate) enrichment_failures: Vec<EnrichmentFailureSummary>,
}

pub(crate) fn vector_health(conn: &rusqlite::Connection) -> Result<VectorHealth, rusqlite::Error> {
    let (total, enrichment_failed_recent) = memory_vector_status_counts(conn)?;
    // Absence is an optional vector capability; a query failure is not.
    let has_vector_table = memcore::db::table_exists(conn, "memories_vec")?;
    let with_vec = if has_vector_table {
        memory_vector_count(conn)?
    } else {
        0
    };
    let orphans: usize = if has_vector_table {
        conn.query_row(
            "SELECT COUNT(*)
             FROM memories_vec v
             LEFT JOIN memories m ON m.id = v.id
             WHERE m.id IS NULL",
            [],
            |row| row.get::<_, i64>(0).map(|n| n as usize),
        )?
    } else {
        0
    };
    let enrichment_failures = enrichment_failure_summary(conn)?;
    let missing = total.saturating_sub(with_vec);
    let coverage = if total == 0 {
        1.0
    } else {
        with_vec as f64 / total as f64
    };
    let dimension = if has_vector_table {
        infer_vector_dimension(conn)?
    } else {
        None
    };
    Ok(VectorHealth {
        total,
        with_vec,
        missing,
        orphans,
        coverage,
        dimension,
        // Persisted vector coverage cannot observe the ordinary save channel,
        // active backfill, or provider execution. A durable intent/lease alone
        // is not evidence for a complete queue count either.
        pending_enrichment: None,
        enrichment_failed_recent,
        enrichment_failures,
    })
}

fn memory_vector_status_counts(
    conn: &rusqlite::Connection,
) -> Result<(usize, usize), rusqlite::Error> {
    let where_clause = vector_backfill_eligible_where(VectorBackfillScope::default(), false);
    conn.query_row(
        &format!(
            "SELECT
                COUNT(*),
                SUM(CASE WHEN json_extract(metadata, '$.enrichment.status') = 'failed' THEN 1 ELSE 0 END)
             FROM memories
             WHERE {where_clause}"
        ),
        [],
        |row| Ok((optional_count(row, 0)?, optional_count(row, 1)?)),
    )
}

fn memory_vector_count(conn: &rusqlite::Connection) -> Result<usize, rusqlite::Error> {
    let where_clause = vector_backfill_eligible_where(VectorBackfillScope::default(), true);
    conn.query_row(
        &format!(
            "SELECT COUNT(DISTINCT v.id)
             FROM memories m
             LEFT JOIN memories_vec v ON v.id = m.id
             WHERE {where_clause}"
        ),
        [],
        |row| optional_count(row, 0),
    )
}

fn enrichment_failure_summary(
    conn: &rusqlite::Connection,
) -> Result<Vec<EnrichmentFailureSummary>, rusqlite::Error> {
    let where_clause = vector_backfill_eligible_where(VectorBackfillScope::default(), false);
    let mut stmt = conn.prepare(&format!(
        "SELECT
             COALESCE(json_extract(metadata, '$.enrichment.failed_stage'), 'unknown') AS stage,
             COALESCE(json_extract(metadata, '$.enrichment.last_error'), '') AS last_error,
             COUNT(*) AS n
         FROM memories
         WHERE ({where_clause})
           AND json_extract(metadata, '$.enrichment.status') = 'failed'
         GROUP BY stage, last_error
         ORDER BY n DESC
         LIMIT 5",
    ))?;
    let rows = stmt.query_map([], |row| {
        Ok(EnrichmentFailureSummary {
            stage: row.get(0)?,
            last_error: row.get(1)?,
            count: row.get::<_, i64>(2)? as usize,
        })
    })?;
    let mut out = Vec::new();
    for row in rows {
        out.push(row?);
    }
    Ok(out)
}

fn infer_vector_dimension(conn: &rusqlite::Connection) -> Result<Option<usize>, rusqlite::Error> {
    let sql: Option<String> = conn
        .query_row(
            "SELECT sql FROM sqlite_master WHERE name = 'memories_vec' LIMIT 1",
            [],
            |row| row.get(0),
        )
        .optional()?;
    let Some(sql) = sql else {
        return Ok(None);
    };
    let sql_lower = sql.to_ascii_lowercase();
    if let Some(idx) = sql_lower.find("float[") {
        let rest = &sql_lower[idx + "float[".len()..];
        if let Some(end) = rest.find(']') {
            return Ok(rest[..end].parse::<usize>().ok());
        }
    }
    Ok(None)
}

pub(crate) fn optional_count(
    row: &rusqlite::Row<'_>,
    idx: usize,
) -> Result<usize, rusqlite::Error> {
    row.get::<_, Option<i64>>(idx)
        .map(|n| n.unwrap_or(0).max(0) as usize)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn infer_vector_dimension_handles_uppercase_float_decl() {
        let conn = rusqlite::Connection::open_in_memory().expect("memory db");
        conn.execute(
            "CREATE TABLE memories_vec (id TEXT PRIMARY KEY, embedding FLOAT[1024])",
            [],
        )
        .expect("create vector table");

        assert_eq!(infer_vector_dimension(&conn).expect("infer"), Some(1024));
    }
}
