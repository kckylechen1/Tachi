//! `tachi status` / `tachi daemon` / `tachi foundry config` handlers.
//!
//! These are lightweight diagnostic commands that read state without
//! holding any DB write locks. They open each manifest DB read-only, query
//! the per-DB foundry histogram, and render a single combined view of:
//!
//! - daemon singleton state (PID file + flock peer)
//! - manifest size + freshness
//! - per-DB foundry job counts + GC-eligible terminal jobs
//! - orphan warnings: manifest entries the running daemon's scheduler
//!   cannot route to (agents/, hub/, vault/, dark DBs)
//! - stuck-in_progress warnings: jobs older than [`STUCK_THRESHOLD_SECS`]
//!   that the safety-net poll should have re-injected by now
//!
//! All output uses ASCII severity icons (`[OK]`, `[!]`, `[X]`) — no
//! emojis, since the terminal rendering target is heterogeneous.

pub(crate) mod status_cli;
pub(crate) mod status_health;

use std::path::{Path, PathBuf};
use std::time::Duration;

use chrono::{DateTime, Utc};
use memory_core::MemoryEntry;
use rusqlite::OptionalExtension;
use serde_json::json;

use memory_core::{job_status_histogram, JobStatusHistogram, MemoryStore};

use crate::daemon_lock::{process_alive, read_pid_file};
use crate::manifest::{DbRole, Manifest};

const STUCK_THRESHOLD_SECS: i64 = 600;

pub(crate) const EXPECTED_EMBEDDING_DIM: usize = 1024;

const DISTILL_STALE_THRESHOLD_SECS: i64 = 36 * 3600;

pub(crate) const WATCH_INTERVAL: Duration = Duration::from_secs(2);

#[derive(Debug, serde::Serialize)]
pub(crate) struct ApiKeyStatus {
    pub(crate) name: String,
    pub(crate) label: String,
    pub(crate) required: bool,
    pub(crate) deprecated: bool,
    pub(crate) status: String,
    pub(crate) source: String,
    pub(crate) env_configured: bool,
    pub(crate) vault_configured: bool,
    pub(crate) drift_warning: Option<String>,
    pub(crate) inferred_invalid_provider: Option<String>,
}

#[derive(Debug, serde::Serialize)]
pub(crate) struct StatusSnapshot {
    pub(crate) daemon: DaemonStatus,
    pub(crate) dbs: Vec<DbStatus>,
    pub(crate) manifest_path: String,
    pub(crate) dispatches: Vec<DispatchStatus>,
    pub(crate) recent_evals: Vec<RecentEval>,
    pub(crate) last_daily_report: Option<String>,
    pub(crate) distill_marker: Option<DistillMarkerStatus>,
    pub(crate) api_keys: Vec<ApiKeyStatus>,
    pub(crate) health_score: u8,
}

#[derive(Debug, serde::Serialize)]
pub(crate) struct DispatchStatus {
    pub(crate) dispatch_id: String,
    pub(crate) agent: String,
    pub(crate) task: String,
    pub(crate) outcome: String,
    pub(crate) elapsed: String,
    pub(crate) reviewed: bool,
}

#[derive(Debug, serde::Serialize)]
pub(crate) struct RecentEval {
    pub(crate) task_id: String,
    pub(crate) agent: String,
    pub(crate) outcome: String,
    pub(crate) quality_score: Option<f64>,
    pub(crate) timestamp: String,
}

#[derive(Debug, serde::Serialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub(crate) enum DaemonStatus {
    Running { pid: i32, lock_path: PathBuf },
    StalePid { pid: i32, lock_path: PathBuf },
    None,
}

#[derive(Debug, serde::Serialize)]
pub(crate) struct DbStatus {
    pub(crate) path: String,
    pub(crate) label: String,
    pub(crate) orphan: bool,
    pub(crate) memory_total: usize,
    pub(crate) vector_count: usize,
    pub(crate) vector_missing: usize,
    pub(crate) vector_orphans: usize,
    pub(crate) vector_coverage: f64,
    pub(crate) vector_dimension: Option<usize>,
    pub(crate) enrichment_failed_recent: usize,
    pub(crate) pending: usize,
    pub(crate) running: usize,
    pub(crate) completed: usize,
    pub(crate) failed: usize,
    pub(crate) gc_eligible: usize,
    pub(crate) stuck_in_progress: usize,
    pub(crate) latest_job: Option<LatestFoundryJob>,
    pub(crate) latest_failed_job: Option<LatestFailedJob>,
    pub(crate) error: Option<String>,
}

#[derive(Debug, Clone, serde::Serialize)]
pub(crate) struct LatestFoundryJob {
    pub(crate) id: String,
    pub(crate) kind: String,
    pub(crate) status: String,
    pub(crate) updated_at: Option<String>,
}

