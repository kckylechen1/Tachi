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

use std::path::{Path, PathBuf};
use std::time::Duration;

use chrono::{DateTime, Utc};
use serde_json::json;

use memory_core::{
    get_foundry_config, job_status_histogram, set_foundry_config, JobStatusHistogram, MemoryStore,
    PerDbConfig,
};

use crate::cli::{DaemonAction, FoundryAction};
use crate::daemon_lock::{process_alive, read_pid_file};
use crate::manifest::{DbRole, Manifest};

/// `in_progress` jobs older than this are flagged as stuck in `tachi status`.
/// Matches the safety-net poll cadence in [`crate::foundry_scheduler`] with
/// generous headroom so transient long-running jobs don't trigger noise.
const STUCK_THRESHOLD_SECS: i64 = 600;

/// Refresh interval for `tachi status --watch`.
const WATCH_INTERVAL: Duration = Duration::from_secs(2);

/// `tachi status` entrypoint.
pub(crate) async fn run_status(
    watch: bool,
    json_out: bool,
    hide_orphans: bool,
    app_home: &Path,
    global_db_path: &Path,
    project_db_path: Option<&Path>,
) -> Result<(), Box<dyn std::error::Error>> {
    if !watch {
        return render_one(
            json_out,
            hide_orphans,
            app_home,
            global_db_path,
            project_db_path,
        );
    }

    if json_out {
        // --watch + --json doesn't make sense (JSON consumers don't want a
        // screen-clearing infinite stream). Treat as one-shot.
        return render_one(
            true,
            hide_orphans,
            app_home,
            global_db_path,
            project_db_path,
        );
    }

    loop {
        // ANSI clear + cursor home so each frame replaces the previous.
        print!("\x1b[2J\x1b[H");
        if let Err(e) = render_one(
            false,
            hide_orphans,
            app_home,
            global_db_path,
            project_db_path,
        ) {
            eprintln!("[!] status render failed: {e}");
        }
        tokio::time::sleep(WATCH_INTERVAL).await;
    }
}

