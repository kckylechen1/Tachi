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
//!   hapi/tachi/openclaw/sigil/antigravity and missing fts5 on quant. Also
//!   reconciles `memories_symbolic_fts` trigram drift in the same pass.
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
//! - **R8** Conservative ephemeral recall-cache cleanup. Empty JSON turn shapes
//!   lack canonical producer provenance and are retained. Exact duplicates use
//!   `repair dedupe exact/apply`.
//! - **R9** Domain normalization/backfill for missing and legacy path-like values.
//! - **R10** Enrichment failure marker reset (explicit opt-in only).
//! - **R11** Plan C split-brain repair: merge a stale regular alias DB into
//!   the repo-local canonical DB, then replace the alias with a symlink.
//! - **R12** Memory hygiene backfill: promote legacy distill rows, backfill
//!   derived/graph provenance, and archive only low-risk raw rows already
//!   covered by distill or exact normalized duplicates.
//!
//! Exit codes: 0 clean (or successful dry-run with no findings), 1 if
//! repairs were found and not applied, 2 if any rule errored.

use std::path::{Path, PathBuf};

use rusqlite::{Connection, OpenFlags};

use crate::manifest::{DbEntry, Manifest};
use tachi_bootstrap::cli::{DedupeAction, QuarantineAction, RepairAction};

pub mod domain;
pub mod edges;
pub mod enrichment;
pub mod exact_dedupe;
pub mod fts;
pub mod integrity;
pub mod inventory;
pub mod jobs;
pub mod junk;
pub mod memory_hygiene;
pub mod plan_c;
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
/// even with conservative ephemeral-junk matching, it is a destructive cleanup rule
/// and must be opted in explicitly via `--rule R8`.
const DEFAULT_RULES: &[&str] = &["R5", "R1", "R2", "R3", "R4", "R7", "R9", "R11"];

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
pub enum RepairError {
    Sqlite(rusqlite::Error),
    Io(std::io::Error),
    Memory(memcore::MemoryError),
}

impl std::fmt::Display for RepairError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RepairError::Sqlite(e) => write!(f, "sqlite: {e}"),
            RepairError::Io(e) => write!(f, "io: {e}"),
            RepairError::Memory(e) => write!(f, "memcore: {e}"),
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
impl From<memcore::MemoryError> for RepairError {
    fn from(e: memcore::MemoryError) -> Self {
        RepairError::Memory(e)
    }
}

/// Opens a repair connection without initializing or migrating the DB while
/// registering the default-deny function required by persistent v23 guards.
pub(crate) fn open_repair_connection(path: impl AsRef<Path>) -> Result<Connection, RepairError> {
    let conn = Connection::open(path)?;
    memcore::db::ensure_reserved_reference_write_guard(&conn)?;
    Ok(conn)
}

/// Per-DB context handed to each rule.
pub struct DbContext {
    pub label: String,
    pub path: PathBuf,
    pub conn: Connection,
}

impl DbContext {
    pub fn open(entry: &DbEntry) -> Result<Self, RepairError> {
        let path = PathBuf::from(&entry.path);
        crate::path_utils::manifest_db_leaf_exists(entry).map_err(|error| {
            RepairError::Io(std::io::Error::new(std::io::ErrorKind::InvalidInput, error))
        })?;
        let conn = open_repair_connection(&path)?;
        // Match the rest of the codebase: prefer WAL & shorter busy timeout
        // for repair sessions running alongside a possibly-live daemon.
        let _ = conn.busy_timeout(std::time::Duration::from_secs(5));
        Ok(DbContext {
            label: inventory::label_for(entry),
            path,
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
    purge_failed: Option<u64>,
    app_home: &Path,
) -> Result<(), Box<dyn std::error::Error>> {
    // Register the FTS5 `simple` tokenizer + sqlite-vec auto-extensions once,
    // BEFORE any Connection::open. Required for any DB whose schema includes
    // `memories_fts` (i.e. every tachi-schema DB).
    if let Err(e) = memcore::db::enable_simple_auto_extension() {
        eprintln!("warning: simple tokenizer init failed: {e}");
    }
    memcore::db::register_sqlite_vec();

    // Subaction dispatch first.
    if let Some(act) = action {
        return match act {
            RepairAction::Dedupe { action } => match action {
                DedupeAction::Exact {
                    db,
                    output,
                    limit,
                    path_prefix,
                } => exact_dedupe::plan(&db, &output, limit, path_prefix.as_deref(), app_home),
                DedupeAction::Apply {
                    db,
                    plan,
                    yes,
                    receipt_out,
                } => exact_dedupe::apply(&db, &plan, yes, &receipt_out, app_home),
                DedupeAction::Restore { db, receipt, yes } => {
                    exact_dedupe::restore(&db, &receipt, yes, app_home)
                }
            },
            RepairAction::Quarantine { action } => run_quarantine(action, app_home, json_out).await,
            RepairAction::Vacuum { db, apply } => {
                vacuum::run_vacuum_cli(&db, apply, app_home, json_out).await
            }
            RepairAction::Fts { db, apply } => {
                run_fts_cli(&db, apply, app_home, json_out, no_backup).await
            }
            RepairAction::Report { json } => {
                run_repair_sweep(
                    db_filter,
                    rule_filter,
                    false,
                    no_backup,
                    json,
                    purge_failed,
                    app_home,
                )
                .await
            }
        };
    }

    run_repair_sweep(
        db_filter,
        rule_filter,
        apply,
        no_backup,
        json_out,
        purge_failed,
        app_home,
    )
    .await
}

async fn run_repair_sweep(
    db_filter: Option<String>,
    rule_filter: Vec<String>,
    apply: bool,
    no_backup: bool,
    json_out: bool,
    purge_failed: Option<u64>,
    app_home: &Path,
) -> Result<(), Box<dyn std::error::Error>> {
    let manifest_path =
        Manifest::default_path(&dirs::home_dir().unwrap_or_else(|| PathBuf::from(".")));
    // app_home is canonical; manifest lives at <app_home>/manifest.json.
    let manifest_path = if app_home.join("manifest.json").exists() {
        app_home.join("manifest.json")
    } else {
        manifest_path
    };
    let manifest = Manifest::load_or_empty(&manifest_path);
    let entries = inventory::select_dbs(&manifest, db_filter.as_deref())?;

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
                "R4" => Box::new(jobs::JobsPurge {
                    failed_days: purge_failed,
                    ..Default::default()
                }),
                "R5" => Box::new(integrity::IntegrityCheck),
                "R7" => Box::new(edges::OrphanRefs),
                "R8" => Box::new(junk::JunkCleanup),
                "R9" => Box::new(domain::DomainRepair),
                "R10" => Box::new(enrichment::EnrichmentFailureReset),
                "R11" => Box::new(plan_c::PlanCRepair {
                    backup_alias: !no_backup,
                }),
                "R12" => Box::new(memory_hygiene::MemoryHygiene),
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
    let _ = memcore::db::enable_simple_auto_extension();
    memcore::db::register_sqlite_vec();
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
    let entries = inventory::select_dbs(&manifest, Some(db))?;
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