#[derive(Debug, Clone, serde::Serialize)]
pub(crate) struct LatestFailedJob {
    pub(crate) id: String,
    pub(crate) kind: String,
    pub(crate) lane: Option<String>,
    pub(crate) updated_at: Option<String>,
    pub(crate) reason: Option<String>,
    pub(crate) inferred_invalid_provider: Option<String>,
}

#[derive(Debug, serde::Serialize)]
pub(crate) struct DistillMarkerStatus {
    pub(crate) path: String,
    pub(crate) last_run_at: String,
    pub(crate) age_seconds: i64,
    pub(crate) age: String,
    pub(crate) is_stale: bool,
}

pub(crate) fn collect_snapshot(
    app_home: &Path,
    global_db_path: &Path,
    project_db_path: Option<&Path>,
) -> StatusSnapshot {
    let lock_path = app_home.join("daemon.lock");
    let daemon = match read_pid_file(&lock_path) {
        Some(pid) if process_alive(pid) => DaemonStatus::Running {
            pid,
            lock_path: lock_path.clone(),
        },
        Some(pid) => DaemonStatus::StalePid {
            pid,
            lock_path: lock_path.clone(),
        },
        None => DaemonStatus::None,
    };

    let manifest_path = app_home.join("manifest.json");
    let manifest = Manifest::load(&manifest_path).unwrap_or_else(|_| Manifest::empty());

    let mut dbs: Vec<DbStatus> = Vec::with_capacity(manifest.dbs.len());
    for entry in &manifest.dbs {
        let path = PathBuf::from(&entry.path);
        let label = if entry.scope_hint.is_empty() {
            path.file_name()
                .and_then(|s| s.to_str())
                .unwrap_or("?.db")
                .to_string()
        } else {
            entry.scope_hint.clone()
        };
        let orphan = is_orphan_entry(entry, &path, global_db_path, project_db_path);
        if !path.exists() {
            dbs.push(DbStatus {
                path: entry.path.clone(),
                label,
                orphan,
                memory_total: 0,
                vector_count: 0,
                vector_missing: 0,
                vector_orphans: 0,
                vector_coverage: 0.0,
                vector_dimension: None,
                enrichment_failed_recent: 0,
                pending: 0,
                running: 0,
                completed: 0,
                failed: 0,
                gc_eligible: 0,
                stuck_in_progress: 0,
                latest_job: None,
                latest_failed_job: None,
                error: Some("missing on disk".to_string()),
            });
            continue;
        }
        match probe_db(&path) {
            Ok((hist, stuck, vector, latest_job, latest_failed_job)) => dbs.push(DbStatus {
                path: entry.path.clone(),
                label,
                orphan,
                memory_total: vector.total,
                vector_count: vector.with_vec,
                vector_missing: vector.missing,
                vector_orphans: vector.orphans,
                vector_coverage: vector.coverage,
                vector_dimension: vector.dimension,
                enrichment_failed_recent: vector.enrichment_failed_recent,
                pending: hist.queued + hist.planned,
                running: hist.running,
                completed: hist.completed,
                failed: hist.failed,
                gc_eligible: hist.gc_eligible,
                stuck_in_progress: stuck,
                latest_job,
                latest_failed_job,
                error: None,
            }),
            Err(e) => dbs.push(DbStatus {
                path: entry.path.clone(),
                label,
                orphan,
                memory_total: 0,
                vector_count: 0,
                vector_missing: 0,
                vector_orphans: 0,
                vector_coverage: 0.0,
                vector_dimension: None,
                enrichment_failed_recent: 0,
                pending: 0,
                running: 0,
                completed: 0,
                failed: 0,
                gc_eligible: 0,
                stuck_in_progress: 0,
                latest_job: None,
                latest_failed_job: None,
                error: Some(e),
            }),
        }
    }

    let dispatches = collect_dispatches(global_db_path);
    let recent_evals = collect_recent_evals(global_db_path, project_db_path);
    let last_daily_report = find_last_daily_report(app_home);
    let distill_marker = read_distill_marker(app_home);
    let mut api_keys = status_health::collect_api_key_status(global_db_path);
    status_health::apply_inferred_provider_failures(&mut api_keys, &dbs);
    let health_score = status_health::calculate_health_score(
        &daemon,
        &dbs,
        distill_marker.as_ref(),
        &api_keys,
        None,
    );

    StatusSnapshot {
        daemon,
        dbs,
        manifest_path: manifest_path.display().to_string(),
        dispatches,
        recent_evals,
        last_daily_report,
        distill_marker,
        api_keys,
        health_score,
    }
}

