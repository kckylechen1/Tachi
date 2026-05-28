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

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::time::Duration;

use chrono::{DateTime, Utc};
use memory_core::MemoryEntry;
use rusqlite::OptionalExtension;
use serde_json::json;

use memory_core::{
    get_foundry_config, job_status_histogram, set_foundry_config, JobStatusHistogram, MemoryStore,
    PerDbConfig,
};

use crate::cli::{DaemonAction, FoundryAction, WatcherAction};
use crate::daemon_lock::{process_alive, read_pid_file};
use crate::manifest::{DbRole, Manifest};

/// `in_progress` jobs older than this are flagged as stuck in `tachi status`.
/// Matches the safety-net poll cadence in [`crate::foundry_scheduler`] with
/// generous headroom so transient long-running jobs don't trigger noise.
const STUCK_THRESHOLD_SECS: i64 = 600;

/// Voyage-4 embeddings and `memories_vec` are both expected to be 1024-dim.
const EXPECTED_EMBEDDING_DIM: usize = 1024;

/// Daily distill should normally run every 24h. Flag after 36h to avoid
/// false positives during one missed scheduler window.
const DISTILL_STALE_THRESHOLD_SECS: i64 = 36 * 3600;

/// Refresh interval for `tachi status --watch`.
const WATCH_INTERVAL: Duration = Duration::from_secs(2);

pub(crate) struct ApiKeyDef {
    pub(crate) key: &'static str,
    pub(crate) label: &'static str,
    pub(crate) required: bool,
    pub(crate) deprecated: bool,
    pub(crate) aliases: &'static [&'static str],
}

pub(crate) const API_KEY_DEFS: &[ApiKeyDef] = &[
    ApiKeyDef {
        key: "VOYAGE_API_KEY",
        label: "Voyage embeddings",
        required: true,
        deprecated: false,
        aliases: &[],
    },
    ApiKeyDef {
        key: "VOYAGE_RERANK_API_KEY",
        label: "Voyage rerank",
        required: false,
        deprecated: false,
        aliases: &["VOYAGE_API_KEY"],
    },
    ApiKeyDef {
        key: "SILICONFLOW_API_KEY",
        label: "SiliconFlow/Qwen background LLM",
        required: true,
        deprecated: false,
        aliases: &["EXTRACT_API_KEY", "SUMMARY_API_KEY", "DISTILL_API_KEY", "REASONING_API_KEY"],
    },
    ApiKeyDef {
        key: "MINIMAX_API_KEY",
        label: "MiniMax legacy distill",
        required: false,
        deprecated: true,
        aliases: &[],
    },
    ApiKeyDef {
        key: "REASONING_API_KEY",
        label: "Legacy reasoning lane",
        required: false,
        deprecated: true,
        aliases: &["ZAI_API_KEY", "BIGMODEL_API_KEY"],
    },
];

/// `tachi status` entrypoint.
pub(crate) async fn run_status(
    watch: bool,
    json_out: bool,
    hide_orphans: bool,
    probe_keys: bool,
    app_home: &Path,
    global_db_path: &Path,
    project_db_path: Option<&Path>,
) -> Result<(), Box<dyn std::error::Error>> {
    if !watch {
        return render_one(
            json_out,
            hide_orphans,
            probe_keys,
            app_home,
            global_db_path,
            project_db_path,
        )
        .await;
    }

    if json_out {
        // --watch + --json doesn't make sense (JSON consumers don't want a
        // screen-clearing infinite stream). Treat as one-shot.
        return render_one(
            true,
            hide_orphans,
            probe_keys,
            app_home,
            global_db_path,
            project_db_path,
        )
        .await;
    }

    loop {
        // ANSI clear + cursor home so each frame replaces the previous.
        print!("\x1b[2J\x1b[H");
        if let Err(e) = render_one(
            false,
            hide_orphans,
            probe_keys,
            app_home,
            global_db_path,
            project_db_path,
        )
        .await
        {
            eprintln!("[!] status render failed: {e}");
        }
        tokio::time::sleep(WATCH_INTERVAL).await;
    }
}

