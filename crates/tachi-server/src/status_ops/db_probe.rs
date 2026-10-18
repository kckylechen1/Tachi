//! Per-DB health probing for `tachi status`: namespace hygiene, foundry job
//! summaries, and the combined probe_db
//! that opens a manifest DB read-only and assembles its health picture.
//! Extracted from `status_ops::mod` (no behavior change).

use super::{
    latest_failed_job, latest_foundry_job, latest_foundry_job_with_statuses, paths_equal,
    LatestFailedJob, LatestFoundryJob, NamespaceHealth, RelationCount,
};
use crate::foundry_runtime_ops::STUCK_THRESHOLD_SECS;
use crate::manifest::DbRole;
use crate::memory_search_ops::vector_health::{optional_count, vector_health, VectorHealth};
use chrono::{DateTime, Utc};
pub(crate) use memcore::RECALL_CACHE_SQL_WHERE as RECALL_CACHE_WHERE;
use memcore::{job_status_histogram, ContinuityMetrics, JobStatusHistogram, MemoryStore};
use std::path::Path;

type ProbeDbResult = (
    JobStatusHistogram,
    usize,
    VectorHealth,
    NamespaceHealth,
    ContinuityMetrics,
    Option<LatestFoundryJob>,
    Option<LatestFoundryJob>,
    Option<LatestFoundryJob>,
    Option<LatestFailedJob>,
);

pub(crate) fn probe_db(path: &Path, expected_role: Option<&str>) -> Result<ProbeDbResult, String> {
    let path_str = path
        .to_str()
        .ok_or_else(|| format!("non-utf8 path: {}", path.display()))?;
    let store = match expected_role {
        Some(role) => MemoryStore::open_read_only_with_label(path_str, role),
        None => MemoryStore::open_read_only(path_str),
    }
    .map_err(|e| format!("open: {e}"))?;
    let conn = store.connection();
    let hist = job_status_histogram(conn, 30).map_err(|e| format!("histogram: {e}"))?;
    let stuck = count_stuck_in_progress(conn).unwrap_or(0);
    let vector = vector_health(conn).unwrap_or_default();
    let namespace = namespace_health(conn).unwrap_or_default();
    let continuity = store.continuity_metrics(200).unwrap_or_default();
    let latest_active_job =
        latest_foundry_job_with_statuses(conn, &["planned", "queued", "running"]).unwrap_or(None);
    let latest_terminal_job =
        latest_foundry_job_with_statuses(conn, &["completed", "failed", "skipped"]).unwrap_or(None);
    let latest_job = latest_foundry_job(conn).unwrap_or(None);
    let latest_failed_job = latest_failed_job(conn).unwrap_or(None);
    Ok((
        hist,
        stuck,
        vector,
        namespace,
        continuity,
        latest_active_job,
        latest_terminal_job,
        latest_job,
        latest_failed_job,
    ))
}

const WIKI_WHERE: &str = r#"
    path = '/wiki'
    OR path LIKE '/wiki/%'
    OR source = 'wiki'
    OR category = 'wiki'
    OR domain = 'wiki'
    OR COALESCE(json_extract(metadata, '$.wiki'), 0) = 1
"#;

fn table_exists(conn: &rusqlite::Connection, name: &str) -> bool {
    conn.query_row(
        "SELECT 1 FROM sqlite_master WHERE type='table' AND name=?1",
        [name],
        |row| row.get::<_, i64>(0),
    )
    .is_ok()
}

fn count_sql(conn: &rusqlite::Connection, sql: &str) -> Result<usize, rusqlite::Error> {
    conn.query_row(sql, [], |row| row.get::<_, i64>(0).map(|n| n as usize))
}