#[derive(Debug, Default)]
struct VectorHealth {
    total: usize,
    with_vec: usize,
    missing: usize,
    orphans: usize,
    coverage: f64,
    dimension: Option<usize>,
    enrichment_failed_recent: usize,
}

fn probe_db(
    path: &Path,
) -> Result<
    (
        JobStatusHistogram,
        usize,
        VectorHealth,
        Option<LatestFoundryJob>,
        Option<LatestFailedJob>,
    ),
    String,
> {
    let path_str = path
        .to_str()
        .ok_or_else(|| format!("non-utf8 path: {}", path.display()))?;
    let store = MemoryStore::open_read_only(path_str).map_err(|e| format!("open: {e}"))?;
    let conn = store.connection();
    let hist = job_status_histogram(conn, 30).map_err(|e| format!("histogram: {e}"))?;
    let stuck = count_stuck_in_progress(conn).unwrap_or(0);
    let vector = vector_health(conn).unwrap_or_default();
    let latest_job = latest_foundry_job(conn).unwrap_or(None);
    let latest_failed_job = latest_failed_job(conn).unwrap_or(None);
    Ok((hist, stuck, vector, latest_job, latest_failed_job))
}

fn vector_health(conn: &rusqlite::Connection) -> Result<VectorHealth, rusqlite::Error> {
    let total: usize = conn.query_row("SELECT COUNT(*) FROM memories", [], |row| {
        row.get::<_, i64>(0).map(|n| n as usize)
    })?;
    let with_vec: usize = conn
        .query_row(
            "SELECT COUNT(DISTINCT v.id)
             FROM memories_vec v
             JOIN memories m ON m.id = v.id",
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
        enrichment_failed_recent,
    })
}

