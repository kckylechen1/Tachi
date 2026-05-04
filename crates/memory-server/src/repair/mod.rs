//! `tachi repair` — PR-5 storage hygiene CLI.
//!
//! Inventories every DB in `~/.tachi/manifest.json` (post-PR-2 GC) and runs a
//! small library of deterministic data-fix rules. Each rule supports
//! `dry_run` (default) and `apply` modes and reports per-DB findings. A
//! per-DB backup (`{path}.bak.{ts}`) is taken before any rule mutates that
//! DB unless `--no-backup` is passed.
//!
//! Rules implemented:
//! - **R1** FTS rebuild — drops + recreates `memories_fts`, fixes drift on
//!   hapi/tachi/openclaw/sigil/antigravity and missing fts5 on quant.
//! - **R2** Backfill NULL `retention_policy` per per-project DB (PR-1 only
//!   ran on the global DB).
//! - **R3** Quarantine resolution — list, restore, restore-all (cross-DB
//!   physical move), purge.
//! - **R4** Drop `dead_letter` and stale `completed` foundry_jobs.
//! - **R5** `PRAGMA integrity_check` + `quick_check`. Failure → SKIP further
//!   repairs on that DB.
//! - **R6** VACUUM INTO + atomic swap. Requires daemon to not be running.
//! - **R7** Orphan reference cleanup (memory_edges, agent_known_state,
//!   processed_events, access_history).
//! - **R8** Deterministic junk cleanup (exact duplicate old versions,
//!   foundry rerank cache records, empty JSON turn records).
//!
//! Exit codes: 0 clean (or successful dry-run with no findings), 1 if
//! repairs were found and not applied, 2 if any rule errored.

use std::path::{Path, PathBuf};

use rusqlite::{Connection, OpenFlags};

use crate::cli::{QuarantineAction, RepairAction};
use crate::manifest::{DbEntry, Manifest};

pub mod edges;
pub mod fts;
pub mod integrity;
pub mod inventory;
pub mod jobs;
pub mod junk;
pub mod quarantine;
pub mod report;
pub mod retention;
pub mod vacuum;

#[cfg(test)]
mod tests;

pub use report::{Finding, ReportBuilder, RuleReport};

/// Default ordered set of rules run when `--rule` is not supplied.
///
/// R8 (junk cleanup) is intentionally **excluded** from the default sweep:
/// even with conservative duplicate matching, it is a destructive cleanup rule
/// and must be opted in explicitly via `--rule R8`.
const DEFAULT_RULES: &[&str] = &["R5", "R1", "R2", "R3", "R4", "R7"];

#[derive(Debug)]
pub struct RepairExit {
    code: i32,
}

impl RepairExit {
    pub fn new(code: i32) -> Self {
        Self { code }
    }

    pub fn code(&self) -> i32 {
        self.code
    }
}

impl std::fmt::Display for RepairExit {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "repair completed with exit code {}", self.code)
    }
}

impl std::error::Error for RepairExit {}

/// Errors raised by individual rules. We log + accumulate rather than abort.
#[derive(Debug)]
#[allow(dead_code)]
pub enum RepairError {
    Sqlite(rusqlite::Error),
    Io(std::io::Error),
    Other(String),
}

impl std::fmt::Display for RepairError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RepairError::Sqlite(e) => write!(f, "sqlite: {e}"),
            RepairError::Io(e) => write!(f, "io: {e}"),
            RepairError::Other(s) => write!(f, "{s}"),
        }
    }
}

impl std::error::Error for RepairError {}

impl From<rusqlite::Error> for RepairError {
    fn from(e: rusqlite::Error) -> Self {
        RepairError::Sqlite(e)
    }
}
impl From<std::io::Error> for RepairError {
    fn from(e: std::io::Error) -> Self {
        RepairError::Io(e)
    }
}

/// Per-DB context handed to each rule.
pub struct DbContext {
    pub label: String,
    pub path: PathBuf,
    #[allow(dead_code)]
    pub schema_kind: String,
    pub conn: Connection,
}

impl DbContext {
    pub fn open(entry: &DbEntry) -> Result<Self, RepairError> {
        let path = PathBuf::from(&entry.path);
        let conn = Connection::open(&path)?;
        // Match the rest of the codebase: prefer WAL & shorter busy timeout
        // for repair sessions running alongside a possibly-live daemon.
        let _ = conn.busy_timeout(std::time::Duration::from_secs(5));
        Ok(DbContext {
            label: inventory::label_for(entry),
            path,
            schema_kind: entry.schema_kind.clone(),
            conn,
        })
    }
}