pub(crate) fn namespace_health(
    conn: &rusqlite::Connection,
) -> Result<NamespaceHealth, rusqlite::Error> {
    let counts = memory_namespace_counts(conn)?;
    let derived_items = if table_exists(conn, "derived_items") {
        count_sql(conn, "SELECT COUNT(*) FROM derived_items")?
    } else {
        0
    };
    let graph_edges = if table_exists(conn, "memory_edges") {
        count_sql(conn, "SELECT COUNT(*) FROM memory_edges")?
    } else {
        0
    };
    let graph_orphan_edges = if graph_edges > 0 {
        count_sql(
            conn,
            "SELECT COUNT(*)
             FROM memory_edges e
             LEFT JOIN memories s ON s.id = e.source_id
             LEFT JOIN memories t ON t.id = e.target_id
             WHERE s.id IS NULL OR t.id IS NULL",
        )?
    } else {
        0
    };
    let graph_relation_types = if graph_edges > 0 {
        relation_type_counts(conn)?
    } else {
        Vec::new()
    };

    Ok(NamespaceHealth {
        recall_cache_rows: counts.recall_cache_rows,
        wiki_rows: counts.wiki_rows,
        wiki_non_source_rows: counts.wiki_non_source_rows,
        wiki_non_category_rows: counts.wiki_non_category_rows,
        kanban_rows: counts.kanban_rows,
        handoff_rows: counts.handoff_rows,
        eval_rows: counts.eval_rows,
        project_scope_rows: counts.project_scope_rows,
        non_project_scope_rows: counts.non_project_scope_rows,
        derived_items,
        graph_edges,
        graph_orphan_edges,
        graph_relation_types,
    })
}

struct MemoryNamespaceCounts {
    recall_cache_rows: usize,
    wiki_rows: usize,
    wiki_non_source_rows: usize,
    wiki_non_category_rows: usize,
    kanban_rows: usize,
    handoff_rows: usize,
    eval_rows: usize,
    project_scope_rows: usize,
    non_project_scope_rows: usize,
}

fn memory_namespace_counts(
    conn: &rusqlite::Connection,
) -> Result<MemoryNamespaceCounts, rusqlite::Error> {
    // A `/wiki/` path already marks a row as wiki (see is_wiki_entry / wiki
    // recall, which key off the path). A wiki page legitimately carries a
    // content-area domain (e.g. `equity_trading`), so only flag rows that are
    // genuinely untagged: empty domain and a non-wiki source.
    conn.query_row(
        &format!(
            "SELECT
                SUM(CASE WHEN ({RECALL_CACHE_WHERE}) THEN 1 ELSE 0 END),
                SUM(CASE WHEN ({WIKI_WHERE}) THEN 1 ELSE 0 END),
                SUM(CASE WHEN ({WIKI_WHERE}) AND source != 'wiki' AND COALESCE(domain, '') = '' THEN 1 ELSE 0 END),
                SUM(CASE WHEN ({WIKI_WHERE}) AND category != 'wiki' THEN 1 ELSE 0 END),
                SUM(CASE WHEN path = '/kanban' OR path LIKE '/kanban/%' OR source = 'kanban' OR category = 'kanban' THEN 1 ELSE 0 END),
                SUM(CASE WHEN path = '/handoff' OR path LIKE '/handoff/%' OR source = 'handoff' OR category = 'handoff' THEN 1 ELSE 0 END),
                SUM(CASE WHEN path = '/eval' OR path LIKE '/eval/%' OR category = 'eval' THEN 1 ELSE 0 END),
                SUM(CASE WHEN scope = 'project' THEN 1 ELSE 0 END),
                SUM(CASE WHEN scope != 'project' THEN 1 ELSE 0 END)
             FROM memories"
        ),
        [],
        |row| {
            Ok(MemoryNamespaceCounts {
                recall_cache_rows: optional_count(row, 0)?,
                wiki_rows: optional_count(row, 1)?,
                wiki_non_source_rows: optional_count(row, 2)?,
                wiki_non_category_rows: optional_count(row, 3)?,
                kanban_rows: optional_count(row, 4)?,
                handoff_rows: optional_count(row, 5)?,
                eval_rows: optional_count(row, 6)?,
                project_scope_rows: optional_count(row, 7)?,
                non_project_scope_rows: optional_count(row, 8)?,
            })
        },
    )
}