pub(crate) fn database_vector_health_json(db_path: &Path) -> serde_json::Value {
    let path_str = match db_path.to_str() {
        Some(s) => s,
        None => {
            return json!({
                "path": db_path.display().to_string(),
                "error": "non-utf8 path",
            })
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
                "enrichment_failed_recent": health.enrichment_failed_recent,
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

pub(crate) fn list_recent_checkpoint_entries(
    server: &crate::MemoryServer,
    limit: usize,
) -> Vec<serde_json::Value> {
    list_recent_entries_by_path(server, "/agent/checkpoints/", limit)
}

pub(crate) fn list_recent_kanban_entries(
    server: &crate::MemoryServer,
    limit: usize,
) -> Vec<serde_json::Value> {
    list_recent_entries_by_path(server, "/kanban/tasks/", limit)
}

fn list_recent_entries_by_path(
    server: &crate::MemoryServer,
    path_prefix: &str,
    limit: usize,
) -> Vec<serde_json::Value> {
    let limit = limit.max(1).min(50);
    let mut rows = Vec::new();
    collect_entries_for_status(
        server.global_db_path_buf().as_path(),
        path_prefix,
        limit,
        "global",
        &mut rows,
    );
    if let Some(path) = server.project_db_path_buf() {
        collect_entries_for_status(path.as_path(), path_prefix, limit, "project", &mut rows);
    }
    rows.sort_by(|a, b| {
        let aa = a.get("updated_at").and_then(|v| v.as_str()).unwrap_or("");
        let bb = b.get("updated_at").and_then(|v| v.as_str()).unwrap_or("");
        bb.cmp(aa)
    });
    rows.truncate(limit);
    rows
}

fn collect_entries_for_status(
    db_path: &Path,
    path_prefix: &str,
    limit: usize,
    db_label: &str,
    out: &mut Vec<serde_json::Value>,
) {
    let Some(path_str) = db_path.to_str() else {
        return;
    };
    let Ok(store) = MemoryStore::open_read_only(path_str) else {
        return;
    };
    let entries: Vec<MemoryEntry> = match store.list_by_path(path_prefix, limit, false) {
        Ok(entries) => entries,
        Err(_) => return,
    };
    for entry in entries {
        out.push(json!({
            "id": entry.id,
            "path": entry.path,
            "summary": entry.summary,
            "updated_at": entry.timestamp,
            "topic": entry.topic,
            "category": entry.category,
            "db": db_label,
        }));
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

fn latest_failed_job(
    conn: &rusqlite::Connection,
) -> Result<Option<LatestFailedJob>, rusqlite::Error> {
    conn.query_row(
        "SELECT id, kind, lane, updated_at, metadata
         FROM foundry_jobs
         WHERE status = 'failed'
         ORDER BY updated_at DESC, created_at DESC
         LIMIT 1",
        [],
        |row| {
            let metadata: String = row.get(4)?;
            let reason = extract_terminal_reason(&metadata);
            let kind: String = row.get(1)?;
            let lane: Option<String> = row.get::<_, Option<String>>(2)?;
            Ok(LatestFailedJob {
                id: row.get(0)?,
                kind: kind.clone(),
                lane: lane.clone(),
                updated_at: row.get::<_, Option<String>>(3)?,
                inferred_invalid_provider: reason.as_deref().and_then(|reason| {
                    status_health::infer_provider_from_failed_job(&kind, lane.as_deref(), reason)
                }),
                reason,
            })
        },
    )
    .optional()
}

fn latest_foundry_job(
    conn: &rusqlite::Connection,
) -> Result<Option<LatestFoundryJob>, rusqlite::Error> {
    conn.query_row(
        "SELECT id, kind, status, updated_at
         FROM foundry_jobs
         ORDER BY updated_at DESC, created_at DESC
         LIMIT 1",
        [],
        |row| {
            Ok(LatestFoundryJob {
                id: row.get(0)?,
                kind: row.get(1)?,
                status: row.get(2)?,
                updated_at: row.get::<_, Option<String>>(3)?,
            })
        },
    )
    .optional()
}

fn extract_terminal_reason(metadata: &str) -> Option<String> {
    let value: serde_json::Value = serde_json::from_str(metadata).ok()?;
    value
        .pointer("/terminal_reason/reason")
        .and_then(|v| v.as_str())
        .or_else(|| value.pointer("/error").and_then(|v| v.as_str()))
        .or_else(|| value.pointer("/last_error").and_then(|v| v.as_str()))
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

fn count_stuck_in_progress(conn: &rusqlite::Connection) -> Result<usize, rusqlite::Error> {
    let cutoff: DateTime<Utc> = Utc::now() - chrono::Duration::seconds(STUCK_THRESHOLD_SECS);
    let cutoff_s = cutoff.to_rfc3339();
    conn.query_row(
        "SELECT COUNT(*) FROM foundry_jobs WHERE status = 'in_progress' AND updated_at < ?1",
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

fn is_orphan_entry(
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
    if named_project_from_path(db_path).is_some() {
        return false;
    }
    !(entry.allow_write
        && entry.schema_kind == "tachi"
        && matches!(
            entry.role,
            DbRole::Agent | DbRole::Foundry | DbRole::Unknown
        ))
}

fn named_project_from_path(db_path: &Path) -> Option<String> {
    let parent = db_path.parent()?;
    let name = parent.file_name()?.to_str()?;
    let grand = parent.parent()?;
    let grand_name = grand.file_name()?.to_str()?;
    let root = grand.parent()?;
    let root_name = root.file_name()?.to_str()?;
    if grand_name == "projects"
        && root_name == ".tachi"
        && db_path.file_name()?.to_str()? == "memory.db"
    {
        Some(name.to_string())
    } else {
        None
    }
}

fn paths_equal(a: &Path, b: &Path) -> bool {
    match (std::fs::canonicalize(a), std::fs::canonicalize(b)) {
        (Ok(x), Ok(y)) => x == y,
        _ => a == b,
    }
}

pub(crate) fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else if max <= 3 {
        s.chars().take(max).collect()
    } else {
        format!(
            "{}...",
            s.chars().take(max.saturating_sub(3)).collect::<String>()
        )
    }
}

fn collect_dispatches(global_db_path: &Path) -> Vec<DispatchStatus> {
    let path_str = match global_db_path.to_str() {
        Some(s) => s,
        None => return Vec::new(),
    };
    let store = match MemoryStore::open_read_only(path_str) {
        Ok(s) => s,
        Err(_) => return Vec::new(),
    };
    let conn = store.connection();
    let mut stmt = match conn.prepare(
        "SELECT id, summary, text, metadata, created_at FROM memories \
         WHERE path LIKE '/kanban/tasks/%' \
         ORDER BY created_at DESC LIMIT 10",
    ) {
        Ok(s) => s,
        Err(_) => return Vec::new(),
    };
    let now = Utc::now();
    let rows = match stmt.query_map([], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, String>(2)?,
            row.get::<_, String>(3)?,
            row.get::<_, String>(4)?,
        ))
    }) {
        Ok(r) => r,
        Err(_) => return Vec::new(),
    };
    let mut out = Vec::new();
    for row in rows {
        let (id, summary, text, meta_str, created_at) = match row {
            Ok(r) => r,
            Err(_) => continue,
        };
        let meta: serde_json::Value = serde_json::from_str(&meta_str).unwrap_or(json!({}));
        let agent = meta
            .get("agent")
            .and_then(|v| v.as_str())
            .unwrap_or("unknown")
            .to_string();
        let outcome = meta
            .get("a2a_state")
            .and_then(|v| v.as_str())
            .map(|s| match s {
                "TASK_STATE_IN_PROGRESS" => "in_progress",
                "TASK_STATE_COMPLETED" => "completed",
                "TASK_STATE_FAILED" => "failed",
                "TASK_STATE_CANCELED" => "aborted",
                "TASK_STATE_INPUT_REQUIRED" => "partial",
                other => other,
            })
            .unwrap_or("unknown")
            .to_string();
        let reviewed = meta
            .get("reviewed")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        let elapsed = created_at
            .parse::<DateTime<Utc>>()
            .ok()
            .map(|dt| status_health::format_elapsed(now - dt))
            .unwrap_or_default();
        let dispatch_id = meta
            .get("dispatch_id")
            .and_then(|v| v.as_str())
            .unwrap_or(&id)
            .chars()
            .take(12)
            .collect();
        let task = meta
            .get("task")
            .and_then(|v| v.as_str())
            .map(|s| s.chars().take(60).collect::<String>())
            .or_else(|| {
                text.lines()
                    .find(|l| l.starts_with("Task: "))
                    .map(|l| l.trim_start_matches("Task: ").chars().take(60).collect())
            })
            .unwrap_or_else(|| {
                summary
                    .trim_start_matches(|c: char| !c.is_alphanumeric())
                    .chars()
                    .take(60)
                    .collect()
            });
        out.push(DispatchStatus {
            dispatch_id,
            agent,
            task,
            outcome,
            elapsed,
            reviewed,
        });
    }
    out
}

fn collect_recent_evals(global_db_path: &Path, project_db_path: Option<&Path>) -> Vec<RecentEval> {
    let mut evals = Vec::new();
    for db_path in std::iter::once(global_db_path).chain(project_db_path) {
        let path_str = match db_path.to_str() {
            Some(s) => s,
            None => continue,
        };
        let store = match MemoryStore::open_read_only(path_str) {
            Ok(s) => s,
            Err(_) => continue,
        };
        let conn = store.connection();
        let mut stmt = match conn.prepare(
            "SELECT id, summary, metadata, created_at FROM memories \
             WHERE path LIKE '/eval/2%' \
               AND id NOT LIKE 'foundry:%' \
               AND category = 'experience' \
             ORDER BY created_at DESC LIMIT 5",
        ) {
            Ok(s) => s,
            Err(_) => continue,
        };
        let rows = match stmt.query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
            ))
        }) {
            Ok(r) => r,
            Err(_) => continue,
        };
        for row in rows {
            let (id, _summary, meta_str, created_at) = match row {
                Ok(r) => r,
                Err(_) => continue,
            };
            let meta: serde_json::Value = serde_json::from_str(&meta_str).unwrap_or(json!({}));
            let agent = meta
                .get("agent")
                .and_then(|v| v.as_str())
                .unwrap_or("unknown")
                .to_string();
            let outcome = meta
                .get("outcome")
                .and_then(|v| v.as_str())
                .unwrap_or("unknown")
                .to_string();
            let quality_score = meta.get("quality_score").and_then(|v| v.as_f64());
            let task_id = id.chars().take(24).collect();
            evals.push(RecentEval {
                task_id,
                agent,
                outcome,
                quality_score,
                timestamp: created_at,
            });
        }
    }
    evals.sort_by(|a, b| b.timestamp.cmp(&a.timestamp));
    evals.truncate(5);
    evals
}