async fn render_one(
    json_out: bool,
    hide_orphans: bool,
    probe_keys: bool,
    app_home: &Path,
    global_db_path: &Path,
    project_db_path: Option<&Path>,
) -> Result<(), Box<dyn std::error::Error>> {
    let snapshot = collect_snapshot(app_home, global_db_path, project_db_path);
    let provider_probes = if probe_keys {
        run_provider_probes(global_db_path).await
    } else {
        Vec::new()
    };

    if json_out {
        // JSON consumers always see the full snapshot incl. orphans, so
        // dashboards/scripts retain visibility regardless of how the
        // operator filters their human-readable view.
        let mut v = serde_json::to_value(&snapshot)?;
        if let Some(obj) = v.as_object_mut() {
            obj.insert("provider_probes".to_string(), serde_json::to_value(&provider_probes)?);
        }
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
            let marker = if db.vector_missing > 0 || vector_dimension_mismatch(db) {
                "[!]"
            } else {
                "[OK]"
            };
            let failures = if db.enrichment_failed_recent > 0 {
                format!(" enrichment_failed={}", db.enrichment_failed_recent)
            } else {
                String::new()
            };
            let dim = db
                .vector_dimension
                .map(|n| n.to_string())
                .unwrap_or_else(|| "unknown".to_string());
            println!(
                "       {marker} vectors={}/{} missing={} coverage={pct:.1}% dim={}{}",
                db.vector_count, db.memory_total, db.vector_missing, dim, failures
            );
        }
        if let Some(job) = &db.latest_job {
            println!(
                "       [i] latest_job kind={} status={} at={}",
                job.kind,
                job.status,
                job.updated_at.as_deref().unwrap_or("unknown")
            );
        }
        if let Some(job) = &db.latest_failed_job {
            let inferred = job
                .inferred_invalid_provider
                .as_deref()
                .map(|provider| format!(" provider={provider}"))
                .unwrap_or_default();
            println!(
                "       [X] latest_failed kind={} lane={} at={}{} reason={}",
                job.kind,
                job.lane.as_deref().unwrap_or("unknown"),
                job.updated_at.as_deref().unwrap_or("unknown"),
                inferred,
                truncate(job.reason.as_deref().unwrap_or("unknown"), 96)
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
    match &snapshot.distill_marker {
        Some(marker) => {
            let icon = if marker.is_stale { "[!]" } else { "[OK]" };
            println!(
                "  {icon} last distill: {} ({}) marker={}",
                marker.last_run_at, marker.age, marker.path
            );
        }
        None => println!("  [!] last distill marker missing"),
    }
    println!();

    println!("Provider Keys");
    for key in &snapshot.api_keys {
        let marker = match key.status.as_str() {
            "configured" => "[OK]",
            "drift" => "[!]",
            "deprecated-unset" => "[i]",
            _ if key.required => "[!]",
            _ => "[i]",
        };
        println!(
            "  {marker} {name:<24} {status:<16} source={source} ({label})",
            name = key.name,
            status = key.status,
            source = key.source,
            label = key.label
        );
        if let Some(provider) = &key.inferred_invalid_provider {
            println!("       [X] inferred invalid provider/key from failed jobs: {provider}");
        }
        if let Some(warning) = &key.drift_warning {
            println!("       [!] {warning}");
        }
    }
    if probe_keys {
        println!("  live probes:");
        for probe in &provider_probes {
            println!(
                "    {}: {}{}",
                probe.name,
                probe.status,
                probe
                    .message
                    .as_ref()
                    .map(|msg| format!(" ({})", truncate(msg, 96)))
                    .unwrap_or_default()
            );
        }
    } else {
        println!("  live probes skipped (pass --probe-keys to test providers)");
    }
    println!();

    let total_pending: usize = snapshot.dbs.iter().map(|d| d.pending).sum();
    let total_orphan = snapshot.dbs.iter().filter(|d| d.orphan).count();
    let total_stuck: usize = snapshot.dbs.iter().map(|d| d.stuck_in_progress).sum();
    println!(
        "Summary: health_score={score}/100, {n} dbs, {pending} total pending, {orphan} orphan (informational), {stuck} stuck in_progress",
        score = snapshot.health_score,
        n = snapshot.dbs.len(),
        pending = total_pending,
        orphan = total_orphan,
        stuck = total_stuck
    );

    Ok(())
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
                "backfill_command": format_backfill_command(d),
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
    let readiness = agent_readiness_json(&app_home, &snapshot);

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
                "provider_auth_failures": auth_failures,
                "latest_failed_jobs": failed_jobs,
            },
            "warnings": warnings,
            "daily_pipeline": snapshot.last_daily_report,
            "distill": snapshot.distill_marker,
            "api_keys": snapshot.api_keys,
            "models": model_lanes_json(),
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

fn build_status_warnings(snapshot: &StatusSnapshot, daemon_state: &serde_json::Value) -> Vec<String> {
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
            "daemon not running — background tasks (enrichment, distill, GC) are paused".to_string(),
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
        if key.status == "drift" {
            warnings.push(format!(
                "provider key {} differs between Tachi Vault and env/config; prefer Vault as source of truth",
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

pub(crate) fn provider_key_status_json(global_db_path: &Path) -> serde_json::Value {
    json!(collect_api_key_status(global_db_path))
}

#[derive(Debug, serde::Serialize)]
struct StatusSnapshot {
    daemon: DaemonStatus,
    dbs: Vec<DbStatus>,
    manifest_path: String,
    dispatches: Vec<DispatchStatus>,
    recent_evals: Vec<RecentEval>,
    last_daily_report: Option<String>,
    distill_marker: Option<DistillMarkerStatus>,
    api_keys: Vec<ApiKeyStatus>,
    health_score: u8,
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
    vector_dimension: Option<usize>,
    enrichment_failed_recent: usize,
    pending: usize,
    running: usize,
    completed: usize,
    failed: usize,
    gc_eligible: usize,
    /// Number of `in_progress` jobs older than [`STUCK_THRESHOLD_SECS`].
    stuck_in_progress: usize,
    latest_job: Option<LatestFoundryJob>,
    latest_failed_job: Option<LatestFailedJob>,
    error: Option<String>,
}

#[derive(Debug, Clone, serde::Serialize)]
struct LatestFoundryJob {
    id: String,
    kind: String,
    status: String,
    updated_at: Option<String>,
}

#[derive(Debug, Clone, serde::Serialize)]
struct LatestFailedJob {
    id: String,
    kind: String,
    lane: Option<String>,
    updated_at: Option<String>,
    reason: Option<String>,
    inferred_invalid_provider: Option<String>,
}

#[derive(Debug, serde::Serialize)]
struct DistillMarkerStatus {
    path: String,
    last_run_at: String,
    age_seconds: i64,
    age: String,
    is_stale: bool,
}

#[derive(Debug, serde::Serialize)]
struct ApiKeyStatus {
    name: String,
    label: String,
    required: bool,
    deprecated: bool,
    status: String,
    source: String,
    env_configured: bool,
    vault_configured: bool,
    drift_warning: Option<String>,
    inferred_invalid_provider: Option<String>,
}

#[derive(Debug, Clone, serde::Serialize)]
pub(crate) struct ProviderProbeResult {
    pub(crate) name: String,
    pub(crate) status: String,
    pub(crate) message: Option<String>,
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
    let mut api_keys = collect_api_key_status(global_db_path);
    apply_inferred_provider_failures(&mut api_keys, &dbs);
    let health_score = calculate_health_score(&daemon, &dbs, distill_marker.as_ref(), &api_keys);

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
    // Diagnostics must NOT take write locks or run schema migrations on
    // potentially read-only DBs. `open_with_label` initializes/migrates
    // the schema on every open; `open_read_only` skips that work.
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
    collect_entries_for_status(server.global_db_path_buf().as_path(), path_prefix, limit, "global", &mut rows);
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
                    infer_provider_from_failed_job(&kind, lane.as_deref(), reason)
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
        age: format_elapsed(age),
        is_stale: age_seconds > DISTILL_STALE_THRESHOLD_SECS,
    })
}

fn collect_api_key_status(global_db_path: &Path) -> Vec<ApiKeyStatus> {
    let mut vault_names = HashSet::new();
    if let Some(path) = global_db_path.to_str() {
        if let Ok(store) = MemoryStore::open_read_only(path) {
            if let Ok(entries) = store.vault_list_entries() {
                vault_names.extend(
                    entries
                        .into_iter()
                        .filter(|entry| entry.secret_type == "api_key")
                        .map(|entry| entry.name),
                );
            }
        }
    }
    let env_file_names = collect_config_env_key_names();

    API_KEY_DEFS
        .iter()
        .map(|def| {
            let env_configured = std::env::var(def.key)
                .ok()
                .map(|value| !value.trim().is_empty())
                .unwrap_or(false);
            let vault_configured = vault_names.contains(def.key);
            let config_present = env_file_names.contains(def.key);
            let alias_configured = def.aliases.iter().any(|alias| {
                vault_names.contains(*alias)
                    || env_file_names.contains(*alias)
                    || std::env::var(alias)
                        .ok()
                        .is_some_and(|value| !value.trim().is_empty())
            });
            let file_configured = config_present;
            let (status, source) = if vault_configured && (env_configured || file_configured) {
                (
                    "drift",
                    if env_configured {
                        "vault+env"
                    } else {
                        "vault+config.env"
                    },
                )
            } else if vault_configured {
                ("configured", "vault")
            } else if env_configured || file_configured || alias_configured {
                (
                    "configured",
                    if env_configured {
                        "env"
                    } else if file_configured {
                        "config.env"
                    } else {
                        "alias"
                    },
                )
            } else if def.deprecated {
                ("deprecated-unset", "none")
            } else {
                ("missing", "none")
            };
            let drift_warning = if vault_configured && (env_configured || file_configured) {
                Some("same key is set in both Vault and env/config; runtime prefers Vault when unlocked but env may be used before unlock".to_string())
            } else if vault_configured && config_present {
                Some("same key name exists in config.env and Vault; remove config.env copy after confirming Vault unlock".to_string())
            } else {
                None
            };
            ApiKeyStatus {
                name: def.key.to_string(),
                label: def.label.to_string(),
                required: def.required,
                deprecated: def.deprecated,
                status: status.to_string(),
                source: source.to_string(),
                env_configured,
                vault_configured,
                drift_warning,
                inferred_invalid_provider: None,
            }
        })
        .collect()
}

fn collect_config_env_key_names() -> HashSet<String> {
    let mut paths = Vec::new();
    if let Some(home) = dirs::home_dir() {
        paths.push(home.join(".tachi").join("config.env"));
        paths.push(home.join(".sigil").join("config.env"));
    }
    if let Ok(home) = std::env::var("TACHI_HOME") {
        paths.push(PathBuf::from(home).join("config.env"));
    }
    paths.push(PathBuf::from(".tachi/config.env"));
    paths.push(PathBuf::from(".sigil/config.env"));

    let mut names = HashSet::new();
    for path in paths {
        let Ok(raw) = std::fs::read_to_string(path) else {
            continue;
        };
        for line in raw.lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            if let Some((key, value)) = line.split_once('=') {
                if !value.trim().is_empty() {
                    names.insert(key.trim().to_string());
                }
            }
        }
    }
    names
}

fn calculate_health_score(
    daemon: &DaemonStatus,
    dbs: &[DbStatus],
    distill_marker: Option<&DistillMarkerStatus>,
    api_keys: &[ApiKeyStatus],
) -> u8 {
    let mut score = 100i32;
    if !matches!(daemon, DaemonStatus::Running { .. }) {
        score -= 20;
    }
    let failed_jobs: usize = dbs.iter().map(|db| db.failed).sum();
    score -= (failed_jobs as i32).min(25);
    let stuck_jobs: usize = dbs.iter().map(|db| db.stuck_in_progress).sum();
    score -= ((stuck_jobs as i32) * 5).min(20);
    let low_vector_dbs = dbs
        .iter()
        .filter(|db| db.memory_total > 0 && db.vector_coverage < 0.9)
        .count();
    score -= ((low_vector_dbs as i32) * 10).min(25);
    let dim_mismatch_dbs = dbs
        .iter()
        .filter(|db| vector_dimension_mismatch(db))
        .count();
    score -= ((dim_mismatch_dbs as i32) * 10).min(20);
    if distill_marker.map(|m| m.is_stale).unwrap_or(true) {
        score -= 10;
    }
    let missing_required_keys = api_keys
        .iter()
        .filter(|key| key.required && key.status == "missing")
        .count();
    score -= ((missing_required_keys as i32) * 10).min(20);
    let drift_keys = api_keys.iter().filter(|key| key.status == "drift").count();
    score -= ((drift_keys as i32) * 5).min(10);
    let inferred_invalid_keys = api_keys
        .iter()
        .filter(|key| key.inferred_invalid_provider.is_some())
        .count();
    score -= ((inferred_invalid_keys as i32) * 10).min(20);
    score.clamp(0, 100) as u8
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

fn vector_dimension_mismatch(db: &DbStatus) -> bool {
    db.vector_count > 0 && db.vector_dimension != Some(EXPECTED_EMBEDDING_DIM)
}

fn is_auth_error(reason: &str) -> bool {
    let lower = reason.to_ascii_lowercase();
    lower.contains("401")
        || lower.contains("403")
        || lower.contains("unauthorized")
        || lower.contains("forbidden")
        || lower.contains("invalid api key")
        || (lower.contains("api key") && lower.contains("invalid"))
        || lower.contains("permission denied")
}

fn infer_provider_from_failed_job(kind: &str, lane: Option<&str>, reason: &str) -> Option<String> {
    let explicit = infer_provider_from_auth_error(reason);
    if explicit.as_deref().is_some_and(|provider| provider != "UNKNOWN") {
        return explicit;
    }
    if !is_auth_error(reason) {
        return None;
    }
    if kind == "recall_rerank_cache" || kind == "memory_distill" {
        return Some("SILICONFLOW".to_string());
    }
    match lane.unwrap_or_default() {
        "rerank" | "embedding" => Some("VOYAGE".to_string()),
        "extract" | "extraction" | "summary" | "distill" | "reasoning" => {
            Some("SILICONFLOW".to_string())
        }
        _ => explicit,
    }
}

fn infer_provider_from_auth_error(reason: &str) -> Option<String> {
    if !is_auth_error(reason) {
        return None;
    }
    let lower = reason.to_ascii_lowercase();
    if lower.contains("voyage") || lower.contains("voyageai") {
        Some("VOYAGE".to_string())
    } else if lower.contains("siliconflow") || lower.contains("qwen") {
        Some("SILICONFLOW".to_string())
    } else if lower.contains("minimax") {
        Some("MINIMAX".to_string())
    } else if lower.contains("zai") || lower.contains("bigmodel") || lower.contains("glm") {
        Some("REASONING".to_string())
    } else {
        Some("UNKNOWN".to_string())
    }
}

fn provider_to_key(provider: &str) -> Option<&'static str> {
    match provider {
        "VOYAGE" => Some("VOYAGE_API_KEY"),
        "SILICONFLOW" => Some("SILICONFLOW_API_KEY"),
        "MINIMAX" => Some("MINIMAX_API_KEY"),
        "REASONING" => Some("REASONING_API_KEY"),
        _ => None,
    }
}

fn apply_inferred_provider_failures(api_keys: &mut [ApiKeyStatus], dbs: &[DbStatus]) {
    let mut providers = HashSet::new();
    for db in dbs {
        if let Some(provider) = db
            .latest_failed_job
            .as_ref()
            .and_then(|job| job.inferred_invalid_provider.as_deref())
        {
            providers.insert(provider.to_string());
        }
    }
    for provider in providers {
        if let Some(key_name) = provider_to_key(&provider) {
            if let Some(key) = api_keys.iter_mut().find(|key| key.name == key_name) {
                key.inferred_invalid_provider = Some(provider.clone());
            }
        }
    }
}

pub(crate) fn model_lanes_json() -> serde_json::Value {
    json!({
        "embedding": {
            "provider": "voyage",
            "model": "voyage-4",
            "expected_dimension": EXPECTED_EMBEDDING_DIM,
            "key": "VOYAGE_API_KEY",
        },
        "rerank": {
            "provider": "voyage",
            "model": "rerank-2.5",
            "keys": ["VOYAGE_RERANK_API_KEY", "VOYAGE_API_KEY"],
        },
        "recall_rerank_cache": {
            "query_generation_provider": "extract/SiliconFlow",
            "rerank_provider": "voyage",
            "auth_failure_hint": "403 during query generation points to SILICONFLOW_API_KEY; 403 during Voyage rerank points to VOYAGE_RERANK_API_KEY or VOYAGE_API_KEY",
        },
        "extract": {
            "provider": "openai-compatible",
            "default_base_url": "https://api.siliconflow.cn/v1/chat/completions",
            "default_model": "Qwen/Qwen3.5-27B",
            "keys": ["EXTRACT_API_KEY", "SILICONFLOW_API_KEY"],
        },
        "summary": {
            "provider": "openai-compatible",
            "inherits": "extract",
            "keys": ["SUMMARY_API_KEY", "EXTRACT_API_KEY", "SILICONFLOW_API_KEY"],
        },
        "distill": {
            "provider": "raw_api default (FOUNDRY_DISTILL_BACKEND), claude_cli optional",
            "keys": ["DISTILL_API_KEY", "REASONING_API_KEY", "ZAI_API_KEY", "BIGMODEL_API_KEY", "EXTRACT_API_KEY", "SILICONFLOW_API_KEY"],
        },
        "reasoning": {
            "provider": "claude-cli-first, openai-compatible fallback",
            "keys": ["REASONING_API_KEY", "ZAI_API_KEY", "BIGMODEL_API_KEY", "DISTILL_API_KEY", "EXTRACT_API_KEY", "SILICONFLOW_API_KEY"],
        }
    })
}

fn agent_readiness_json(app_home: &Path, snapshot: &StatusSnapshot) -> serde_json::Value {
    let home = dirs::home_dir().unwrap_or_else(|| PathBuf::from("."));
    let agent_rule_files = vec![
        ("claude", home.join(".claude").join("CLAUDE.md")),
        ("codex", home.join(".codex").join("AGENTS.md")),
        ("gemini", home.join(".gemini").join("GEMINI.md")),
    ];
    let rules: Vec<_> = agent_rule_files
        .into_iter()
        .map(|(agent, path)| {
            let installed = std::fs::read_to_string(&path)
                .ok()
                .is_some_and(|body| body.contains("BEGIN TACHI MEMORY RULES"));
            json!({
                "agent": agent,
                "path": path.display().to_string(),
                "exists": path.exists(),
                "tachi_rules_installed": installed,
            })
        })
        .collect();
    let mcp_configs = vec![
        ("claude", home.join(".claude").join("mcp.json")),
        ("cursor", home.join(".cursor").join("mcp.json")),
        ("gemini", home.join(".gemini").join("mcp.json")),
        ("amp", home.join("Library/Application Support/Amp/settings.json")),
    ];
    let mcp: Vec<_> = mcp_configs
        .into_iter()
        .map(|(agent, path)| {
            let raw = std::fs::read_to_string(&path).unwrap_or_default();
            json!({
                "agent": agent,
                "path": path.display().to_string(),
                "exists": path.exists(),
                "mentions_tachi": raw.to_ascii_lowercase().contains("tachi") || raw.contains("memory-server"),
            })
        })
        .collect();
    json!({
        "rules": rules,
        "mcp_configs": mcp,
        "runs_root": crate::shell_ops::shell_runs_root().display().to_string(),
        "last_distill_marker": snapshot.distill_marker.as_ref().map(|m| m.path.clone()),
        "doctor_hint": format!("tachi doctor --jobs --probe-keys --roots {}", shell_quote(&app_home.display().to_string())),
    })
}

fn format_backfill_command(db: &DbStatus) -> String {
    format!("tachi backfill-vectors --db {}", shell_quote(&db.path))
}

fn shell_quote(value: &str) -> String {
    if value
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '/' | '.' | '_' | '-' | ':' | '='))
    {
        value.to_string()
    } else {
        format!("'{}'", value.replace('\'', "'\\''"))
    }
}

pub(crate) async fn run_provider_probes(global_db_path: &Path) -> Vec<ProviderProbeResult> {
    let llm = match crate::llm::LlmClient::new() {
        Ok(client) => client,
        Err(err) => {
            return vec![ProviderProbeResult {
                name: "llm_client".to_string(),
                status: "failed".to_string(),
                message: Some(err),
            }];
        }
    };
    if let Ok(secrets) = load_keychain_vault_api_key_values(global_db_path) {
        llm.set_provider_secrets(secrets);
    }

    let mut out = Vec::new();
    let embed = tokio::time::timeout(
        std::time::Duration::from_secs(15),
        llm.embed_voyage("tachi provider probe", "document"),
    )
    .await;
    out.push(match embed {
        Ok(Ok(vec)) => ProviderProbeResult {
            name: "voyage_embed".to_string(),
            status: "ok".to_string(),
            message: Some(format!("{} dims", vec.len())),
        },
        Ok(Err(err)) => ProviderProbeResult {
            name: "voyage_embed".to_string(),
            status: "failed".to_string(),
            message: Some(err),
        },
        Err(_) => ProviderProbeResult {
            name: "voyage_embed".to_string(),
            status: "timeout".to_string(),
            message: Some("timed out after 15s".to_string()),
        },
    });

    let rerank = tokio::time::timeout(
        std::time::Duration::from_secs(15),
        llm.rerank_voyage(
            "tachi provider probe",
            &[
                "Tachi stores operational memory".to_string(),
                "Unrelated weather note".to_string(),
            ],
            1,
        ),
    )
    .await;
    out.push(match rerank {
        Ok(Ok(rows)) => ProviderProbeResult {
            name: "voyage_rerank".to_string(),
            status: "ok".to_string(),
            message: Some(format!("{} result(s)", rows.len())),
        },
        Ok(Err(err)) => ProviderProbeResult {
            name: "voyage_rerank".to_string(),
            status: "failed".to_string(),
            message: Some(err),
        },
        Err(_) => ProviderProbeResult {
            name: "voyage_rerank".to_string(),
            status: "timeout".to_string(),
            message: Some("timed out after 15s".to_string()),
        },
    });

    let chat = tokio::time::timeout(
        std::time::Duration::from_secs(20),
        llm.call_extract_llm(
            "Return exactly OK.",
            "Provider probe. Reply OK only.",
            None,
            0.0,
            8,
        ),
    )
    .await;
    out.push(match chat {
        Ok(Ok(text)) => ProviderProbeResult {
            name: "chat_extract".to_string(),
            status: "ok".to_string(),
            message: Some(text.chars().take(80).collect()),
        },
        Ok(Err(err)) => ProviderProbeResult {
            name: "chat_extract".to_string(),
            status: "failed".to_string(),
            message: Some(err),
        },
        Err(_) => ProviderProbeResult {
            name: "chat_extract".to_string(),
            status: "timeout".to_string(),
            message: Some("timed out after 20s".to_string()),
        },
    });
    out
}

fn load_keychain_vault_api_key_values(
    vault_db_path: &Path,
) -> Result<Vec<(String, String)>, Box<dyn std::error::Error>> {
    if !cfg!(target_os = "macos") {
        return Ok(Vec::new());
    }

    use base64::{engine::general_purpose::STANDARD as B64, Engine};

    let output = std::process::Command::new("security")
        .args([
            "find-generic-password",
            "-s",
            "tachi-vault",
            "-a",
            "default",
            "-w",
        ])
        .output()?;
    if !output.status.success() {
        return Ok(Vec::new());
    }

    let password = String::from_utf8(output.stdout)?.trim().to_string();
    if password.is_empty() {
        return Ok(Vec::new());
    }

    let vault_db_str = vault_db_path.to_str().ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!(
                "Vault DB path contains invalid UTF-8: {}",
                vault_db_path.display()
            ),
        )
    })?;
    let store = memory_core::MemoryStore::open_read_only(vault_db_str)?;
    let Some(config) = store.vault_get_config()? else {
        return Ok(Vec::new());
    };

    let salt = B64.decode(&config.salt)?;
    let key = crate::vault_crypto::derive_key(&password, &salt)?;
    if !crate::vault_crypto::verify_password(&key, &config.verifier)? {
        return Ok(Vec::new());
    }

    let mut out = Vec::new();
    for entry in store.vault_list_entries()? {
        if entry.secret_type != "api_key"
            || !entry.name.ends_with("_API_KEY")
            || entry.allowed_agents.is_some()
        {
            continue;
        }
        let decrypted = crate::vault_crypto::decrypt(&key, &entry.encrypted_value, &entry.nonce)?;
        let value = String::from_utf8(decrypted)?;
        if !value.trim().is_empty() {
            out.push((entry.name, value));
        }
    }
    Ok(out)
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
                // SAFETY: `pid` was validated above via `read_pid()` and
                // `file.read_to_string()` that returned a valid i32. SIGTERM is a defined
                // constant; no user-controlled data flows into this call.
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

