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
//! - stuck running warnings: jobs older than [`STUCK_THRESHOLD_SECS`]
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

pub(crate) const STUCK_THRESHOLD_SECS: i64 = 600;
const DISPATCH_STALE_THRESHOLD_SECS: i64 = 6 * 60 * 60;

pub(crate) const EXPECTED_EMBEDDING_DIM: usize = 1024;
const FOUNDRY_RECALL_CACHE_SOURCE: &str = "foundry_recall_rerank_cache";

const DISTILL_STALE_THRESHOLD_SECS: i64 = 36 * 3600;

pub(crate) const WATCH_INTERVAL: Duration = Duration::from_secs(2);

#[derive(Debug, Clone, serde::Deserialize, serde::Serialize)]
pub(crate) struct ApiKeyRotationMemberStatus {
    pub(crate) name: String,
    pub(crate) status: String,
    pub(crate) message: Option<String>,
    pub(crate) last_probe_at: Option<String>,
}

#[derive(Debug, serde::Serialize)]
pub(crate) struct ApiKeyRotationStatus {
    pub(crate) total_keys: i64,
    pub(crate) configured_keys: i64,
    pub(crate) healthy_keys: Option<i64>,
    pub(crate) rate_limited_keys: i64,
    pub(crate) auth_failed_keys: i64,
    pub(crate) current_index: i64,
    pub(crate) strategy: String,
    pub(crate) next_retry_at: Option<String>,
    pub(crate) members: Vec<ApiKeyRotationMemberStatus>,
}

#[derive(Debug, serde::Serialize)]
pub(crate) struct ApiKeyStatus {
    pub(crate) name: String,
    pub(crate) label: String,
    pub(crate) required: bool,
    pub(crate) deprecated: bool,
    pub(crate) canonical_name: String,
    pub(crate) alias_names: Vec<String>,
    pub(crate) status: String,
    pub(crate) source: String,
    pub(crate) env_configured: bool,
    pub(crate) vault_configured: bool,
    pub(crate) cleanup_hint: Option<String>,
    pub(crate) drift_warning: Option<String>,
    pub(crate) inferred_invalid_provider: Option<String>,
    pub(crate) rotation: Option<ApiKeyRotationStatus>,
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
    pub(crate) provider_probe_cache: Option<status_health::ProviderProbeCache>,
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
    Running {
        pid: i32,
        lock_path: PathBuf,
    },
    Foreign {
        pid: i32,
        lock_path: PathBuf,
        reason: String,
        version: Option<String>,
        port: Option<u16>,
        global_db: Option<String>,
    },
    StalePid {
        pid: i32,
        lock_path: PathBuf,
    },
    None,
}

#[derive(Debug, Clone, Default, serde::Serialize)]
pub(crate) struct DaemonPidInfo {
    pub(crate) pid: Option<i32>,
    pub(crate) port: Option<u16>,
    pub(crate) version: Option<String>,
    pub(crate) global_db: Option<String>,
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
    pub(crate) pending_enrichment: usize,
    pub(crate) enrichment_failed_recent: usize,
    pub(crate) enrichment_failures: Vec<EnrichmentFailureSummary>,
    pub(crate) pending: usize,
    pub(crate) running: usize,
    pub(crate) active_jobs: usize,
    pub(crate) completed: usize,
    pub(crate) failed: usize,
    pub(crate) skipped: usize,
    pub(crate) terminal_jobs: usize,
    pub(crate) gc_eligible: usize,
    pub(crate) stuck_in_progress: usize,
    pub(crate) latest_active_job: Option<LatestFoundryJob>,
    pub(crate) latest_terminal_job: Option<LatestFoundryJob>,
    pub(crate) latest_job: Option<LatestFoundryJob>,
    pub(crate) latest_failed_job: Option<LatestFailedJob>,
    pub(crate) error: Option<String>,
}

#[derive(Debug, Clone, serde::Serialize)]
pub(crate) struct EnrichmentFailureSummary {
    pub(crate) stage: String,
    pub(crate) last_error: String,
    pub(crate) count: usize,
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
    collect_snapshot_inner(app_home, global_db_path, project_db_path, false)
}

pub(crate) fn collect_snapshot_with_provider_value_compare(
    app_home: &Path,
    global_db_path: &Path,
    project_db_path: Option<&Path>,
) -> StatusSnapshot {
    collect_snapshot_inner(app_home, global_db_path, project_db_path, true)
}