fn find_last_daily_report(app_home: &Path) -> Option<String> {
    let reports_dir = app_home.join("reports").join("daily");
    let entries = std::fs::read_dir(&reports_dir).ok()?;
    let mut files: Vec<String> = entries
        .filter_map(|e| e.ok())
        .filter(|e| e.path().extension().map(|ext| ext == "md").unwrap_or(false))
        .filter_map(|e| e.file_name().to_str().map(String::from))
        .collect();
    files.sort();
    files
        .last()
        .map(|f| reports_dir.join(f).display().to_string())
}

fn read_distill_marker(app_home: &Path) -> Option<DistillMarkerStatus> {
    let marker_path = app_home.join("foundry-runs").join(".last_distill_run");
    let raw = std::fs::read_to_string(&marker_path).ok()?;
    let last_run_at = raw.trim().to_string();
    if last_run_at.is_empty() {
        return None;
    }
    let dt = DateTime::parse_from_rfc3339(&last_run_at)
        .ok()?
        .with_timezone(&Utc);
    let age = Utc::now() - dt;
    let age_seconds = age.num_seconds().max(0);
    Some(DistillMarkerStatus {
        path: marker_path.display().to_string(),
        last_run_at,
        age_seconds,
        age: status_health::format_elapsed(age),
        is_stale: age_seconds > DISTILL_STALE_THRESHOLD_SECS,
    })
}

/// MCP tool handler: returns a concise JSON health summary for agents.
pub(crate) async fn handle_tachi_status_agent(
    server: &crate::MemoryServer,
) -> Result<String, String> {
    handle_tachi_status_detail(server, false).await
}