pub(crate) async fn run_watcher(
    action: WatcherAction,
    global_db_path: &Path,
    project_db_path: Option<PathBuf>,
) -> Result<(), Box<dyn std::error::Error>> {
    let server = crate::MemoryServer::new(global_db_path.to_path_buf(), project_db_path)?;
    match action {
        WatcherAction::Status { json: json_out } => {
            let params = crate::tool_params::TachiMemoryParams {
                action: "briefing".to_string(),
                query: Some("passive watcher".to_string()),
                scope: None,
                top_k: 1,
                path_prefix: None,
                file_context: None,
                error_context: None,
                category: None,
                include_archived: false,
                enable_rerank: false,
                synthesize: false,
                model: None,
                text: None,
                title: None,
                summary: None,
                topic: None,
                keywords: Vec::new(),
                entities: Vec::new(),
                importance: None,
                retention_policy: None,
                kind: None,
                path: None,
                id: None,
                force: false,
                source: None,
                flow_id: None,
                event: None,
                state: None,
                project: None,
                domain: None,
            };
            let body = crate::facade_memory_ops::handle_tachi_memory(&server, params).await?;
            let value: serde_json::Value = serde_json::from_str(&body)?;
            let watcher = value
                .get("passive_watcher")
                .cloned()
                .unwrap_or_else(|| json!({"status":"unknown"}));
            if json_out {
                println!("{}", serde_json::to_string_pretty(&watcher)?);
            } else {
                println!("passive watcher: {}", watcher["status"].as_str().unwrap_or("unknown"));
                if let Some(path) = watcher.get("latest_jsonl").and_then(|v| v.as_str()) {
                    println!("  latest_jsonl: {path}");
                }
                if let Some(note) = watcher.get("note").and_then(|v| v.as_str()) {
                    println!("  note: {note}");
                }
            }
            Ok(())
        }
        WatcherAction::CaptureLatest { json: json_out } => {
            let captured = crate::facade_memory_ops::capture_latest_claude_jsonl_checkpoint(&server)
                .await?
                .unwrap_or_else(|| json!({"status":"not_detected"}));
            if json_out {
                println!("{}", serde_json::to_string_pretty(&captured)?);
            } else {
                println!(
                    "passive watcher capture: {}",
                    captured["status"].as_str().unwrap_or("unknown")
                );
                if let Some(path) = captured.get("path").and_then(|v| v.as_str()) {
                    println!("  source: {path}");
                }
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
        assert_eq!(truncate("abcdefghij", 5), "ab...");
    }

    #[test]
    fn infer_provider_from_auth_error_maps_real_failures() {
        assert_eq!(
            infer_provider_from_failed_job(
                "recall_rerank_cache",
                Some("rerank"),
                "SiliconFlow 403 forbidden"
            ),
            Some("SILICONFLOW".to_string())
        );
        assert_eq!(
            infer_provider_from_auth_error("Voyage API error: 403 Forbidden"),
            Some("VOYAGE".to_string())
        );
        assert_eq!(
            infer_provider_from_failed_job("memory_distill", Some("distill"), "403 Forbidden"),
            Some("SILICONFLOW".to_string())
        );
        assert_eq!(infer_provider_from_auth_error("network timeout"), None);
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