/// Trait every rule implements.
pub trait RepairRule {
    fn id(&self) -> &'static str;
    fn name(&self) -> &'static str;
    /// Inspect the DB and report what _would_ change. Must be side-effect free.
    fn dry_run(&self, db: &mut DbContext) -> Result<RuleReport, RepairError>;
    /// Apply repairs. Must be a superset of dry_run findings.
    fn apply(&self, db: &mut DbContext) -> Result<RuleReport, RepairError>;
    /// Whether this rule mutates the DB file (used to decide whether to take
    /// a backup before invocation).
    fn mutates(&self) -> bool {
        true
    }
}

/// Top-level entry point invoked from bootstrap.rs.
pub async fn run_repair(
    action: Option<RepairAction>,
    db_filter: Option<String>,
    rule_filter: Vec<String>,
    apply: bool,
    no_backup: bool,
    json_out: bool,
    app_home: &Path,
) -> Result<(), Box<dyn std::error::Error>> {
    // Register the FTS5 `simple` tokenizer + sqlite-vec auto-extensions once,
    // BEFORE any Connection::open. Required for any DB whose schema includes
    // `memories_fts` (i.e. every tachi-schema DB).
    if let Err(e) = libsimple::enable_auto_extension() {
        eprintln!("warning: simple tokenizer init failed: {e}");
    }
    memory_core::db::register_sqlite_vec();

    // Subaction dispatch first.
    if let Some(act) = action {
        return match act {
            RepairAction::Quarantine { action } => run_quarantine(action, app_home, json_out).await,
            RepairAction::Vacuum { db, apply } => {
                vacuum::run_vacuum_cli(&db, apply, app_home, json_out).await
            }
            RepairAction::Fts { db, apply } => {
                run_fts_cli(&db, apply, app_home, json_out, no_backup).await
            }
            RepairAction::Report { json } => {
                eprintln!("`tachi repair report` is a stub — re-run `tachi repair --json` for the latest scan.");
                if json {
                    println!("{{\"summary\":{{\"hint\":\"re-run with --json\"}}}}");
                }
                Ok(())
            }
        };
    }

    // No subcommand → multi-DB sweep over all rules (or filtered).
    let manifest_path =
        Manifest::default_path(&dirs::home_dir().unwrap_or_else(|| PathBuf::from(".")));
    // app_home is canonical; manifest lives at <app_home>/manifest.json.
    let manifest_path = if app_home.join("manifest.json").exists() {
        app_home.join("manifest.json")
    } else {
        manifest_path
    };
    let manifest = Manifest::load_or_empty(&manifest_path);
    let entries = inventory::select_dbs(&manifest, db_filter.as_deref());

    let mut active_rules = resolve_rules(&rule_filter);
    // R5 (integrity check) is a safety gate: always run it first regardless
    // of user-supplied filter so that a failing integrity check can block
    // mutating rules.
    if !active_rules.iter().any(|r| r == "R5") {
        active_rules.insert(0, "R5".to_string());
    }

    let mut report = ReportBuilder::new(apply);
    let daemon_alive = inventory::daemon_alive(app_home);
    if daemon_alive {
        report.note(
            "daemon is running — R6 (VACUUM) skipped; in-DB rules continue with WAL.".to_string(),
        );
    }

    for entry in entries {
        let mut ctx = match DbContext::open(&entry) {
            Ok(c) => c,
            Err(e) => {
                report.push_open_error(&entry, e.to_string());
                continue;
            }
        };

        let mut backed_up = false;
        let mut integrity_ok = true;

        for rule_id in &active_rules {
            let rule: Box<dyn RepairRule> = match rule_id.as_str() {
                "R1" => Box::new(fts::FtsRebuild),
                "R2" => Box::new(retention::RetentionBackfill),
                "R3" => Box::new(quarantine::QuarantineSweep),
                "R4" => Box::new(jobs::JobsPurge::default()),
                "R5" => Box::new(integrity::IntegrityCheck),
                "R7" => Box::new(edges::OrphanRefs),
                "R8" => Box::new(junk::JunkCleanup),
                // R6 deliberately not part of bulk sweep — too aggressive.
                _ => continue,
            };

            // Skip mutating rules if integrity is bad.
            if !integrity_ok && rule.mutates() {
                report.push_skip(
                    &ctx,
                    rule.id(),
                    rule.name(),
                    "skipped: integrity_check failed earlier",
                );
                continue;
            }

            let rr = if apply {
                if rule.mutates() && !no_backup && !backed_up {
                    match backup_db(&ctx.path) {
                        Ok(p) => {
                            report.note(format!("backed up {} → {}", ctx.label, p.display()));
                            backed_up = true;
                        }
                        Err(e) => {
                            report.note(format!(
                                "[X] backup failed for {}: {e}; SKIPPING mutating rules",
                                ctx.label
                            ));
                            // Treat as if integrity bad so we don't mutate.
                            integrity_ok = false;
                            continue;
                        }
                    }
                }
                rule.apply(&mut ctx)
            } else {
                rule.dry_run(&mut ctx)
            };

            match rr {
                Ok(r) => {
                    if rule.id() == "R5" && r.findings.iter().any(|f| f.kind == "integrity_fail") {
                        integrity_ok = false;
                    }
                    report.push(r);
                }
                Err(e) => {
                    report.push_error(&ctx, rule.id(), rule.name(), e.to_string());
                }
            }
        }
    }

    let exit = report.render(json_out);
    if exit != 0 {
        return Err(Box::new(RepairExit::new(exit)));
    }
    Ok(())
}