/// Full diagnostic JSON for tests, doctor flows, and readiness checks.
pub(crate) async fn handle_tachi_status_full(
    server: &crate::MemoryServer,
) -> Result<String, String> {
    handle_tachi_status_detail(server, true).await
}

async fn handle_tachi_status_detail(
    server: &crate::MemoryServer,
    full: bool,
) -> Result<String, String> {
    let app_home: PathBuf = std::env::var("TACHI_HOME")
        .ok()
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            dirs::home_dir()
                .unwrap_or_else(|| PathBuf::from("."))
                .join(".tachi")
        });
    let global_db_path = server.global_db_path_buf();
    let project_db_path = server.project_db_path_buf();

    let snapshot = collect_snapshot(&app_home, &global_db_path, project_db_path.as_deref());

    let daemon_state = match &snapshot.daemon {
        DaemonStatus::Running { pid, .. } => json!({
            "running": true,
            "pid": pid,
        }),
        DaemonStatus::StalePid { pid, .. } => json!({
            "running": false,
            "stale": true,
            "pid": pid,
        }),
        DaemonStatus::None => json!({
            "running": false,
        }),
    };

    let total_dbs = snapshot.dbs.len();
    let total_pending: usize = snapshot.dbs.iter().map(|d| d.pending).sum();
    let total_failed: usize = snapshot.dbs.iter().map(|d| d.failed).sum();
    let total_stuck: usize = snapshot.dbs.iter().map(|d| d.stuck_in_progress).sum();
    let low_coverage: Vec<serde_json::Value> = snapshot
        .dbs
        .iter()
        .filter(|d| d.memory_total > 0 && d.vector_coverage < 0.9)
        .map(|d| {
            json!({
                "label": d.label,
                "coverage": format!("{:.1}%", d.vector_coverage * 100.0),
                "missing": d.vector_missing,
                "total": d.memory_total,
                "dimension": d.vector_dimension,
                "backfill_command": status_health::format_backfill_command(d),
            })
        })
        .collect();
    let vector_dimension_mismatches: Vec<serde_json::Value> = snapshot
        .dbs
        .iter()
        .filter(|d| vector_dimension_mismatch(d))
        .map(|d| {
            json!({
                "label": d.label,
                "path": d.path,
                "dimension": d.vector_dimension,
                "expected_dimension": EXPECTED_EMBEDDING_DIM,
                "remediation": "Rebuild/backfill memories_vec with voyage-4 1024-d embeddings.",
            })
        })
        .collect();
    let vector_orphan_dbs: Vec<serde_json::Value> = snapshot
        .dbs
        .iter()
        .filter(|d| d.vector_orphans > 0)
        .map(|d| {
            json!({
                "label": d.label,
                "path": d.path,
                "orphans": d.vector_orphans,
                "remediation": "Run `tachi repair --rule R7 --apply` to remove vector rows whose memory no longer exists.",
            })
        })
        .collect();
    let auth_failures: Vec<serde_json::Value> = snapshot
        .dbs
        .iter()
        .filter_map(|d| {
            d.latest_failed_job.as_ref().and_then(|job| {
                let reason = job.reason.as_deref()?;
                job.inferred_invalid_provider.is_some().then(|| {
                    json!({
                        "db": d.label,
                        "kind": job.kind,
                        "lane": job.lane,
                        "updated_at": job.updated_at,
                        "reason": reason,
                        "inferred_invalid_provider": job.inferred_invalid_provider,
                    })
                })
            })
        })
        .collect();
    let failed_jobs: Vec<serde_json::Value> = snapshot
        .dbs
        .iter()
        .filter_map(|d| {
            d.latest_failed_job.as_ref().map(|job| {
                json!({
                    "db": d.label,
                    "path": d.path,
                    "id": job.id,
                    "kind": job.kind,
                    "lane": job.lane,
                    "updated_at": job.updated_at,
                    "reason": job.reason,
                    "inferred_invalid_provider": job.inferred_invalid_provider,
                })
            })
        })
        .collect();
    let readiness = status_health::agent_readiness_json(&app_home, &snapshot);

    let warnings = build_status_warnings(&snapshot, &daemon_state);

    if full {
        serde_json::to_string(&json!({
            "daemon": daemon_state,
            "version": env!("CARGO_PKG_VERSION"),
            "health_score": snapshot.health_score,
            "databases": {
                "total": total_dbs,
                "pending_jobs": total_pending,
                "failed_jobs": total_failed,
                "stuck_jobs": total_stuck,
                "low_vector_coverage": low_coverage,
                "vector_dimension_mismatches": vector_dimension_mismatches,
                "vector_orphans": vector_orphan_dbs,
                "provider_auth_failures": auth_failures,
                "latest_failed_jobs": failed_jobs,
            },
            "warnings": warnings,
            "daily_pipeline": snapshot.last_daily_report,
            "distill": snapshot.distill_marker,
            "api_keys": snapshot.api_keys,
            "models": status_health::model_lanes_json(),
            "agent_readiness": readiness,
        }))
        .map_err(|e| e.to_string())
    } else {
        let api_key_drift = snapshot
            .api_keys
            .iter()
            .filter(|key| key.status == "drift")
            .count();
        let api_key_missing = snapshot
            .api_keys
            .iter()
            .filter(|key| key.required && key.status == "missing")
            .count();
        let distill = snapshot.distill_marker.as_ref().map(|marker| {
            json!({
                "is_stale": marker.is_stale,
                "age": marker.age,
            })
        });
        serde_json::to_string(&json!({
            "detail": "agent",
            "daemon": daemon_state,
            "version": env!("CARGO_PKG_VERSION"),
            "health_score": snapshot.health_score,
            "warnings": warnings.into_iter().take(8).collect::<Vec<_>>(),
            "jobs": {
                "failed": total_failed,
                "pending": total_pending,
                "stuck": total_stuck,
            },
            "distill": distill,
            "vector_coverage_issues": low_coverage.len(),
            "provider_auth_failures": auth_failures.len(),
            "api_keys": {
                "drift": api_key_drift,
                "missing_required": api_key_missing,
            },
            "doctor_hint": readiness.get("doctor_hint"),
        }))
        .map_err(|e| e.to_string())
    }
}