fn render_one(
    json_out: bool,
    hide_orphans: bool,
    app_home: &Path,
    global_db_path: &Path,
    project_db_path: Option<&Path>,
) -> Result<(), Box<dyn std::error::Error>> {
    let snapshot = collect_snapshot(app_home, global_db_path, project_db_path);

    if json_out {
        // JSON consumers always see the full snapshot incl. orphans, so
        // dashboards/scripts retain visibility regardless of how the
        // operator filters their human-readable view.
        let v = serde_json::to_value(&snapshot)?;
        println!("{}", serde_json::to_string_pretty(&v)?);
        return Ok(());
    }

    println!("tachi status @ {}", Utc::now().to_rfc3339());
    println!("  app_home: {}", app_home.display());
    println!();

    // Daemon section
    println!("Daemon");
    match &snapshot.daemon {
        DaemonStatus::Running { pid, lock_path } => {
            println!("  [OK] running pid={pid} lock={}", lock_path.display());
        }
        DaemonStatus::StalePid { pid, lock_path } => {
            println!(
                "  [!] stale pid file pid={pid} (process not alive); lock at {}",
                lock_path.display()
            );
        }
        DaemonStatus::None => {
            println!("  [OK] no daemon running (single-process mode)");
        }
    }
    println!();

    // Manifest section.
    //
    // B6: when --hide-orphans is passed, suppress per-row noise from DBs
    // the daemon's scheduler has no route to (typically agent-owned DBs
    // from extensions whose hub plugin is not installed). Orphans still
    // appear in --json output and in the Summary count below so the total
    // is never silently understated.
    let visible_dbs: Vec<&DbStatus> = snapshot
        .dbs
        .iter()
        .filter(|db| !(hide_orphans && db.orphan))
        .collect();
    let hidden_orphans = snapshot.dbs.len() - visible_dbs.len();

    println!("Manifest ({} dbs)", snapshot.dbs.len());
    if snapshot.dbs.is_empty() {
        println!("  [!] manifest empty or missing — run `tachi doctor` to populate it");
    }
    if hidden_orphans > 0 {
        println!(
            "  [i] {hidden_orphans} orphan db{plural} hidden by --hide-orphans (still counted in Summary)",
            plural = if hidden_orphans == 1 { "" } else { "s" },
        );
    }
    for db in &visible_dbs {
        let stuck_marker = if db.stuck_in_progress > 0 {
            format!(" [!] {} stuck in_progress", db.stuck_in_progress)
        } else {
            String::new()
        };
        // "orphan" = manifest entry exists but the running daemon's
        // scheduler has no route that maps writes to that DB. It's
        // informational, not an error — extensions register manifest
        // rows whose hub plugin may be inactive on this host.
        let orphan_marker = if db.orphan {
            " [i] orphan (no scheduler route on this host — informational, not an error)"
        } else {
            ""
        };
        println!(
            "  [OK] {label:<20} pending={pending:<4} running={running:<3} completed={completed:<5} failed={failed:<3} gc_eligible={gc:<4}{orphan}{stuck}",
            label = truncate(&db.label, 20),
            pending = db.pending,
            running = db.running,
            completed = db.completed,
            failed = db.failed,
            gc = db.gc_eligible,
            orphan = orphan_marker,
            stuck = stuck_marker,
        );
        if db.memory_total > 0 {
            let pct = db.vector_coverage * 100.0;
            let marker = if db.vector_missing > 0 { "[!]" } else { "[OK]" };
            let failures = if db.enrichment_failed_recent > 0 {
                format!(" enrichment_failed={}", db.enrichment_failed_recent)
            } else {
                String::new()
            };
            println!(
                "       {marker} vectors={}/{} missing={} coverage={pct:.1}%{}",
                db.vector_count, db.memory_total, db.vector_missing, failures
            );
        }
        if let Some(err) = &db.error {
            println!("       [X] {err}");
        }
    }
    println!();

    // Dispatches section
    println!("Dispatches (recent)");
    if snapshot.dispatches.is_empty() {
        println!("  (none)");
    } else {
        for d in &snapshot.dispatches {
            let icon = match d.outcome.as_str() {
                "completed" | "success" if d.reviewed => "[OK]",
                "completed" | "success" => "[!] ",
                "in_progress" => "[..]",
                _ => "[X] ",
            };
            let review_tag = if !d.reviewed && matches!(d.outcome.as_str(), "completed" | "success")
            {
                " unreviewed"
            } else {
                ""
            };
            println!(
                "  {icon} {id:<12} {agent:<14} \"{task}\"  {outcome} {elapsed}{review}",
                id = truncate(&d.dispatch_id, 12),
                agent = truncate(&d.agent, 14),
                task = truncate(&d.task, 40),
                outcome = d.outcome,
                elapsed = d.elapsed,
                review = review_tag,
            );
        }
    }
    println!();

    // Recent evals section
    println!("Recent Evals");
    if snapshot.recent_evals.is_empty() {
        println!("  (none)");
    } else {
        for e in &snapshot.recent_evals {
            let icon = match e.outcome.as_str() {
                "success" => "[OK]",
                "partial" => "[~] ",
                _ => "[X] ",
            };
            let quality = e
                .quality_score
                .map(|q| format!("  quality={q:.2}"))
                .unwrap_or_default();
            println!(
                "  {icon} {id:<24} {agent:<14} {outcome}{quality}",
                id = truncate(&e.task_id, 24),
                agent = truncate(&e.agent, 14),
                outcome = e.outcome,
            );
        }
    }
    println!();

    // Daily pipeline section
    println!("Daily Pipeline");
    match &snapshot.last_daily_report {
        Some(p) => println!("  [OK] last report: {p}"),
        None => println!("  (no daily reports yet)"),
    }
    println!();

    let total_pending: usize = snapshot.dbs.iter().map(|d| d.pending).sum();
    let total_orphan = snapshot.dbs.iter().filter(|d| d.orphan).count();
    let total_stuck: usize = snapshot.dbs.iter().map(|d| d.stuck_in_progress).sum();
    println!(
        "Summary: {n} dbs, {pending} total pending, {orphan} orphan (informational), {stuck} stuck in_progress",
        n = snapshot.dbs.len(),
        pending = total_pending,
        orphan = total_orphan,
        stuck = total_stuck
    );

    Ok(())
}