fn resolve_rules(rule_filter: &[String]) -> Vec<String> {
    if rule_filter.is_empty() {
        DEFAULT_RULES.iter().map(|s| s.to_string()).collect()
    } else {
        rule_filter
            .iter()
            .map(|s| s.trim().to_uppercase())
            .filter(|s| !s.is_empty())
            .collect()
    }
}

/// Take a consistent SQLite snapshot backup at `{path}.bak.{ts}`.
pub fn backup_db(path: &Path) -> std::io::Result<PathBuf> {
    let ts = chrono::Utc::now().format("%Y%m%d-%H%M%S");
    let backup = sibling_with_suffix(path, &format!("bak.{ts}"));
    let _ = libsimple::enable_auto_extension();
    memory_core::db::register_sqlite_vec();
    let src = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY)
        .map_err(sqlite_io_error)?;
    let mut dst = Connection::open(&backup).map_err(sqlite_io_error)?;
    let backup_job = rusqlite::backup::Backup::new(&src, &mut dst).map_err(sqlite_io_error)?;
    backup_job
        .run_to_completion(128, std::time::Duration::from_millis(100), None)
        .map_err(sqlite_io_error)?;
    Ok(backup)
}

fn sqlite_io_error(e: rusqlite::Error) -> std::io::Error {
    std::io::Error::new(std::io::ErrorKind::Other, e)
}

fn sibling_with_suffix(path: &Path, suffix: &str) -> PathBuf {
    let mut s = path.as_os_str().to_owned();
    s.push(".");
    s.push(suffix);
    PathBuf::from(s)
}

// ─── Sub-dispatchers ─────────────────────────────────────────────────────────

async fn run_quarantine(
    action: QuarantineAction,
    app_home: &Path,
    json_out: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    let manifest_path = if app_home.join("manifest.json").exists() {
        app_home.join("manifest.json")
    } else {
        Manifest::default_path(&dirs::home_dir().unwrap_or_else(|| PathBuf::from(".")))
    };
    let manifest = Manifest::load_or_empty(&manifest_path);
    match action {
        QuarantineAction::List { json } => quarantine::cmd_list(&manifest, json || json_out),
        QuarantineAction::Restore { id, apply } => {
            quarantine::cmd_restore(&manifest, &id, apply, json_out)
        }
        QuarantineAction::RestoreAll { to_db, apply } => {
            quarantine::cmd_restore_all(&manifest, &to_db, apply, json_out)
        }
        QuarantineAction::Purge { older_than, apply } => {
            quarantine::cmd_purge(&manifest, older_than, apply, json_out)
        }
    }
}

async fn run_fts_cli(
    db: &str,
    apply: bool,
    app_home: &Path,
    json_out: bool,
    no_backup: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    let manifest_path = if app_home.join("manifest.json").exists() {
        app_home.join("manifest.json")
    } else {
        Manifest::default_path(&dirs::home_dir().unwrap_or_else(|| PathBuf::from(".")))
    };
    let manifest = Manifest::load_or_empty(&manifest_path);
    let entries = inventory::select_dbs(&manifest, Some(db));
    if entries.is_empty() {
        return Err(format!("no DB matched '{db}'").into());
    }
    let mut report = ReportBuilder::new(apply);
    for entry in entries {
        let mut ctx = match DbContext::open(&entry) {
            Ok(c) => c,
            Err(e) => {
                report.push_open_error(&entry, e.to_string());
                continue;
            }
        };
        if apply && !no_backup {
            match backup_db(&ctx.path) {
                Ok(p) => report.note(format!("backed up {} → {}", ctx.label, p.display())),
                Err(e) => {
                    report.note(format!("[X] backup failed for {}: {e}", ctx.label));
                    continue;
                }
            }
        }
        let r = if apply {
            fts::FtsRebuild.apply(&mut ctx)
        } else {
            fts::FtsRebuild.dry_run(&mut ctx)
        };
        match r {
            Ok(rep) => report.push(rep),
            Err(e) => report.push_error(&ctx, "R1", "FTS rebuild", e.to_string()),
        }
    }
    let exit = report.render(json_out);
    if exit != 0 {
        return Err(Box::new(RepairExit::new(exit)));
    }
    Ok(())
}