/// Lightweight warning lines for `tachi_memory action=alerts` — no provider keys, models, or skill matrices.
pub(crate) async fn collect_agent_warning_lines(server: &crate::MemoryServer) -> Vec<String> {
    let global_db = server.global_db_path_buf();
    let project_db = server.project_db_path_buf();
    tokio::task::spawn_blocking(move || {
        let app_home: PathBuf = std::env::var("TACHI_HOME")
            .ok()
            .map(PathBuf::from)
            .unwrap_or_else(|| {
                dirs::home_dir()
                    .unwrap_or_else(|| PathBuf::from("."))
                    .join(".tachi")
            });
        let snapshot = collect_snapshot(&app_home, &global_db, project_db.as_deref());
        let daemon_state = match &snapshot.daemon {
            DaemonStatus::Running { .. } => json!({ "running": true }),
            DaemonStatus::StalePid { .. } => json!({ "running": false, "stale": true }),
            DaemonStatus::None => json!({ "running": false }),
        };
        build_status_warnings(&snapshot, &daemon_state)
    })
    .await
    .unwrap_or_default()
}

fn build_status_warnings(
    snapshot: &StatusSnapshot,
    daemon_state: &serde_json::Value,
) -> Vec<String> {
    let total_dbs = snapshot.dbs.len();
    let total_failed: usize = snapshot.dbs.iter().map(|d| d.failed).sum();
    let total_stuck: usize = snapshot.dbs.iter().map(|d| d.stuck_in_progress).sum();
    let low_coverage_count = snapshot
        .dbs
        .iter()
        .filter(|d| d.memory_total > 0 && d.vector_coverage < 0.9)
        .count();
    let vector_dimension_mismatch_count = snapshot
        .dbs
        .iter()
        .filter(|d| vector_dimension_mismatch(d))
        .count();
    let auth_failures = snapshot
        .dbs
        .iter()
        .filter(|d| {
            d.latest_failed_job
                .as_ref()
                .and_then(|job| job.inferred_invalid_provider.as_ref())
                .is_some()
        })
        .count();

    let mut warnings: Vec<String> = Vec::new();
    if !daemon_state["running"].as_bool().unwrap_or(false) {
        warnings.push(
            "daemon not running — background tasks (enrichment, distill, GC) are paused"
                .to_string(),
        );
    }
    if low_coverage_count > 0 {
        warnings.push(format!(
            "{low_coverage_count} db(s) have vector coverage below 90%"
        ));
    }
    if vector_dimension_mismatch_count > 0 {
        warnings.push(format!(
            "{vector_dimension_mismatch_count} db(s) have vector dimension metadata that differs from expected {EXPECTED_EMBEDDING_DIM}"
        ));
    }
    if total_failed > 0 {
        warnings.push(format!(
            "{total_failed} foundry job(s) failed across {total_dbs} dbs"
        ));
    }
    if auth_failures > 0 {
        warnings.push("latest foundry failures include provider auth/API-key errors".to_string());
    }
    if total_stuck > 0 {
        warnings.push(format!(
            "{total_stuck} foundry job(s) stuck in_progress for over {STUCK_THRESHOLD_SECS}s"
        ));
    }
    if snapshot
        .distill_marker
        .as_ref()
        .map(|marker| marker.is_stale)
        .unwrap_or(true)
    {
        warnings.push("daily distill marker is missing or stale".to_string());
    }
    for key in &snapshot.api_keys {
        if key.required && key.status == "missing" {
            warnings.push(format!(
                "required provider key {} is missing (add to Tachi Vault or config env)",
                key.name
            ));
        }
        if let Some(provider) = &key.inferred_invalid_provider {
            warnings.push(format!(
                "provider key {} appears invalid for provider {} based on latest failed jobs",
                key.name, provider
            ));
        }
    }
    warnings
}