/// MCP tool handler: returns a concise JSON health summary for agents.
pub(crate) async fn handle_tachi_status(server: &crate::MemoryServer) -> Result<String, String> {
    let app_home: PathBuf = std::env::var("TACHI_HOME")
        .ok()
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            dirs::home_dir().unwrap_or_else(|| PathBuf::from(".")).join(".tachi")
        });
    let global_db_path = server.global_db_path_buf();
    let project_db_path = server.project_db_path_buf();

    let snapshot =
        collect_snapshot(&app_home, &global_db_path, project_db_path.as_deref());

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
    let low_coverage: Vec<serde_json::Value> = snapshot
        .dbs
        .iter()
        .filter(|d| d.vector_coverage < 0.9)
        .map(|d| {
            json!({
                "label": d.label,
                "coverage": format!("{:.1}%", d.vector_coverage * 100.0),
                "missing": d.vector_missing,
            })
        })
        .collect();

    let mut warnings: Vec<String> = Vec::new();
    if !daemon_state["running"].as_bool().unwrap_or(false) {
        warnings.push("daemon not running — background tasks (enrichment, distill, GC) are paused".to_string());
    }
    if !low_coverage.is_empty() {
        warnings.push(format!(
            "{} db(s) have vector coverage below 90%",
            low_coverage.len()
        ));
    }
    if total_failed > 0 {
        warnings.push(format!("{total_failed} foundry job(s) failed across {total_dbs} dbs"));
    }

    serde_json::to_string(&json!({
        "daemon": daemon_state,
        "version": env!("CARGO_PKG_VERSION"),
        "databases": {
            "total": total_dbs,
            "pending_jobs": total_pending,
            "failed_jobs": total_failed,
            "low_vector_coverage": low_coverage,
        },
        "warnings": warnings,
        "daily_pipeline": snapshot.last_daily_report,
    }))
    .map_err(|e| e.to_string())
}

#[derive(Debug, serde::Serialize)]
struct StatusSnapshot {
    daemon: DaemonStatus,
    dbs: Vec<DbStatus>,
    manifest_path: String,
    dispatches: Vec<DispatchStatus>,
    recent_evals: Vec<RecentEval>,
    last_daily_report: Option<String>,
}

#[derive(Debug, serde::Serialize)]
struct DispatchStatus {
    dispatch_id: String,
    agent: String,
    task: String,
    outcome: String,
    elapsed: String,
    reviewed: bool,
}

#[derive(Debug, serde::Serialize)]
struct RecentEval {
    task_id: String,
    agent: String,
    outcome: String,
    quality_score: Option<f64>,
    timestamp: String,
}

#[derive(Debug, serde::Serialize)]
#[serde(tag = "state", rename_all = "snake_case")]
enum DaemonStatus {
    Running { pid: i32, lock_path: PathBuf },
    StalePid { pid: i32, lock_path: PathBuf },
    None,
}

#[derive(Debug, serde::Serialize)]
struct DbStatus {
    path: String,
    label: String,
    /// True if the manifest entry is not under any scope the running
    /// daemon's scheduler can route to (i.e. agents/, hub/, vault/, etc.).
    orphan: bool,
    memory_total: usize,
    vector_count: usize,
    vector_missing: usize,
    vector_coverage: f64,
    enrichment_failed_recent: usize,
    pending: usize,
    running: usize,
    completed: usize,
    failed: usize,
    gc_eligible: usize,
    /// Number of `in_progress` jobs older than [`STUCK_THRESHOLD_SECS`].
    stuck_in_progress: usize,
    error: Option<String>,
}

