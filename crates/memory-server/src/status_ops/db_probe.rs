//! Per-DB health probing for `tachi status`: vector coverage/dimension,
//! namespace hygiene, enrichment-failure summaries, and the combined probe_db
//! that opens a manifest DB read-only and assembles its health picture.
//! Extracted from `status_ops::mod` (no behavior change).

// All of `tachi status`'s shared imports (Path, MemoryStore, rusqlite traits,
// json!, the foundry/namespace types) are reachable through the parent module.
use super::*;

#[derive(Debug, Default)]
pub(crate) struct VectorHealth {
    pub(crate) total: usize,
    pub(crate) with_vec: usize,
    pub(crate) missing: usize,
    pub(crate) orphans: usize,
    pub(crate) coverage: f64,
    pub(crate) dimension: Option<usize>,
    pub(crate) pending_enrichment: usize,
    pub(crate) enrichment_failed_recent: usize,
    pub(crate) enrichment_failures: Vec<EnrichmentFailureSummary>,
}

type ProbeDbResult = (
    JobStatusHistogram,
    usize,
    VectorHealth,
    NamespaceHealth,
    Option<LatestFoundryJob>,
    Option<LatestFoundryJob>,
    Option<LatestFoundryJob>,
    Option<LatestFailedJob>,
);

pub(crate) fn probe_db(path: &Path) -> Result<ProbeDbResult, String> {
    let path_str = path
        .to_str()
        .ok_or_else(|| format!("non-utf8 path: {}", path.display()))?;
    let store = MemoryStore::open_read_only(path_str).map_err(|e| format!("open: {e}"))?;
    let conn = store.connection();
    let hist = job_status_histogram(conn, 30).map_err(|e| format!("histogram: {e}"))?;
    let stuck = count_stuck_in_progress(conn).unwrap_or(0);
    let vector = vector_health(conn).unwrap_or_default();
    let namespace = namespace_health(conn).unwrap_or_default();
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
        latest_active_job,
        latest_terminal_job,
        latest_job,
        latest_failed_job,
    ))
}

const RECALL_CACHE_WHERE: &str = r#"
    id = 'foundry_recall_rerank_cache'
    OR id LIKE 'foundry:recall-cache:%'
    OR source = 'foundry_recall_rerank_cache'
    OR topic = 'foundry_recall_rerank_cache'
    OR topic = 'recall_rerank_cache'
    OR path = '/recall-cache'
    OR path LIKE '%/recall-cache'
    OR path LIKE '%/recall-cache/%'
    OR path LIKE '%foundry_recall_rerank_cache%'
    OR COALESCE(json_extract(metadata, '$.recall_rerank_cache'), 0) = 1
    OR COALESCE(json_extract(metadata, '$.cache_key'), '') = 'foundry_recall_rerank_cache'
"#;

const RECALL_CACHE_WHERE_M: &str = r#"
    m.id = 'foundry_recall_rerank_cache'
    OR m.id LIKE 'foundry:recall-cache:%'
    OR m.source = 'foundry_recall_rerank_cache'
    OR m.topic = 'foundry_recall_rerank_cache'
    OR m.topic = 'recall_rerank_cache'
    OR m.path = '/recall-cache'
    OR m.path LIKE '%/recall-cache'
    OR m.path LIKE '%/recall-cache/%'
    OR m.path LIKE '%foundry_recall_rerank_cache%'
    OR COALESCE(json_extract(m.metadata, '$.recall_rerank_cache'), 0) = 1
    OR COALESCE(json_extract(m.metadata, '$.cache_key'), '') = 'foundry_recall_rerank_cache'
"#;

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

fn count_where(conn: &rusqlite::Connection, where_sql: &str) -> Result<usize, rusqlite::Error> {
    count_sql(
        conn,
        &format!("SELECT COUNT(*) FROM memories WHERE {where_sql}"),
    )
}

pub(crate) fn namespace_health(
    conn: &rusqlite::Connection,
) -> Result<NamespaceHealth, rusqlite::Error> {
    let recall_cache_rows = count_where(conn, RECALL_CACHE_WHERE)?;
    let wiki_rows = count_where(conn, WIKI_WHERE)?;
    // A `/wiki/` path already marks a row as wiki (see is_wiki_entry / wiki
    // recall, which key off the path). A wiki page legitimately carries a
    // content-area domain (e.g. `equity_trading`), so only flag rows that are
    // GENUINELY untagged: empty domain AND a non-wiki source. Those are the
    // ones a domain/source backfill would actually help.
    let wiki_non_source_rows = count_where(
        conn,
        &format!("({WIKI_WHERE}) AND source != 'wiki' AND COALESCE(domain, '') = ''"),
    )?;
    let wiki_non_category_rows =
        count_where(conn, &format!("({WIKI_WHERE}) AND category != 'wiki'"))?;
    let kanban_rows = count_where(
        conn,
        "path = '/kanban' OR path LIKE '/kanban/%' OR source = 'kanban' OR category = 'kanban'",
    )?;
    let handoff_rows = count_where(
        conn,
        "path = '/handoff' OR path LIKE '/handoff/%' OR source = 'handoff' OR category = 'handoff'",
    )?;
    let eval_rows = count_where(
        conn,
        "path = '/eval' OR path LIKE '/eval/%' OR category = 'eval'",
    )?;
    let project_scope_rows = count_where(conn, "scope = 'project'")?;
    let non_project_scope_rows = count_where(conn, "scope != 'project'")?;
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
        recall_cache_rows,
        wiki_rows,
        wiki_non_source_rows,
        wiki_non_category_rows,
        kanban_rows,
        handoff_rows,
        eval_rows,
        project_scope_rows,
        non_project_scope_rows,
        derived_items,
        graph_edges,
        graph_orphan_edges,
        graph_relation_types,
    })
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