fn relation_type_counts(
    conn: &rusqlite::Connection,
) -> Result<Vec<RelationCount>, rusqlite::Error> {
    let mut stmt = conn.prepare(
        "SELECT COALESCE(NULLIF(relation, ''), 'unknown') AS relation, COUNT(*) AS n
         FROM memory_edges
         GROUP BY relation
         ORDER BY n DESC, relation ASC
         LIMIT 8",
    )?;
    let rows = stmt.query_map([], |row| {
        Ok(RelationCount {
            relation: row.get(0)?,
            count: row.get::<_, i64>(1)? as usize,
        })
    })?;
    let mut out = Vec::new();
    for row in rows {
        out.push(row?);
    }
    Ok(out)
}

fn count_stuck_in_progress(conn: &rusqlite::Connection) -> Result<usize, rusqlite::Error> {
    let cutoff: DateTime<Utc> = Utc::now() - chrono::Duration::seconds(STUCK_THRESHOLD_SECS);
    let cutoff_s = cutoff.to_rfc3339_opts(chrono::SecondsFormat::Millis, true);
    count_stuck_in_progress_before(conn, &cutoff_s)
}

fn count_stuck_in_progress_before(
    conn: &rusqlite::Connection,
    cutoff_s: &str,
) -> Result<usize, rusqlite::Error> {
    conn.query_row(
        "SELECT COUNT(*) FROM foundry_jobs WHERE status = 'running' AND datetime(updated_at) < datetime(?1)",
        rusqlite::params![cutoff_s],
        |row| row.get::<_, i64>(0).map(|n| n as usize),
    )
    .or_else(|e| match e {
        rusqlite::Error::QueryReturnedNoRows => Ok(0),
        other => {
            tracing::warn!("count_stuck_in_progress query failed: {other}");
            Err(other)
        }
    })
}

pub(crate) fn is_orphan_entry_in_home(
    entry: &crate::manifest::DbEntry,
    db_path: &Path,
    global_db_path: &Path,
    project_db_path: Option<&Path>,
    tachi_home: &Path,
) -> bool {
    if paths_equal(db_path, global_db_path) {
        return false;
    }
    if let Some(project) = project_db_path {
        if paths_equal(db_path, project) {
            return false;
        }
    }
    if crate::path_utils::named_project_for_db_path_in_home(db_path, tachi_home).is_some() {
        return false;
    }
    !(entry.allow_write
        && entry.schema_kind == "tachi"
        && matches!(
            entry.role,
            DbRole::Agent | DbRole::Foundry | DbRole::Unknown
        ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stuck_foundry_counter_uses_datetime_for_mixed_updated_cutoff() {
        // tachi#1638: this is the status-probe copy of the `updated_at < ?`
        // boundary. A writer-only migration leaves the raw lexical predicate,
        // where the same-second bare row sorts before the canonical cutoff and
        // is falsely counted as stuck. The datetime() reader wrap is what makes
        // mixed bare/canonical rows safe during the GC coexistence window.
        let conn = rusqlite::Connection::open_in_memory().expect("memory db");
        conn.execute(
            "CREATE TABLE foundry_jobs (
                id TEXT PRIMARY KEY,
                status TEXT NOT NULL,
                updated_at TEXT NOT NULL
            )",
            [],
        )
        .expect("create foundry jobs table");
        conn.execute(
            "INSERT INTO foundry_jobs (id, status, updated_at)
             VALUES
             ('same-second-running', 'running', '2026-07-20T12:00:00.100999+00:00'),
             ('actually-stuck-running', 'running', '2026-07-19T12:00:00.999999+00:00'),
             ('old-completed', 'completed', '2026-07-19T12:00:00.999999+00:00')",
            [],
        )
        .expect("seed foundry jobs");

        assert_eq!(
            count_stuck_in_progress_before(&conn, "2026-07-20T12:00:00.100Z").unwrap(),
            1
        );
    }
}