fn collect_snapshot(
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
                vector_coverage: 0.0,
                enrichment_failed_recent: 0,
                pending: 0,
                running: 0,
                completed: 0,
                failed: 0,
                gc_eligible: 0,
                stuck_in_progress: 0,
                error: Some("missing on disk".to_string()),
            });
            continue;
        }
        match probe_db(&path) {
            Ok((hist, stuck, vector)) => dbs.push(DbStatus {
                path: entry.path.clone(),
                label,
                orphan,
                memory_total: vector.total,
                vector_count: vector.with_vec,
                vector_missing: vector.missing,
                vector_coverage: vector.coverage,
                enrichment_failed_recent: vector.enrichment_failed_recent,
                pending: hist.queued + hist.planned,
                running: hist.running,
                completed: hist.completed,
                failed: hist.failed,
                gc_eligible: hist.gc_eligible,
                stuck_in_progress: stuck,
                error: None,
            }),
            Err(e) => dbs.push(DbStatus {
                path: entry.path.clone(),
                label,
                orphan,
                memory_total: 0,
                vector_count: 0,
                vector_missing: 0,
                vector_coverage: 0.0,
                enrichment_failed_recent: 0,
                pending: 0,
                running: 0,
                completed: 0,
                failed: 0,
                gc_eligible: 0,
                stuck_in_progress: 0,
                error: Some(e),
            }),
        }
    }

    let dispatches = collect_dispatches(global_db_path);
    let recent_evals = collect_recent_evals(global_db_path, project_db_path);
    let last_daily_report = find_last_daily_report(app_home);

    StatusSnapshot {
        daemon,
        dbs,
        manifest_path: manifest_path.display().to_string(),
        dispatches,
        recent_evals,
        last_daily_report,
    }
}

#[derive(Debug, Default)]
struct VectorHealth {
    total: usize,
    with_vec: usize,
    missing: usize,
    coverage: f64,
    enrichment_failed_recent: usize,
}

fn probe_db(path: &Path) -> Result<(JobStatusHistogram, usize, VectorHealth), String> {
    let path_str = path
        .to_str()
        .ok_or_else(|| format!("non-utf8 path: {}", path.display()))?;
    // Diagnostics must NOT take write locks or run schema migrations on
    // potentially read-only DBs. `open_with_label` initializes/migrates
    // the schema on every open; `open_read_only` skips that work.
    let store = MemoryStore::open_read_only(path_str).map_err(|e| format!("open: {e}"))?;
    let conn = store.connection();
    let hist = job_status_histogram(conn, 30).map_err(|e| format!("histogram: {e}"))?;
    let stuck = count_stuck_in_progress(conn).unwrap_or(0);
    let vector = vector_health(conn).unwrap_or_default();
    Ok((hist, stuck, vector))
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
    Ok(VectorHealth {
        total,
        with_vec,
        missing,
        coverage,
        enrichment_failed_recent,
    })
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
        // The `updated_at` column or `in_progress` status may not exist
        // on every DB schema version; treat that as "no data" rather than
        // a hard error so `tachi status` works against legacy files.
        rusqlite::Error::SqliteFailure(_, _) => Ok(0),
        other => Err(other),
    })
}