fn collect_snapshot_inner(
    app_home: &Path,
    global_db_path: &Path,
    project_db_path: Option<&Path>,
    compare_provider_values: bool,
) -> StatusSnapshot {
    let lock_path = app_home.join("daemon.lock");
    let pid_info = read_daemon_pid_info(app_home);
    let daemon = match read_pid_file(&lock_path) {
        Some(pid) if process_alive(pid) => {
            if let Some(reason) = daemon_mismatch_reason(pid, pid_info.as_ref(), global_db_path) {
                DaemonStatus::Foreign {
                    pid,
                    lock_path,
                    reason,
                    version: pid_info.as_ref().and_then(|info| info.version.clone()),
                    port: pid_info.as_ref().and_then(|info| info.port),
                    global_db: pid_info.as_ref().and_then(|info| info.global_db.clone()),
                }
            } else {
                DaemonStatus::Running { pid, lock_path }
            }
        }
        Some(pid) => DaemonStatus::StalePid { pid, lock_path },
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
                pending_enrichment: 0,
                enrichment_failed_recent: 0,
                enrichment_failures: Vec::new(),
                pending: 0,
                running: 0,
                active_jobs: 0,
                completed: 0,
                failed: 0,
                skipped: 0,
                terminal_jobs: 0,
                gc_eligible: 0,
                stuck_in_progress: 0,
                latest_active_job: None,
                latest_terminal_job: None,
                latest_job: None,
                latest_failed_job: None,
                error: Some("missing on disk".to_string()),
            });
            continue;
        }
        match probe_db(&path) {
            Ok((
                hist,
                stuck,
                vector,
                latest_active_job,
                latest_terminal_job,
                latest_job,
                latest_failed_job,
            )) => dbs.push(DbStatus {
                path: entry.path.clone(),
                label,
                orphan,
                memory_total: vector.total,
                vector_count: vector.with_vec,
                vector_missing: vector.missing,
                vector_orphans: vector.orphans,
                vector_coverage: vector.coverage,
                vector_dimension: vector.dimension,
                pending_enrichment: vector.pending_enrichment,
                enrichment_failed_recent: vector.enrichment_failed_recent,
                enrichment_failures: vector.enrichment_failures,
                pending: hist.queued + hist.planned,
                running: hist.running,
                active_jobs: hist.planned + hist.queued + hist.running,
                completed: hist.completed,
                failed: hist.failed,
                skipped: hist.skipped,
                terminal_jobs: hist.completed + hist.failed + hist.skipped,
                gc_eligible: hist.gc_eligible,
                stuck_in_progress: stuck,
                latest_active_job,
                latest_terminal_job,
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
                pending_enrichment: 0,
                enrichment_failed_recent: 0,
                enrichment_failures: Vec::new(),
                pending: 0,
                running: 0,
                active_jobs: 0,
                completed: 0,
                failed: 0,
                skipped: 0,
                terminal_jobs: 0,
                gc_eligible: 0,
                stuck_in_progress: 0,
                latest_active_job: None,
                latest_terminal_job: None,
                latest_job: None,
                latest_failed_job: None,
                error: Some(e),
            }),
        }
    }

    let dispatches = collect_dispatches(global_db_path, project_db_path);
    let recent_evals = collect_recent_evals(global_db_path, project_db_path);
    let last_daily_report = find_last_daily_report(app_home);
    let distill_marker = read_distill_marker(app_home);
    let provider_probe_cache = status_health::read_provider_probe_cache(app_home);
    let fresh_provider_probe_cache = provider_probe_cache
        .as_ref()
        .filter(|cache| !cache.is_stale());
    let mut api_keys = if compare_provider_values {
        status_health::collect_api_key_status_with_probe_cache(
            global_db_path,
            fresh_provider_probe_cache,
            true,
        )
    } else {
        status_health::collect_api_key_status_with_probe_cache(
            global_db_path,
            fresh_provider_probe_cache,
            false,
        )
    };
    status_health::apply_inferred_provider_failures(&mut api_keys, &dbs);
    let cached_probe_results = provider_probe_cache
        .as_ref()
        .filter(|cache| !cache.is_stale())
        .map(|cache| cache.probes.as_slice());
    let health_score = status_health::calculate_health_score(
        &daemon,
        &dbs,
        distill_marker.as_ref(),
        &api_keys,
        cached_probe_results,
        fresh_provider_probe_cache.map(|cache| cache.rotation_groups.as_slice()),
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
        provider_probe_cache,
        health_score,
    }
}

pub(crate) fn read_daemon_pid_info(app_home: &Path) -> Option<DaemonPidInfo> {
    let raw = std::fs::read_to_string(app_home.join("daemon.pid")).ok()?;
    let parsed: serde_json::Value = serde_json::from_str(&raw).ok()?;
    Some(DaemonPidInfo {
        pid: parsed
            .get("pid")
            .and_then(|value| value.as_i64())
            .and_then(|value| i32::try_from(value).ok()),
        port: parsed
            .get("port")
            .and_then(|value| value.as_u64())
            .and_then(|value| u16::try_from(value).ok()),
        version: parsed
            .get("version")
            .and_then(|value| value.as_str())
            .map(str::to_string),
        global_db: parsed
            .get("global_db")
            .and_then(|value| value.as_str())
            .map(str::to_string),
    })
}

pub(crate) fn daemon_mismatch_reason(
    lock_pid: i32,
    pid_info: Option<&DaemonPidInfo>,
    global_db_path: &Path,
) -> Option<String> {
    let Some(info) = pid_info else {
        return Some("daemon.pid missing or unreadable".to_string());
    };
    if let Some(pid) = info.pid {
        if pid != lock_pid {
            return Some(format!(
                "daemon.pid pid={pid} does not match lock pid={lock_pid}"
            ));
        }
    }
    match info.version.as_deref() {
        Some(version) => {
            if version != env!("CARGO_PKG_VERSION") {
                return Some(format!(
                    "daemon version {version} does not match binary {}",
                    env!("CARGO_PKG_VERSION")
                ));
            }
        }
        None => return Some("daemon.pid missing version".to_string()),
    }
    if let Some(daemon_global) = info.global_db.as_deref() {
        if !daemon_global.is_empty() && !path_matches_string(global_db_path, daemon_global) {
            return Some(format!(
                "daemon global_db {} does not match {}",
                daemon_global,
                global_db_path.display()
            ));
        }
    }
    None
}

fn path_matches_string(left: &Path, right: &str) -> bool {
    if left.as_os_str() == right {
        return true;
    }
    std::fs::canonicalize(left)
        .ok()
        .zip(std::fs::canonicalize(right).ok())
        .map(|(left, right)| left == right)
        .unwrap_or(false)
}

#[derive(Debug, Default)]
struct VectorHealth {
    total: usize,
    with_vec: usize,
    missing: usize,
    orphans: usize,
    coverage: f64,
    dimension: Option<usize>,
    pending_enrichment: usize,
    enrichment_failed_recent: usize,
    enrichment_failures: Vec<EnrichmentFailureSummary>,
}

type ProbeDbResult = (
    JobStatusHistogram,
    usize,
    VectorHealth,
    Option<LatestFoundryJob>,
    Option<LatestFoundryJob>,
    Option<LatestFoundryJob>,
    Option<LatestFailedJob>,
);

fn probe_db(path: &Path) -> Result<ProbeDbResult, String> {
    let path_str = path
        .to_str()
        .ok_or_else(|| format!("non-utf8 path: {}", path.display()))?;
    let store = MemoryStore::open_read_only(path_str).map_err(|e| format!("open: {e}"))?;
    let conn = store.connection();
    let hist = job_status_histogram(conn, 30).map_err(|e| format!("histogram: {e}"))?;
    let stuck = count_stuck_in_progress(conn).unwrap_or(0);
    let vector = vector_health(conn).unwrap_or_default();
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
        latest_active_job,
        latest_terminal_job,
        latest_job,
        latest_failed_job,
    ))
}