pub(crate) fn vector_health(conn: &rusqlite::Connection) -> Result<VectorHealth, rusqlite::Error> {
    let total: usize = conn.query_row(
        &format!("SELECT COUNT(*) FROM memories WHERE NOT ({RECALL_CACHE_WHERE})"),
        [],
        |row| row.get::<_, i64>(0).map(|n| n as usize),
    )?;
    let with_vec: usize = conn
        .query_row(
            &format!(
                "SELECT COUNT(DISTINCT v.id)
             FROM memories_vec v
             JOIN memories m ON m.id = v.id
             WHERE NOT ({RECALL_CACHE_WHERE_M})"
            ),
            [],
            |row| row.get::<_, i64>(0).map(|n| n as usize),
        )
        .unwrap_or(0);
    let orphans: usize = conn
        .query_row(
            "SELECT COUNT(*)
             FROM memories_vec v
             LEFT JOIN memories m ON m.id = v.id
             WHERE m.id IS NULL",
            [],
            |row| row.get::<_, i64>(0).map(|n| n as usize),
        )
        .unwrap_or(0);
    let enrichment_failed_recent: usize = conn
        .query_row(
            "SELECT COUNT(*) FROM memories
             WHERE json_extract(metadata, '$.enrichment.status') = 'failed'",
            [],
            |row| row.get::<_, i64>(0).map(|n| n as usize),
        )
        .unwrap_or(0);
    let pending_enrichment: usize = conn
        .query_row(
            &format!(
                "SELECT COUNT(*)
             FROM memories m
             LEFT JOIN memories_vec v ON v.id = m.id
             WHERE NOT ({RECALL_CACHE_WHERE_M})
               AND v.id IS NULL
               AND COALESCE(json_extract(m.metadata, '$.enrichment.status'), '') != 'failed'"
            ),
            [],
            |row| row.get::<_, i64>(0).map(|n| n as usize),
        )
        .unwrap_or(0);
    let enrichment_failures = enrichment_failure_summary(conn).unwrap_or_default();
    let missing = total.saturating_sub(with_vec);
    let coverage = if total == 0 {
        1.0
    } else {
        with_vec as f64 / total as f64
    };
    let dimension = infer_vector_dimension(conn).ok().flatten();
    Ok(VectorHealth {
        total,
        with_vec,
        missing,
        orphans,
        coverage,
        dimension,
        pending_enrichment,
        enrichment_failed_recent,
        enrichment_failures,
    })
}

fn enrichment_failure_summary(
    conn: &rusqlite::Connection,
) -> Result<Vec<EnrichmentFailureSummary>, rusqlite::Error> {
    let mut stmt = conn.prepare(
        "SELECT
             COALESCE(json_extract(metadata, '$.enrichment.failed_stage'), 'unknown') AS stage,
             COALESCE(json_extract(metadata, '$.enrichment.last_error'), '') AS last_error,
             COUNT(*) AS n
         FROM memories
         WHERE json_extract(metadata, '$.enrichment.status') = 'failed'
         GROUP BY stage, last_error
         ORDER BY n DESC
         LIMIT 5",
    )?;
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

pub(crate) fn database_vector_health_json(db_path: &Path) -> serde_json::Value {
    let path_str = match db_path.to_str() {
        Some(s) => s,
        None => {
            return json!({
                "path": db_path.display().to_string(),
                "error": "non-utf8 path",
            });
        }
    };
    match MemoryStore::open_read_only(path_str) {
        Ok(store) => match vector_health(store.connection()) {
            Ok(health) => json!({
                "path": db_path.display().to_string(),
                "memory_total": health.total,
                "vector_count": health.with_vec,
                "vector_missing": health.missing,
                "vector_orphans": health.orphans,
                "coverage": health.coverage,
                "dimension": health.dimension,
                "expected_dimension": EXPECTED_EMBEDDING_DIM,
                "pending_enrichment": health.pending_enrichment,
                "enrichment_failed_recent": health.enrichment_failed_recent,
                "enrichment_failures": health.enrichment_failures,
            }),
            Err(err) => json!({
                "path": db_path.display().to_string(),
                "error": err.to_string(),
            }),
        },
        Err(err) => json!({
            "path": db_path.display().to_string(),
            "error": err.to_string(),
        }),
    }
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
    if let Some(idx) = sql.find("float[") {
        let rest = &sql[idx + "float[".len()..];
        if let Some(end) = rest.find(']') {
            return Ok(rest[..end].parse::<usize>().ok());
        }
    }
    Ok(None)
}


fn count_stuck_in_progress(conn: &rusqlite::Connection) -> Result<usize, rusqlite::Error> {
    let cutoff: DateTime<Utc> = Utc::now() - chrono::Duration::seconds(STUCK_THRESHOLD_SECS);
    let cutoff_s = cutoff.to_rfc3339();
    conn.query_row(
        "SELECT COUNT(*) FROM foundry_jobs WHERE status = 'running' AND updated_at < ?1",
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

pub(crate) fn is_orphan_entry(
    entry: &crate::manifest::DbEntry,
    db_path: &Path,
    global_db_path: &Path,
    project_db_path: Option<&Path>,
) -> bool {
    if paths_equal(db_path, global_db_path) {
        return false;
    }
    if let Some(project) = project_db_path {
        if paths_equal(db_path, project) {
            return false;
        }
    }
    if crate::path_utils::named_project_for_db_path(db_path).is_some() {
        return false;
    }
    !(entry.allow_write
        && entry.schema_kind == "tachi"
        && matches!(
            entry.role,
            DbRole::Agent | DbRole::Foundry | DbRole::Unknown
        ))
}