/// Mirrors the scheduler's current reduced-scope routing: own global DB,
/// own project DB, and canonical `~/.tachi/projects/<name>/memory.db` are
/// routable; everything else is an orphan until DbScope::Path exists.
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
    if grand_name == "projects" && db_path.file_name()?.to_str()? == "memory.db" {
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

fn truncate(s: &str, max: usize) -> String {
    if s.len() <= max {
        s.to_string()
    } else {
        format!("{}…", &s[..max.saturating_sub(1)])
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
            .map(|dt| format_elapsed(now - dt))
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

fn format_elapsed(dur: chrono::Duration) -> String {
    let secs = dur.num_seconds().max(0);
    if secs < 60 {
        format!("{secs}s ago")
    } else if secs < 3600 {
        format!("{}m ago", secs / 60)
    } else if secs < 86400 {
        format!("{}h ago", secs / 3600)
    } else {
        format!("{}d ago", secs / 86400)
    }
}

// ─── tachi daemon ────────────────────────────────────────────────────────────

pub(crate) async fn run_daemon(
    action: DaemonAction,
    app_home: &Path,
) -> Result<(), Box<dyn std::error::Error>> {
    let lock_path = app_home.join("daemon.lock");
    match action {
        DaemonAction::Status { json: json_out } => {
            let pid = read_pid_file(&lock_path);
            let alive = pid.map(process_alive).unwrap_or(false);
            if json_out {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&json!({
                        "lock_path": lock_path.display().to_string(),
                        "pid": pid,
                        "alive": alive,
                    }))?
                );
            } else {
                match (pid, alive) {
                    (Some(p), true) => {
                        println!("[OK] daemon running pid={p} lock={}", lock_path.display())
                    }
                    (Some(p), false) => println!(
                        "[!] stale pid file pid={p} at {} (process not alive)",
                        lock_path.display()
                    ),
                    (None, _) => println!("[OK] no daemon running"),
                }
            }
            Ok(())
        }
        DaemonAction::Kill { force } => {
            let pid = match read_pid_file(&lock_path) {
                Some(p) => p,
                None => {
                    println!(
                        "[OK] no daemon to kill (no lock file at {})",
                        lock_path.display()
                    );
                    return Ok(());
                }
            };
            let alive = process_alive(pid);
            if !alive {
                if force {
                    let _ = std::fs::remove_file(&lock_path);
                    println!(
                        "[OK] removed stale lock {} (pid {pid} was not alive)",
                        lock_path.display()
                    );
                } else {
                    println!(
                        "[!] pid {pid} in {} is not alive; rerun with --force to unlink the stale lock",
                        lock_path.display()
                    );
                }
                return Ok(());
            }
            // Live process: send SIGTERM. The daemon's tokio::signal handler
            // performs graceful shutdown, which Drop-releases the flock and
            // unlinks the lock file.
            #[cfg(unix)]
            {
                let r = unsafe { libc::kill(pid as libc::pid_t, libc::SIGTERM) };
                if r == 0 {
                    println!("[OK] sent SIGTERM to daemon pid={pid}");
                } else {
                    let err = std::io::Error::last_os_error();
                    return Err(format!("kill({pid}) failed: {err}").into());
                }
            }
            #[cfg(not(unix))]
            {
                return Err("daemon kill is only implemented on unix".into());
            }
            Ok(())
        }
    }
}

// ─── tachi foundry config ────────────────────────────────────────────────────

pub(crate) async fn run_foundry(
    action: FoundryAction,
    app_home: &Path,
    global_db_path: &Path,
) -> Result<(), Box<dyn std::error::Error>> {
    match action {
        FoundryAction::ConfigGet { db } => {
            let target = db.unwrap_or_else(|| global_db_path.to_path_buf());
            let cfg = read_per_db_config(&target)?;
            println!(
                "{}",
                serde_json::to_string_pretty(&json!({
                    "db": target.display().to_string(),
                    "config": cfg,
                }))?
            );
            Ok(())
        }
        FoundryAction::ConfigSet {
            db,
            enabled,
            max_jobs_per_minute,
            distill_concurrency,
            enrichment_concurrency,
            llm_provider_override,
        } => {
            let target = db.unwrap_or_else(|| global_db_path.to_path_buf());
            let path_str = target
                .to_str()
                .ok_or_else(|| format!("non-utf8 path: {}", target.display()))?;
            let store = MemoryStore::open_with_label(path_str, "tachi-foundry-config")
                .map_err(|e| format!("open {}: {e}", target.display()))?;
            let mut cfg = get_foundry_config(store.connection())
                .map_err(|e| format!("get_foundry_config: {e}"))?;
            if let Some(v) = enabled {
                cfg.enabled = v;
            }
            if let Some(v) = max_jobs_per_minute {
                cfg.max_jobs_per_minute = v;
            }
            if let Some(v) = distill_concurrency {
                cfg.distill_concurrency = v;
            }
            if let Some(v) = enrichment_concurrency {
                cfg.enrichment_concurrency = v;
            }
            if let Some(v) = llm_provider_override {
                cfg.llm_provider_override = if v.is_empty() { None } else { Some(v) };
            }
            set_foundry_config(store.connection(), &cfg, "tachi-cli")
                .map_err(|e| format!("set_foundry_config: {e}"))?;
            println!(
                "{}",
                serde_json::to_string_pretty(&json!({
                    "db": target.display().to_string(),
                    "config": cfg,
                    "updated": true,
                }))?
            );
            Ok(())
        }
        FoundryAction::ConfigList { json: json_out } => {
            let manifest_path = app_home.join("manifest.json");
            let manifest = Manifest::load(&manifest_path).unwrap_or_else(|_| Manifest::empty());
            let mut entries: Vec<serde_json::Value> = Vec::new();
            for entry in &manifest.dbs {
                // B4: skip checkpoint-copy fixtures (`*.db.checkpointed.<ts>.db`).
                // These are produced by the daemon's WAL-checkpoint copy-aside
                // dance during integration tests; they're never user-meaningful
                // foundry targets but kept getting picked up by manifest scans
                // because they live next to real DBs in `tmp/`.
                if is_checkpoint_fixture_path(&entry.path) {
                    continue;
                }
                let p = PathBuf::from(&entry.path);
                if !p.exists() {
                    entries.push(json!({
                        "db": entry.path,
                        "label": entry.scope_hint,
                        "config": null,
                        "error": "missing on disk",
                    }));
                    continue;
                }
                match read_per_db_config(&p) {
                    Ok(cfg) => entries.push(json!({
                        "db": entry.path,
                        "label": entry.scope_hint,
                        "config": cfg,
                    })),
                    Err(e) => entries.push(json!({
                        "db": entry.path,
                        "label": entry.scope_hint,
                        "config": null,
                        "error": e.to_string(),
                    })),
                }
            }
            if json_out {
                println!("{}", serde_json::to_string_pretty(&entries)?);
            } else {
                for e in &entries {
                    let db = e.get("db").and_then(|v| v.as_str()).unwrap_or("?");
                    let label = e.get("label").and_then(|v| v.as_str()).unwrap_or("?");
                    if let Some(err) = e.get("error").and_then(|v| v.as_str()) {
                        // B4: route failure rows to stderr so a user piping
                        // the clean `[OK]` list (`tachi foundry config-list
                        // 2>/dev/null | awk ...`) doesn't get a poisoned
                        // table. Behavior of `--json` is unchanged: machine
                        // consumers always get the full structured list on
                        // stdout including the `error` field.
                        eprintln!("[X] {label:<20} {db}  ({err})");
                    } else {
                        let cfg = e.get("config").cloned().unwrap_or(serde_json::Value::Null);
                        let enabled = cfg.get("enabled").and_then(|v| v.as_bool()).unwrap_or(true);
                        let max_jpm = cfg
                            .get("max_jobs_per_minute")
                            .and_then(|v| v.as_u64())
                            .unwrap_or(0);
                        let dc = cfg
                            .get("distill_concurrency")
                            .and_then(|v| v.as_u64())
                            .unwrap_or(0);
                        let ec = cfg
                            .get("enrichment_concurrency")
                            .and_then(|v| v.as_u64())
                            .unwrap_or(0);
                        println!(
                            "[OK] {label:<20} enabled={enabled} max_jpm={max_jpm:<3} distill={dc} enrich={ec}  {db}"
                        );
                    }
                }
            }
            Ok(())
        }
    }
}

/// B4: a path is a checkpoint-copy fixture if its filename matches the
/// daemon's internal WAL copy-aside naming scheme:
/// `<base>.db.checkpointed.<RFC3339-ish-stamp>.db`. These are throw-away
/// byproducts of integration tests / WAL truncation rescues; they should
/// never appear in user-facing manifest listings.
///
/// Detection is purely lexical (no stat / no I/O) so it stays cheap inside
/// the manifest-iteration loop. We require BOTH the `.checkpointed.` infix
/// AND a final `.db` extension so we don't accidentally suppress real DBs
/// that happen to have the substring elsewhere in their absolute path
/// (e.g. an unfortunate parent directory name).
fn is_checkpoint_fixture_path(path: &str) -> bool {
    let lower = path.to_ascii_lowercase();
    let file = lower
        .rsplit(std::path::MAIN_SEPARATOR)
        .next()
        .unwrap_or(&lower);
    file.contains(".checkpointed.") && file.ends_with(".db")
}

fn read_per_db_config(path: &Path) -> Result<PerDbConfig, Box<dyn std::error::Error>> {
    let path_str = path
        .to_str()
        .ok_or_else(|| format!("non-utf8 path: {}", path.display()))?;
    let store = MemoryStore::open_with_label(path_str, "tachi-foundry-config")
        .map_err(|e| format!("open {}: {e}", path.display()))?;
    let cfg =
        get_foundry_config(store.connection()).map_err(|e| format!("get_foundry_config: {e}"))?;
    Ok(cfg)
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
        assert_eq!(truncate("abcdefghij", 5), "abcd…");
    }

    /// B4: `tachi foundry config-list` must skip checkpoint-copy fixtures
    /// (`*.db.checkpointed.<stamp>.db`) but keep real DBs. Pure-lexical
    /// classifier so the manifest loop stays I/O-free.
    #[test]
    fn checkpoint_fixture_classifier_recognizes_only_real_fixtures() {
        let sep = std::path::MAIN_SEPARATOR;
        let cases = [
            // Real production DBs — must NOT be suppressed.
            (
                format!("{sep}Users{sep}u{sep}.tachi{sep}global{sep}memory.db"),
                false,
            ),
            (
                format!("{sep}home{sep}u{sep}.openclaw{sep}agents{sep}main{sep}memory.db"),
                false,
            ),
            // Parent directory contains the substring but the file does not.
            (
                format!("{sep}srv{sep}checkpointed{sep}prod{sep}memory.db"),
                false,
            ),
            // Non-.db artifacts the daemon may leave behind — we only filter
            // the canonical `.db` copy fixture.
            (
                format!(
                    "{sep}tmp{sep}feature-daemon-global.db.checkpointed.20260430T012609Z.sqlite"
                ),
                false,
            ),
            // Real fixtures — MUST be suppressed.
            (
                format!("{sep}tmp{sep}feature-daemon-global.db.checkpointed.20260430T012609Z.db"),
                true,
            ),
            (
                format!("{sep}tmp{sep}feature-daemon-project.db.checkpointed.20260430T015747Z.db"),
                true,
            ),
            // Mixed-case stamps still classified (lowercase compare).
            (
                format!("{sep}tmp{sep}foo.db.CHECKPOINTED.20260430T015747Z.DB"),
                true,
            ),
        ];
        for (path, expected) in cases {
            assert_eq!(
                is_checkpoint_fixture_path(&path),
                expected,
                "is_checkpoint_fixture_path({path:?}) misclassified"
            );
        }
    }
}