fn vector_health(conn: &rusqlite::Connection) -> Result<VectorHealth, rusqlite::Error> {
    let total: usize = conn.query_row(
        "SELECT COUNT(*) FROM memories WHERE source != ?1",
        [FOUNDRY_RECALL_CACHE_SOURCE],
        |row| row.get::<_, i64>(0).map(|n| n as usize),
    )?;
    let with_vec: usize = conn
        .query_row(
            "SELECT COUNT(DISTINCT v.id)
             FROM memories_vec v
             JOIN memories m ON m.id = v.id
             WHERE m.source != ?1",
            [FOUNDRY_RECALL_CACHE_SOURCE],
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
            "SELECT COUNT(*)
             FROM memories m
             LEFT JOIN memories_vec v ON v.id = m.id
             WHERE m.source != ?1
               AND v.id IS NULL
               AND COALESCE(json_extract(m.metadata, '$.enrichment.status'), '') != 'failed'",
            [FOUNDRY_RECALL_CACHE_SOURCE],
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

fn latest_foundry_job_with_statuses(
    conn: &rusqlite::Connection,
    statuses: &[&str],
) -> Result<Option<LatestFoundryJob>, rusqlite::Error> {
    let placeholders = vec!["?"; statuses.len()].join(",");
    let sql = format!(
        "SELECT id, kind, status, updated_at
         FROM foundry_jobs
         WHERE status IN ({placeholders})
         ORDER BY updated_at DESC, created_at DESC
         LIMIT 1"
    );
    let params = rusqlite::params_from_iter(statuses.iter().copied());
    conn.query_row(&sql, params, |row| {
        Ok(LatestFoundryJob {
            id: row.get(0)?,
            kind: row.get(1)?,
            status: row.get(2)?,
            updated_at: row.get::<_, Option<String>>(3)?,
        })
    })
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

fn paths_equal(a: &Path, b: &Path) -> bool {
    match (std::fs::canonicalize(a), std::fs::canonicalize(b)) {
        (Ok(x), Ok(y)) => x == y,
        _ => a == b,
    }
}

pub(crate) fn resolve_app_home() -> PathBuf {
    std::env::var("TACHI_HOME")
        .ok()
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            dirs::home_dir()
                .unwrap_or_else(|| PathBuf::from("."))
                .join(".tachi")
        })
}

pub(crate) fn runtime_observability_json(
    server: &crate::MemoryServer,
    app_home: &Path,
    daemon: Option<&DaemonStatus>,
) -> serde_json::Value {
    let current_pid = std::process::id();
    let binary = std::env::current_exe()
        .ok()
        .map(|path| path.display().to_string());
    let daemon_pid = match daemon {
        Some(DaemonStatus::Running { pid, .. }) => Some(*pid),
        Some(DaemonStatus::Foreign { pid, .. }) => Some(*pid),
        Some(DaemonStatus::StalePid { pid, .. }) => Some(*pid),
        Some(DaemonStatus::None) => None,
        None => read_pid_file(app_home.join("daemon.lock")),
    };
    let daemon_running = daemon_pid.map(process_alive).unwrap_or(false);
    let serving_daemon = daemon_pid
        .map(|pid| daemon_running && pid as u32 == current_pid)
        .unwrap_or(false);
    let mode = if serving_daemon {
        "daemon"
    } else if daemon_running {
        "sidecar_or_stdio"
    } else {
        "single_process"
    };
    let process_role = if serving_daemon {
        "daemon_authority"
    } else if daemon_running {
        "stdio_daemon_client"
    } else {
        "embedded_stdio"
    };
    let authoritative_runtime = if daemon_running {
        "daemon"
    } else {
        "current_process"
    };
    let stdio_adapter = !serving_daemon;
    let write_forwarding = json!({
        "expected": daemon_running && !serving_daemon,
        "target": if daemon_running && !serving_daemon { "daemon" } else { "current_process" },
        "fallback": if daemon_running && !serving_daemon { "in_process_on_forward_failure" } else { "none" },
    });

    let vault = {
        let state = server.vault_read();
        let unlocked_for_seconds = state.unlock_time.map(|instant| instant.elapsed().as_secs());
        let lockout_remaining_seconds = state.failed_attempts.1.map(|until| {
            until
                .saturating_duration_since(std::time::Instant::now())
                .as_secs()
        });
        json!({
            "unlocked": state.key.is_some() && unlocked_for_seconds
                .map(|elapsed| elapsed <= state.auto_lock_after_secs)
                .unwrap_or(false),
            "unlocked_for_seconds": unlocked_for_seconds,
            "auto_lock_after_seconds": state.auto_lock_after_secs,
            "failed_attempts": state.failed_attempts.0,
            "lockout_remaining_seconds": lockout_remaining_seconds,
        })
    };

    json!({
        "pid": current_pid,
        "binary": binary,
        "mode": mode,
        "process_role": process_role,
        "authoritative_runtime": authoritative_runtime,
        "stdio_adapter": stdio_adapter,
        "write_forwarding": write_forwarding,
        "serving_daemon": serving_daemon,
        "daemon": {
            "pid": daemon_pid,
            "running": daemon_running,
            "matches_current_process": serving_daemon,
        },
        "provider_secret_count": server.llm.provider_secret_count(),
        "provider_pools": server.llm.provider_pool_statuses(),
        "vault": vault,
    })
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

fn collect_dispatches(
    global_db_path: &Path,
    project_db_path: Option<&Path>,
) -> Vec<DispatchStatus> {
    let now = Utc::now();
    let mut out: Vec<(String, DispatchStatus)> = Vec::new();
    let mut seen = std::collections::HashSet::new();
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
            "SELECT id, summary, text, metadata, created_at FROM memories \
             WHERE path LIKE '/kanban/tasks/%' \
               AND source != ?1 \
             ORDER BY created_at DESC LIMIT 10",
        ) {
            Ok(s) => s,
            Err(_) => continue,
        };
        let rows = match stmt.query_map([FOUNDRY_RECALL_CACHE_SOURCE], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, String>(4)?,
            ))
        }) {
            Ok(r) => r,
            Err(_) => continue,
        };
        for row in rows {
            let (id, summary, text, meta_str, created_at) = match row {
                Ok(r) => r,
                Err(_) => continue,
            };
            let meta: serde_json::Value = serde_json::from_str(&meta_str).unwrap_or(json!({}));
            let dispatch_id_full = meta
                .get("dispatch_id")
                .and_then(|v| v.as_str())
                .unwrap_or(&id)
                .to_string();
            if !seen.insert(dispatch_id_full.clone()) {
                continue;
            }
            let agent = meta
                .get("agent")
                .and_then(|v| v.as_str())
                .unwrap_or("unknown")
                .to_string();
            let outcome = normalize_dispatch_outcome(
                meta.get("a2a_state").and_then(|v| v.as_str()),
                &created_at,
                now,
            );
            let reviewed = meta
                .get("reviewed")
                .and_then(|v| v.as_bool())
                .unwrap_or(false);
            let elapsed = created_at
                .parse::<DateTime<Utc>>()
                .ok()
                .map(|dt| status_health::format_elapsed(now - dt))
                .unwrap_or_default();
            let dispatch_id = dispatch_id_full.chars().take(12).collect();
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
            out.push((
                created_at,
                DispatchStatus {
                    dispatch_id,
                    agent,
                    task,
                    outcome,
                    elapsed,
                    reviewed,
                },
            ));
        }
    }
    out.sort_by(|a, b| b.0.cmp(&a.0));
    out.truncate(10);
    out.into_iter().map(|(_, status)| status).collect()
}

