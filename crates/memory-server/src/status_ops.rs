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
use crate::manifest::Manifest;

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
        if let Some(err) = &db.error {
            println!("       [X] {err}");
        }
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

#[derive(Debug, serde::Serialize)]
struct StatusSnapshot {
    daemon: DaemonStatus,
    dbs: Vec<DbStatus>,
    manifest_path: String,
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
        let orphan = is_orphan_path(&path, global_db_path, project_db_path);
        if !path.exists() {
            dbs.push(DbStatus {
                path: entry.path.clone(),
                label,
                orphan,
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
            Ok((hist, stuck)) => dbs.push(DbStatus {
                path: entry.path.clone(),
                label,
                orphan,
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

    StatusSnapshot {
        daemon,
        dbs,
        manifest_path: manifest_path.display().to_string(),
    }
}

fn probe_db(path: &Path) -> Result<(JobStatusHistogram, usize), String> {
    let path_str = path
        .to_str()
        .ok_or_else(|| format!("non-utf8 path: {}", path.display()))?;
    let store =
        MemoryStore::open_with_label(path_str, "tachi-status").map_err(|e| format!("open: {e}"))?;
    let conn = store.connection();
    let hist = job_status_histogram(conn, 30).map_err(|e| format!("histogram: {e}"))?;
    let stuck = count_stuck_in_progress(conn).unwrap_or(0);
    Ok((hist, stuck))
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
fn is_orphan_path(db_path: &Path, global_db_path: &Path, project_db_path: Option<&Path>) -> bool {
    if paths_equal(db_path, global_db_path) {
        return false;
    }
    if let Some(project) = project_db_path {
        if paths_equal(db_path, project) {
            return false;
        }
    }
    named_project_from_path(db_path).is_none()
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

    #[test]
    fn orphan_classification_matches_scheduler_routing() {
        let global = PathBuf::from("/tmp/status/global/memory.db");
        let project = PathBuf::from("/tmp/status/project/memory.db");
        let named = PathBuf::from("/home/u/.tachi/projects/sigil/memory.db");
        let agent = PathBuf::from("/home/u/.tachi/agents/main/memory.db");
        assert!(!is_orphan_path(&global, &global, Some(&project)));
        assert!(!is_orphan_path(&project, &global, Some(&project)));
        assert!(!is_orphan_path(&named, &global, Some(&project)));
        assert!(is_orphan_path(&agent, &global, Some(&project)));
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
                format!("{sep}tmp{sep}feature-daemon-global.db.checkpointed.20260430T012609Z.sqlite"),
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