pub(crate) fn vector_dimension_mismatch(db: &DbStatus) -> bool {
    db.vector_count > 0 && db.vector_dimension != Some(EXPECTED_EMBEDDING_DIM)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(role: DbRole, scope_hint: &str) -> crate::manifest::DbEntry {
        crate::manifest::DbEntry {
            path: "/tmp/status/memory.db".to_string(),
            role,
            owner: "test".to_string(),
            schema_kind: "tachi".to_string(),
            vec_enabled: true,
            allow_write: true,
            last_doctor_at: String::new(),
            last_classification: "healthy".to_string(),
            scope_hint: scope_hint.to_string(),
            notes: String::new(),
        }
    }

    #[test]
    fn orphan_classification_matches_scheduler_routing() {
        let global = PathBuf::from("/tmp/status/global/memory.db");
        let project = PathBuf::from("/tmp/status/project/memory.db");
        let named = PathBuf::from("/home/u/.tachi/projects/sigil/memory.db");
        let agent = PathBuf::from("/home/u/.tachi/agents/main/memory.db");
        assert!(!is_orphan_entry(
            &entry(DbRole::Global, "global"),
            &global,
            &global,
            Some(&project)
        ));
        assert!(!is_orphan_entry(
            &entry(DbRole::Project, "project"),
            &project,
            &global,
            Some(&project)
        ));
        assert!(!is_orphan_entry(
            &entry(DbRole::Project, "project:sigil"),
            &named,
            &global,
            Some(&project)
        ));
        assert!(!is_orphan_entry(
            &entry(DbRole::Agent, "openclaw-agent:main"),
            &agent,
            &global,
            Some(&project)
        ));
    }

    #[test]
    fn truncate_honors_max() {
        assert_eq!(truncate("abc", 5), "abc");
        assert_eq!(truncate("abcdefghij", 5), "ab...");
    }

    #[test]
    fn infer_provider_from_auth_error_maps_real_failures() {
        assert_eq!(
            status_health::infer_provider_from_failed_job(
                "recall_rerank_cache",
                Some("rerank"),
                "SiliconFlow 403 forbidden"
            ),
            Some("SILICONFLOW".to_string())
        );
        assert_eq!(
            status_health::infer_provider_from_auth_error("Voyage API error: 403 Forbidden"),
            Some("VOYAGE".to_string())
        );
        assert_eq!(
            status_health::infer_provider_from_failed_job(
                "memory_distill",
                Some("distill"),
                "403 Forbidden"
            ),
            Some("SILICONFLOW".to_string())
        );
        assert_eq!(
            status_health::infer_provider_from_auth_error("network timeout"),
            None
        );
    }

    #[test]
    fn checkpoint_fixture_classifier_recognizes_only_real_fixtures() {
        let sep = std::path::MAIN_SEPARATOR;
        let cases = [
            (
                format!("{sep}Users{sep}u{sep}.tachi{sep}global{sep}memory.db"),
                false,
            ),
            (
                format!("{sep}home{sep}u{sep}.openclaw{sep}agents{sep}main{sep}memory.db"),
                false,
            ),
            (
                format!("{sep}srv{sep}checkpointed{sep}prod{sep}memory.db"),
                false,
            ),
            (
                format!(
                    "{sep}tmp{sep}feature-daemon-global.db.checkpointed.20260430T012609Z.sqlite"
                ),
                false,
            ),
            (
                format!("{sep}tmp{sep}feature-daemon-global.db.checkpointed.20260430T012609Z.db"),
                true,
            ),
            (
                format!("{sep}tmp{sep}feature-daemon-project.db.checkpointed.20260430T015747Z.db"),
                true,
            ),
            (
                format!("{sep}tmp{sep}foo.db.CHECKPOINTED.20260430T015747Z.DB"),
                true,
            ),
        ];
        for (path, expected) in cases {
            assert_eq!(
                status_cli::is_checkpoint_fixture_path(&path),
                expected,
                "is_checkpoint_fixture_path({path:?}) misclassified"
            );
        }
    }
}