fn normalize_dispatch_outcome(
    a2a_state: Option<&str>,
    created_at: &str,
    now: DateTime<Utc>,
) -> String {
    let outcome = a2a_state
        .map(|s| match s {
            "TASK_STATE_WORKING" | "TASK_STATE_IN_PROGRESS" => "in_progress",
            "TASK_STATE_COMPLETED" => "completed",
            "TASK_STATE_FAILED" => "failed",
            "TASK_STATE_CANCELED" => "aborted",
            "TASK_STATE_INPUT_REQUIRED" => "partial",
            other => other,
        })
        .unwrap_or("unknown")
        .to_string();

    if outcome == "in_progress"
        && created_at
            .parse::<DateTime<Utc>>()
            .ok()
            .is_some_and(|dt| now - dt > chrono::Duration::seconds(DISPATCH_STALE_THRESHOLD_SECS))
    {
        return "stale_working".to_string();
    }

    outcome
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
               AND category IN ('eval', 'experience') \
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
    let app_home = resolve_app_home();
    let global_db_path = server.global_db_path_buf();
    let project_db_path = server.project_db_path_buf();

    let snapshot = collect_snapshot(&app_home, &global_db_path, project_db_path.as_deref());

    let daemon_state = match &snapshot.daemon {
        DaemonStatus::Running { pid, .. } => json!({
            "running": true,
            "pid": pid,
        }),
        DaemonStatus::Foreign {
            pid,
            reason,
            version,
            port,
            global_db,
            ..
        } => json!({
            "running": false,
            "foreign": true,
            "pid": pid,
            "reason": reason,
            "version": version,
            "port": port,
            "global_db": global_db,
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
    let total_active: usize = snapshot.dbs.iter().map(|d| d.active_jobs).sum();
    let total_terminal: usize = snapshot.dbs.iter().map(|d| d.terminal_jobs).sum();
    let total_failed: usize = snapshot.dbs.iter().map(|d| d.failed).sum();
    let total_stuck: usize = snapshot.dbs.iter().map(|d| d.stuck_in_progress).sum();
    let worker_queues: Vec<serde_json::Value> = snapshot
        .dbs
        .iter()
        .map(|d| {
            json!({
                "label": d.label,
                "path": d.path,
                "queue_state": if d.active_jobs == 0 && d.failed == 0 && d.stuck_in_progress == 0 {
                    "idle"
                } else if d.stuck_in_progress > 0 {
                    "stuck"
                } else if d.failed > 0 {
                    "failed"
                } else {
                    "active"
                },
                "active_jobs": d.active_jobs,
                "pending_jobs": d.pending,
                "running_jobs": d.running,
                "failed_jobs": d.failed,
                "stuck_jobs": d.stuck_in_progress,
                "terminal_jobs": d.terminal_jobs,
                "completed_jobs": d.completed,
                "skipped_jobs": d.skipped,
                "gc_eligible_jobs": d.gc_eligible,
                "latest_active_job": &d.latest_active_job,
                "latest_terminal_job": &d.latest_terminal_job,
                "backfill": {
                    "needed": d.vector_missing.saturating_sub(d.pending_enrichment) > 0 || vector_dimension_mismatch(d),
                    "pending_enrichment": d.pending_enrichment,
                    "missing_vectors": d.vector_missing,
                    "coverage": d.vector_coverage,
                    "dimension": d.vector_dimension,
                    "expected_dimension": EXPECTED_EMBEDDING_DIM,
                    "command": if d.vector_missing.saturating_sub(d.pending_enrichment) > 0 || vector_dimension_mismatch(d) {
                        Some(status_health::format_backfill_command(d))
                    } else {
                        None
                    },
                }
            })
        })
        .collect();
    let low_coverage: Vec<serde_json::Value> = snapshot
        .dbs
        .iter()
        .filter(|d| d.memory_total > 0 && d.vector_coverage < 0.9)
        .map(|d| {
            json!({
                "label": d.label,
                "coverage": format!("{:.1}%", d.vector_coverage * 100.0),
                "missing": d.vector_missing,
                "pending_enrichment": d.pending_enrichment,
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
                "remediation": "Vector rows exist but use an unexpected dimension; rebuild vectors with the current embedding model instead of treating this as missing-vector backfill.",
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
    let enrichment_failure_dbs: Vec<serde_json::Value> = snapshot
        .dbs
        .iter()
        .filter(|d| d.enrichment_failed_recent > 0)
        .map(|d| {
            json!({
                "label": d.label,
                "path": d.path,
                "enrichment_failed": d.enrichment_failed_recent,
                "failures": d.enrichment_failures,
                "remediation": "Inspect failed enrichment metadata and provider probes; vector backfill will not retry summary/metadata enrichment failures. After fixing provider/schema issues, run `tachi repair --rule R10 --apply --db <label>` to clear stale failed markers.",
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

    let runtime = runtime_observability_json(server, &app_home, Some(&snapshot.daemon));
    let mut warnings = build_status_warnings(&snapshot, &daemon_state);
    if runtime["daemon"]["running"].as_bool().unwrap_or(false)
        && !runtime["daemon"]["matches_current_process"]
            .as_bool()
            .unwrap_or(false)
    {
        warnings.push(
            "this MCP process is a stdio adapter while a daemon is running; supported writes should forward to the daemon, otherwise restart stale MCP clients if runtime state looks inconsistent"
                .to_string(),
        );
    }

    if full {
        serde_json::to_string(&json!({
            "daemon": daemon_state,
            "runtime": runtime,
            "version": env!("CARGO_PKG_VERSION"),
            "health_score": snapshot.health_score,
            "databases": {
                "total": total_dbs,
                "active_jobs": total_active,
                "pending_jobs": total_pending,
                "terminal_jobs": total_terminal,
                "failed_jobs": total_failed,
                "stuck_jobs": total_stuck,
                "worker_queues": worker_queues,
                "low_vector_coverage": low_coverage,
                "vector_dimension_mismatches": vector_dimension_mismatches,
                "vector_orphans": vector_orphan_dbs,
                "enrichment_failures": enrichment_failure_dbs,
                "provider_auth_failures": auth_failures,
                "latest_failed_jobs": failed_jobs,
            },
            "warnings": warnings,
            "daily_pipeline": snapshot.last_daily_report,
            "distill": snapshot.distill_marker,
            "api_keys": snapshot.api_keys,
            "provider_pools": server.llm.provider_pool_statuses(),
            "provider_probe_cache": snapshot.provider_probe_cache,
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
            "runtime": runtime,
            "version": env!("CARGO_PKG_VERSION"),
            "health_score": snapshot.health_score,
            "warnings": warnings.into_iter().take(8).collect::<Vec<_>>(),
            "jobs": {
                "active": total_active,
                "failed": total_failed,
                "pending": total_pending,
                "stuck": total_stuck,
                "terminal_history": total_terminal,
            },
            "distill": distill,
            "vector_coverage_issues": low_coverage.len(),
            "vector_orphans": vector_orphan_dbs.len(),
            "enrichment_failures": enrichment_failure_dbs.len(),
            "provider_auth_failures": auth_failures.len(),
            "api_keys": {
                "drift": api_key_drift,
                "missing_required": api_key_missing,
                "provider_pools": server.llm.provider_pool_statuses(),
            },
            "provider_probe_cache": snapshot.provider_probe_cache,
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
        let app_home = resolve_app_home();
        let snapshot = collect_snapshot(&app_home, &global_db, project_db.as_deref());
        let daemon_state = match &snapshot.daemon {
            DaemonStatus::Running { .. } => json!({ "running": true }),
            DaemonStatus::Foreign { reason, .. } => {
                json!({ "running": false, "foreign": true, "reason": reason })
            }
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
    let low_coverage_dbs: Vec<&str> = snapshot
        .dbs
        .iter()
        .filter(|d| d.memory_total > 0 && d.vector_coverage < 0.9)
        .map(|d| d.label.as_str())
        .collect();
    let low_coverage_count = low_coverage_dbs.len();
    let vector_dimension_mismatch_dbs: Vec<&str> = snapshot
        .dbs
        .iter()
        .filter(|d| vector_dimension_mismatch(d))
        .map(|d| d.label.as_str())
        .collect();
    let vector_dimension_mismatch_count = vector_dimension_mismatch_dbs.len();
    let vector_orphan_dbs: Vec<&str> = snapshot
        .dbs
        .iter()
        .filter(|d| d.vector_orphans > 0)
        .map(|d| d.label.as_str())
        .collect();
    let vector_orphan_count: usize = snapshot.dbs.iter().map(|d| d.vector_orphans).sum();
    let enrichment_failed_dbs: Vec<&str> = snapshot
        .dbs
        .iter()
        .filter(|d| d.enrichment_failed_recent > 0)
        .map(|d| d.label.as_str())
        .collect();
    let enrichment_failed_count: usize = snapshot
        .dbs
        .iter()
        .map(|d| d.enrichment_failed_recent)
        .sum();
    let auth_failure_dbs: Vec<&str> = snapshot
        .dbs
        .iter()
        .filter(|d| {
            d.latest_failed_job
                .as_ref()
                .and_then(|job| job.inferred_invalid_provider.as_ref())
                .is_some()
        })
        .map(|d| d.label.as_str())
        .collect();
    let auth_failures = auth_failure_dbs.len();

    let mut warnings: Vec<String> = Vec::new();
    if !daemon_state["running"].as_bool().unwrap_or(false) {
        warnings.push(
            "daemon not running — background tasks (enrichment, distill, GC) are paused"
                .to_string(),
        );
    }
    if low_coverage_count > 0 {
        warnings.push(format!(
            "{low_coverage_count} db(s) have vector coverage below 90%: {}",
            low_coverage_dbs.join(", ")
        ));
    }
    if vector_dimension_mismatch_count > 0 {
        warnings.push(format!(
            "{vector_dimension_mismatch_count} db(s) have vector dimension metadata that differs from expected {EXPECTED_EMBEDDING_DIM}: {}",
            vector_dimension_mismatch_dbs.join(", ")
        ));
    }
    if vector_orphan_count > 0 {
        warnings.push(format!(
            "{vector_orphan_count} orphan vector row(s) found in {}",
            vector_orphan_dbs.join(", ")
        ));
    }
    if enrichment_failed_count > 0 {
        warnings.push(format!(
            "{enrichment_failed_count} memory enrichment failure(s) remain in {}",
            enrichment_failed_dbs.join(", ")
        ));
    }
    if total_failed > 0 {
        let failed_dbs: Vec<&str> = snapshot
            .dbs
            .iter()
            .filter(|d| d.failed > 0)
            .map(|d| d.label.as_str())
            .collect();
        let db_list = if failed_dbs.is_empty() {
            format!("across {total_dbs} db(s)")
        } else {
            format!("[{}] across {} db(s)", failed_dbs.join(", "), total_dbs)
        };
        warnings.push(format!("{total_failed} foundry job(s) failed {db_list}"));
    }
    if auth_failures > 0 {
        warnings.push(format!(
            "latest foundry failures include provider auth/API-key errors in: {}",
            auth_failure_dbs.join(", ")
        ));
    }
    if let Some(cache) = &snapshot.provider_probe_cache {
        if cache.is_stale() {
            warnings.push(format!(
                "provider key probe cache is older than {}h; run `tachi status --probe-keys` or wait for the daily pipeline",
                cache.ttl_seconds / 3600
            ));
        } else {
            for probe in cache.probes.iter().filter(|probe| probe.status != "ok") {
                let message = probe
                    .message
                    .as_deref()
                    .map(truncate_probe_warning)
                    .unwrap_or_else(|| "no detail".to_string());
                warnings.push(format!(
                    "provider probe {} is {} ({message})",
                    probe.name, probe.status
                ));
            }
            for group in &cache.rotation_groups {
                if group.auth_failed_keys > 0 {
                    let failed = group
                        .keys
                        .iter()
                        .filter(|key| key.status == "auth_failed")
                        .map(|key| key.name.as_str())
                        .collect::<Vec<_>>();
                    warnings.push(format!(
                        "provider rotation group {} has {} auth-failed key(s): {}",
                        group.logical_name,
                        group.auth_failed_keys,
                        failed.join(", ")
                    ));
                }
                if group.rate_limited_keys >= group.configured_keys && group.configured_keys > 0 {
                    warnings.push(format!(
                        "provider rotation group {} has all {} configured key(s) rate-limited",
                        group.logical_name, group.configured_keys
                    ));
                }
            }
        }
    }
    if total_stuck > 0 {
        let stuck_dbs: Vec<&str> = snapshot
            .dbs
            .iter()
            .filter(|d| d.stuck_in_progress > 0)
            .map(|d| d.label.as_str())
            .collect();
        let db_list = if stuck_dbs.is_empty() {
            String::new()
        } else {
            format!(" — affected: {}", stuck_dbs.join(", "))
        };
        warnings.push(format!(
            "{total_stuck} foundry job(s) stuck running for over {STUCK_THRESHOLD_SECS}s{db_list}"
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

fn truncate_probe_warning(message: &str) -> String {
    truncate(message, 120)
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
        let _guard = crate::utils::global_test_lock()
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let saved = std::env::var_os("TACHI_HOME");
        std::env::set_var("TACHI_HOME", "/tmp/status-tachi-home");
        let global = PathBuf::from("/tmp/status/global/memory.db");
        let project = PathBuf::from("/tmp/status/project/memory.db");
        let named = PathBuf::from("/tmp/status-tachi-home/projects/sigil/memory.db");
        let agent = PathBuf::from("/tmp/status-tachi-home/agents/main/memory.db");
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
        if let Some(v) = saved {
            std::env::set_var("TACHI_HOME", v);
        } else {
            std::env::remove_var("TACHI_HOME");
        }
    }

    #[test]
    fn daemon_mismatch_detects_foreign_version() {
        let global = PathBuf::from("/tmp/status/global/memory.db");
        let info = DaemonPidInfo {
            pid: Some(42),
            port: Some(6888),
            version: Some("1.3.0".to_string()),
            global_db: Some(global.display().to_string()),
        };
        let reason = daemon_mismatch_reason(42, Some(&info), &global)
            .expect("foreign version should be reported");
        assert!(reason.contains("1.3.0"));
        assert!(reason.contains(env!("CARGO_PKG_VERSION")));
    }

    #[test]
    fn daemon_mismatch_detects_pid_file_lock_pid_disagreement() {
        let global = PathBuf::from("/tmp/status/global/memory.db");
        let info = DaemonPidInfo {
            pid: Some(7),
            port: Some(6919),
            version: Some(env!("CARGO_PKG_VERSION").to_string()),
            global_db: Some(global.display().to_string()),
        };
        let reason = daemon_mismatch_reason(42, Some(&info), &global)
            .expect("pid disagreement should be reported");
        assert!(reason.contains("pid=7"));
        assert!(reason.contains("lock pid=42"));
    }

    #[test]
    fn daemon_mismatch_accepts_matching_daemon_pid_file() {
        let global = PathBuf::from("/tmp/status/global/memory.db");
        let info = DaemonPidInfo {
            pid: Some(42),
            port: Some(6919),
            version: Some(env!("CARGO_PKG_VERSION").to_string()),
            global_db: Some(global.display().to_string()),
        };
        assert!(daemon_mismatch_reason(42, Some(&info), &global).is_none());
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

    fn vector_health_entry(id: &str, source: &str, vector: Option<Vec<f32>>) -> MemoryEntry {
        MemoryEntry {
            id: id.to_string(),
            path: format!("/scratch/status/{id}"),
            summary: "summary".to_string(),
            text: "status vector health test memory".to_string(),
            importance: 0.7,
            timestamp: Utc::now().to_rfc3339(),
            valid_from: String::new(),
            valid_until: None,
            category: "fact".to_string(),
            topic: "status".to_string(),
            keywords: Vec::new(),
            persons: Vec::new(),
            entities: Vec::new(),
            location: String::new(),
            source: source.to_string(),
            scope: "general".to_string(),
            archived: false,
            access_count: 0,
            last_access: None,
            revision: 1,
            metadata: json!({}),
            vector,
            retention_policy: None,
            domain: None,
            recall_count: 0,
            query_diversity: 0,
            tier: "raw".to_string(),
        }
    }

    #[test]
    fn vector_health_excludes_recall_cache_rows_from_coverage() {
        let dir = tempfile::tempdir().expect("temp db dir");
        let db = dir.path().join("memory.db");
        let mut store = MemoryStore::open(db.to_str().expect("db path")).expect("open store");

        store
            .upsert(&vector_health_entry(
                "normal-with-vector",
                "manual",
                Some(vec![0.1; EXPECTED_EMBEDDING_DIM]),
            ))
            .expect("insert vector row");
        store
            .upsert(&vector_health_entry("normal-missing", "manual", None))
            .expect("insert missing row");
        store
            .upsert(&vector_health_entry(
                "cache-missing",
                FOUNDRY_RECALL_CACHE_SOURCE,
                None,
            ))
            .expect("insert cache row");

        let health = vector_health(store.connection()).expect("vector health");
        assert_eq!(health.total, 2);
        assert_eq!(health.with_vec, 1);
        assert_eq!(health.missing, 1);
        assert_eq!(health.pending_enrichment, 1);
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

    fn db_status(label: &str, failed: usize, stuck: usize, coverage: f64) -> DbStatus {
        DbStatus {
            path: format!("/tmp/{label}.db"),
            label: label.to_string(),
            orphan: false,
            memory_total: 100,
            vector_count: (100.0 * coverage) as usize,
            vector_missing: 0,
            vector_orphans: 0,
            vector_coverage: coverage,
            vector_dimension: Some(EXPECTED_EMBEDDING_DIM),
            pending_enrichment: 0,
            enrichment_failed_recent: 0,
            enrichment_failures: Vec::new(),
            pending: 0,
            running: 0,
            active_jobs: 0,
            completed: 0,
            failed,
            skipped: 0,
            terminal_jobs: failed,
            gc_eligible: 0,
            stuck_in_progress: stuck,
            latest_active_job: None,
            latest_terminal_job: None,
            latest_job: None,
            latest_failed_job: None,
            error: None,
        }
    }

    fn empty_snapshot(dbs: Vec<DbStatus>) -> StatusSnapshot {
        StatusSnapshot {
            daemon: DaemonStatus::None,
            dbs,
            manifest_path: String::new(),
            dispatches: Vec::new(),
            recent_evals: Vec::new(),
            last_daily_report: None,
            distill_marker: None,
            api_keys: Vec::new(),
            provider_probe_cache: None,
            health_score: 95,
        }
    }

    fn daemon_running() -> serde_json::Value {
        serde_json::json!({ "running": true })
    }

    #[test]
    fn normalize_dispatch_outcome_marks_old_working_rows_stale() {
        let now = DateTime::parse_from_rfc3339("2026-06-09T00:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        let fresh = (now - chrono::Duration::minutes(30)).to_rfc3339();
        let stale = (now - chrono::Duration::hours(7)).to_rfc3339();

        assert_eq!(
            normalize_dispatch_outcome(Some("TASK_STATE_WORKING"), &fresh, now),
            "in_progress"
        );
        assert_eq!(
            normalize_dispatch_outcome(Some("TASK_STATE_WORKING"), &stale, now),
            "stale_working"
        );
        assert_eq!(
            normalize_dispatch_outcome(Some("TASK_STATE_COMPLETED"), &stale, now),
            "completed"
        );
    }

    #[test]
    fn collect_dispatches_excludes_recall_cache_rows() {
        let dir = tempfile::tempdir().expect("temp db dir");
        let db = dir.path().join("memory.db");
        let store = MemoryStore::open(db.to_str().expect("db path")).expect("open store");
        let now = Utc::now().to_rfc3339();
        store
            .connection()
            .execute(
                "INSERT INTO memories
                 (id, path, summary, text, importance, timestamp, category, topic, keywords, entities, source, scope, archived, created_at, updated_at, access_count, revision, metadata)
                 VALUES (?1, ?2, ?3, ?4, 0.8, ?5, 'kanban', 'kanban', '[]', '[]', ?6, 'general', 0, ?5, ?5, 0, 1, ?7)",
                rusqlite::params![
                    "real-dispatch",
                    "/kanban/tasks/20260609T000000Z-codex",
                    "Kanban: real task",
                    "Dispatch Task\nTask: real worker task",
                    now,
                    "manual",
                    json!({
                        "dispatch_id": "20260609T000000Z-codex",
                        "agent": "codex",
                        "task": "real worker task",
                        "a2a_state": "TASK_STATE_COMPLETED",
                        "reviewed": true,
                    })
                    .to_string(),
                ],
            )
            .expect("insert real dispatch");
        store
            .connection()
            .execute(
                "INSERT INTO memories
                 (id, path, summary, text, importance, timestamp, category, topic, keywords, entities, source, scope, archived, created_at, updated_at, access_count, revision, metadata)
                 VALUES (?1, ?2, ?3, ?4, 0.8, ?5, 'kanban', 'kanban', '[]', '[]', ?6, 'general', 0, ?5, ?5, 0, 1, '{}')",
                rusqlite::params![
                    "foundry:recall-cache:noise",
                    "/kanban/tasks/recall-cache/review_tachi_mcp_facade",
                    "Recall rerank cache for query: review tachi mcp facade",
                    "Recall rerank cache for query: review tachi mcp facade",
                    Utc::now().to_rfc3339(),
                    FOUNDRY_RECALL_CACHE_SOURCE,
                ],
            )
            .expect("insert recall cache row");

        let dispatches = collect_dispatches(&db, None);
        assert_eq!(dispatches.len(), 1, "got: {dispatches:?}");
        assert_eq!(dispatches[0].dispatch_id, "20260609T000");
        assert_eq!(dispatches[0].agent, "codex");
        assert_eq!(dispatches[0].task, "real worker task");
    }

    #[test]
    fn collect_dispatches_includes_project_db_rows() {
        let dir = tempfile::tempdir().expect("temp db dir");
        let global_db = dir.path().join("global.db");
        let project_db = dir.path().join("project.db");
        MemoryStore::open(global_db.to_str().expect("global path")).expect("open global");
        let project =
            MemoryStore::open(project_db.to_str().expect("project path")).expect("open project");
        let now = Utc::now().to_rfc3339();
        project
            .connection()
            .execute(
                "INSERT INTO memories
                 (id, path, summary, text, importance, timestamp, category, topic, keywords, entities, source, scope, archived, created_at, updated_at, access_count, revision, metadata)
                 VALUES (?1, ?2, ?3, ?4, 0.8, ?5, 'kanban', 'kanban', '[]', '[]', 'manual', 'project', 0, ?5, ?5, 0, 1, ?6)",
                rusqlite::params![
                    "project-dispatch",
                    "/kanban/tasks/20260609T000001Z-codex",
                    "Kanban: project task",
                    "Dispatch Task\nTask: project worker task",
                    now,
                    json!({
                        "dispatch_id": "20260609T000001Z-codex",
                        "agent": "codex",
                        "task": "project worker task",
                        "a2a_state": "TASK_STATE_COMPLETED",
                        "reviewed": true,
                    })
                    .to_string(),
                ],
            )
            .expect("insert project dispatch");

        let dispatches = collect_dispatches(&global_db, Some(&project_db));
        assert_eq!(dispatches.len(), 1, "got: {dispatches:?}");
        assert_eq!(dispatches[0].dispatch_id, "20260609T000");
        assert_eq!(dispatches[0].task, "project worker task");
    }

    #[test]
    fn collect_recent_evals_reads_eval_category_rows() {
        let dir = tempfile::tempdir().expect("temp db dir");
        let global_db = dir.path().join("global.db");
        let project_db = dir.path().join("project.db");
        MemoryStore::open(global_db.to_str().expect("global path")).expect("open global");
        let project =
            MemoryStore::open(project_db.to_str().expect("project path")).expect("open project");
        let now = Utc::now().to_rfc3339();
        project
            .connection()
            .execute(
                "INSERT INTO memories
                 (id, path, summary, text, importance, timestamp, category, topic, keywords, entities, source, scope, archived, created_at, updated_at, access_count, revision, metadata)
                 VALUES (?1, ?2, ?3, ?4, 0.8, ?5, 'eval', 'eval', '[]', '[]', 'manual', 'project', 0, ?5, ?5, 0, 1, ?6)",
                rusqlite::params![
                    "eval-row",
                    "/eval/2026-06-09/20260609T000002Z-codex",
                    "[✓] codex / UX smoke",
                    "eval text",
                    now,
                    json!({
                        "agent": "codex",
                        "outcome": "success",
                        "quality_score": 0.82,
                    })
                    .to_string(),
                ],
            )
            .expect("insert eval");

        let evals = collect_recent_evals(&global_db, Some(&project_db));
        assert_eq!(evals.len(), 1, "got: {evals:?}");
        assert_eq!(evals[0].agent, "codex");
        assert_eq!(evals[0].outcome, "success");
        assert_eq!(evals[0].quality_score, Some(0.82));
    }

    #[test]
    fn build_status_warnings_lists_db_names_for_low_coverage() {
        let snapshot = empty_snapshot(vec![
            db_status("global", 0, 0, 0.95),
            db_status("sigil", 0, 0, 0.42),
            db_status("hyperion", 0, 0, 0.81),
        ]);
        let warnings = build_status_warnings(&snapshot, &daemon_running());
        let low_cov = warnings
            .iter()
            .find(|w| w.contains("vector coverage below 90%"))
            .expect("low-coverage warning present");
        assert!(
            low_cov.contains("sigil") && low_cov.contains("hyperion"),
            "low-coverage warning should name the affected dbs, got: {low_cov}"
        );
        assert!(
            !low_cov.contains("global"),
            "healthy dbs must not appear in low-coverage warning, got: {low_cov}"
        );
    }

    #[test]
    fn build_status_warnings_lists_db_names_for_failed_jobs() {
        let mut sigil = db_status("sigil", 3, 0, 0.95);
        sigil.latest_failed_job = Some(LatestFailedJob {
            id: "job-1".to_string(),
            kind: "enrich".to_string(),
            lane: None,
            updated_at: None,
            reason: Some("401 invalid api key".to_string()),
            inferred_invalid_provider: Some("openai".to_string()),
        });
        let snapshot = empty_snapshot(vec![db_status("global", 0, 0, 0.95), sigil]);
        let warnings = build_status_warnings(&snapshot, &daemon_running());
        let failed = warnings
            .iter()
            .find(|w| w.contains("foundry job(s) failed"))
            .expect("failed-jobs warning present");
        assert!(
            failed.contains("sigil"),
            "failed-jobs warning should name the affected dbs, got: {failed}"
        );
        let auth = warnings
            .iter()
            .find(|w| w.contains("auth/API-key errors"))
            .expect("auth-failure warning present");
        assert!(
            auth.contains("sigil"),
            "auth-failure warning should name the affected dbs, got: {auth}"
        );
    }

    #[test]
    fn build_status_warnings_lists_db_names_for_stuck_jobs() {
        let snapshot = empty_snapshot(vec![
            db_status("global", 0, 0, 0.95),
            db_status("sigil", 0, 2, 0.95),
        ]);
        let warnings = build_status_warnings(&snapshot, &daemon_running());
        let stuck = warnings
            .iter()
            .find(|w| w.contains("stuck running"))
            .expect("stuck-jobs warning present");
        assert!(
            stuck.contains("sigil"),
            "stuck-jobs warning should name the affected dbs, got: {stuck}"
        );
    }

    #[test]
    fn build_status_warnings_lists_enrichment_failures_and_vector_orphans() {
        let mut hyperion = db_status("hyperion", 0, 0, 1.0);
        hyperion.enrichment_failed_recent = 42;
        let mut sigil = db_status("sigil", 0, 0, 1.0);
        sigil.vector_orphans = 2;
        let snapshot = empty_snapshot(vec![db_status("global", 0, 0, 1.0), hyperion, sigil]);

        let warnings = build_status_warnings(&snapshot, &daemon_running());
        let enrichment = warnings
            .iter()
            .find(|w| w.contains("memory enrichment failure"))
            .expect("enrichment-failure warning present");
        assert!(
            enrichment.contains("hyperion") && enrichment.contains("42"),
            "enrichment warning should name affected db and count, got: {enrichment}"
        );
        let orphans = warnings
            .iter()
            .find(|w| w.contains("orphan vector row"))
            .expect("vector-orphan warning present");
        assert!(
            orphans.contains("sigil") && orphans.contains("2"),
            "vector orphan warning should name affected db and count, got: {orphans}"
        );
    }

    #[test]
    fn health_score_drops_for_background_enrichment_failures_and_orphans() {
        let mut hyperion = db_status("hyperion", 0, 0, 1.0);
        hyperion.enrichment_failed_recent = 42;
        let mut sigil = db_status("sigil", 0, 0, 1.0);
        sigil.vector_orphans = 1;
        let dbs = vec![hyperion, sigil];
        let score = status_health::calculate_health_score(
            &DaemonStatus::Running {
                pid: 1,
                lock_path: PathBuf::from("/tmp/tachi.lock"),
            },
            &dbs,
            Some(&DistillMarkerStatus {
                path: "/tmp/marker".to_string(),
                last_run_at: "2026-06-09T00:00:00Z".to_string(),
                age_seconds: 0,
                age: "0s ago".to_string(),
                is_stale: false,
            }),
            &[],
            Some(&[]),
            Some(&[]),
        );

        assert!(
            score < 100,
            "background failures/orphans must prevent perfect health score"
        );
    }
}
