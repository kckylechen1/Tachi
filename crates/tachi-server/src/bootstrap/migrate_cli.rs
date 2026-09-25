//! `tachi migrate` (kckylechen1/tachi#1223) — a cross-library schema-version
//! sweep: enumerate every DB this binary knows how to address, report each
//! one's `PRAGMA user_version` gap against `EXPECTED_SCHEMA_VERSION`, and
//! (only under `--apply`) migrate exactly the ones that are behind.
//!
//! ## Trust-boundary discipline (read before touching this file)
//!
//! This is the #1119 signature-incident surface: an unauthorized process
//! silently forward-migrating a live DB a deployed daemon still depends on.
//! Everything here follows that redesign's rule verbatim:
//!
//! - Authorization is a **typed value constructed at this call site**
//!   ([`memcore::DbOpenContext::open_existing_allow`]), never a process env
//!   var, and never a reuse of the shared top-level `--allow-schema-migration`
//!   flag / `StartupContext.schema_migration` (that flag's `approved_by`
//!   string is asserted verbatim by other tests — see
//!   `bootstrap::manifest_cli`'s and `bootstrap::backfill`'s test modules).
//!   `--apply` on THIS subcommand is its own, independent authorization
//!   decision with its own provenance string.
//! - Plan-only mode never opens a DB for write. It reads `PRAGMA user_version`
//!   through the exact same zero-touch immutable-URI probe
//!   `doctor::schema_skew` already uses (`make_immutable_uri` +
//!   `memcore::db::open_immutable_readonly` + `read_schema_version`) — no
//!   WAL/SHM sidecar can be created by an immutable read-only SQLite open.
//! - `--apply` only ever constructs `MigrationAuthority::Allow` for a library
//!   this sweep has itself classified [`GapStatus::NeedsMigration`] (a real,
//!   stamped-older, existing DB). Libraries already at
//!   `EXPECTED_SCHEMA_VERSION` are reported as a no-op WITHOUT being
//!   reopened — there is nothing to authorize and no reason to touch them.
//!   Libraries stamped `0` ([`GapStatus::Unstamped`] — genuinely fresh, or a
//!   legacy pre-`user_version` file of unknown shape) are deliberately left
//!   untouched: this sweep's job is migrating existing Tachi schemas forward,
//!   not deciding whether an unstamped file should become one.
//! - A library another process (typically a live daemon) currently holds is
//!   skipped and listed, never treated as fatal for the rest of the sweep —
//!   mirrors `foundry_runtime_ops::daily_distill`'s "best-effort per target"
//!   pattern, not `tidy --execute`'s whole-run abort. Two layers, not one:
//!   **(1) a liveness pre-check** — before ANY authorized write-open,
//!   `--apply` asks [`crate::status_ops::collect_daemon_status`] (this app
//!   home's scoped-then-legacy daemon lock, the same reusable singleton the
//!   live `serve` daemon holds for its entire lifetime per
//!   `bootstrap::serve::daemon::serve_http_daemon` and `tachi status`
//!   already consult) whether a daemon is alive for this invocation's own
//!   `global_db_path`. If one is — `Running` or `Foreign`, either way a real
//!   process is holding that lock — every library is skipped
//!   `SkippedLocked` WITHOUT ever attempting the open: a live daemon's
//!   `FoundryScheduler` polls (and can write to) every manifest-listed DB, not
//!   only its own global one, so this one liveness check gates the global,
//!   workspace, and every named-project library alike. This closes the
//!   exact #1119 race a per-open-only BUSY check cannot: WAL +
//!   `busy_timeout=5000` (`memcore/src/db/schema/ddl.rs`) gives a live-but-
//!   momentarily-idle daemon no persistent write lock, so an external
//!   `Immediate` open between its writes would otherwise succeed. This
//!   check is re-run fresh inside [`apply_one`] itself, at each library's
//!   own apply attempt — NEVER cached once before the sweep's loop starts.
//!   A daemon can start (or a lock can be acquired) between this
//!   invocation's own start and a later library's apply; a snapshot taken
//!   once up front and reused across every `apply_one` call would let that
//!   later apply race straight past the guard on a now-stale "no daemon"
//!   reading (kckylechen1/tachi#1223, second review round — the original
//!   version of this fix made exactly that mistake).
//!   **(2) the per-open `SQLITE_BUSY` classification** (via
//!   [`memcore::db::sqlite_error_is_locked`]) remains as defense in depth for
//!   contention the liveness pre-check cannot see — e.g. a second concurrent
//!   `migrate --apply` invocation, or any other writer racing the open at the
//!   SQLite layer.

use std::error::Error;
use std::path::{Path, PathBuf};

use memcore::db::migrations::EXPECTED_SCHEMA_VERSION;
use memcore::path_router::UNKNOWN_DB_LABEL;
use memcore::{pending_legacy_sidecar, resolve_memory_db_read_path, DbOpenContext, MemoryStore};
use serde::Serialize;

use super::print_pretty_json;

/// One known library slot this sweep addresses.
struct Library {
    /// Human-facing identity: `"global"`, `"workspace"`, or
    /// `"project:<name>"` for a manifest-addressed named project.
    label: String,
    path: PathBuf,
}

/// Where a library stands relative to `EXPECTED_SCHEMA_VERSION`, as read by
/// the zero-touch plan-time probe.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
enum GapStatus {
    /// No file at this path — nothing to report or migrate.
    NotFound,
    /// The file exists but its `PRAGMA user_version` could not be read
    /// (corrupt, foreign schema, or transient I/O/lock error at probe time).
    Unreadable,
    /// `user_version == 0`: genuinely fresh, or a legacy pre-`user_version`
    /// file. Deliberately out of this sweep's scope (see module doc).
    Unstamped,
    /// `user_version == EXPECTED_SCHEMA_VERSION`: nothing to do.
    UpToDate,
    /// `0 < user_version < EXPECTED_SCHEMA_VERSION`: the sweep's actual
    /// target — `--apply` migrates exactly these.
    NeedsMigration,
    /// `user_version > EXPECTED_SCHEMA_VERSION`: this binary is older than
    /// the DB. Never touched (matches
    /// `memcore::db::migrations::check_schema_version_gate`'s hard refusal).
    AheadOfBinary,
}

/// What `--apply` actually did to a library. `None` in plan-only mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
enum AppliedOutcome {
    /// Already at `EXPECTED_SCHEMA_VERSION`; not reopened.
    NoOpAlreadyCurrent,
    /// Authorized open succeeded; `user_version` is now
    /// `EXPECTED_SCHEMA_VERSION`.
    Migrated,
    /// Another process holds this library; skipped, sweep continued.
    SkippedLocked,
    /// The authorized open itself failed for a reason other than a lock.
    Failed,
    /// Not attempted: out of `--apply`'s scope for this status
    /// (`NotFound` / `Unreadable` / `Unstamped` / `AheadOfBinary`).
    NotAttempted,
}

#[derive(Debug, Clone, Serialize)]
struct MigrateFinding {
    label: String,
    path: String,
    stored_version: Option<u32>,
    expected_version: u32,
    status: GapStatus,
    applied: Option<AppliedOutcome>,
    note: String,
}

#[derive(Debug, Serialize)]
struct MigrateReport {
    apply: bool,
    findings: Vec<MigrateFinding>,
}

/// Enumerate every library slot this sweep addresses: the global DB, the
/// current workspace's `.tachi/memory.db` (if this invocation resolved one —
/// mirrors every other CLI subcommand's `project_db_path`), and every
/// manifest-addressed named project resolved to its real backing file.
///
/// Named-project enumeration deliberately mirrors the ONLY two existing call
/// sites that walk this same list today
/// (`bootstrap::serve::background`'s WAL-checkpoint sweep and
/// `foundry_runtime_ops::daily_distill::runner`'s batch): list the
/// `~/.tachi/projects/<name>/` alias names, then resolve each one through
/// `MemoryServer::resolve_named_project_db_path` to its real repo-local (or
/// alias) file — never treat the alias directory itself as the data store
/// (`project_db_ops.rs`'s "alias, not a data store" doc comment). A name that
/// fails to resolve (stale manifest entry, broken alias) is tolerated
/// silently, matching `doctor`'s hub-capability lint collector.
fn enumerate_known_libraries(
    global_db_path: &Path,
    project_db_path: Option<&Path>,
) -> Vec<Library> {
    let mut libs = vec![Library {
        label: "global".to_string(),
        path: global_db_path.to_path_buf(),
    }];

    if let Some(p) = project_db_path {
        libs.push(Library {
            label: "workspace".to_string(),
            path: p.to_path_buf(),
        });
    }

    for name in crate::path_utils::list_named_projects() {
        if let Ok(path) = crate::server_state::MemoryServer::resolve_named_project_db_path(&name) {
            libs.push(Library {
                label: format!("project:{name}"),
                path,
            });
        }
    }

    dedup_by_canonical_path(libs)
}

/// The workspace `.tachi/memory.db` and a named-project alias can resolve to
/// the SAME physical file (a repo that is both "the current workspace" and a
/// registered named project). Report each physical library exactly once,
/// keeping whichever label listed it first (global > workspace > named
/// projects, in enumeration order) — reporting the same file twice under two
/// labels would double-count it in the summary and, under `--apply`, attempt
/// two redundant opens of the same path.
///
/// The dedup key is the file this slot would physically address after the
/// read-only #1132 filename resolution below. Distinct real files in a
/// split-brain directory must remain distinct findings.
fn dedup_by_canonical_path(libs: Vec<Library>) -> Vec<Library> {
    let mut seen = std::collections::HashSet::new();
    let mut out = Vec::with_capacity(libs.len());
    for lib in libs {
        if seen.insert(physical_dedup_key(&lib.path)) {
            out.push(lib);
        }
    }
    out
}

/// Dedup by the physical path the read-only #1132 resolver would probe.
/// `resolve_memory_db_read_path` only stats/reads link targets — it never
/// renames, creates, or relinks anything — so enumeration stays read-only.
/// On ambiguity, use the raw path: `plan_one` reports `Unreadable` rather
/// than silently selecting either real file.
fn physical_dedup_key(path: &Path) -> PathBuf {
    let resolved = resolve_memory_db_read_path(path).unwrap_or_else(|_| path.to_path_buf());
    std::fs::canonicalize(&resolved).unwrap_or(resolved)
}

/// Zero-touch `PRAGMA user_version` read: the exact probe
/// `doctor::schema_skew` uses (immutable-URI open, never a regular
/// read-write/read-only handle) so a plan-only pass over an existing DB can
/// never create a WAL/SHM sidecar or otherwise touch a single byte of it.
fn probe_schema_version(path: &Path) -> Result<u32, String> {
    let uri = crate::doctor::make_immutable_uri(path);
    let conn = memcore::db::open_immutable_readonly(&uri).map_err(|e| e.to_string())?;
    memcore::db::migrations::read_schema_version(&conn).map_err(|e| e.to_string())
}

fn plan_one(lib: &Library) -> MigrateFinding {
    // Read-only #1132-aware resolution: a slot whose canonical
    // `tachi-memory.db` file does not exist yet but whose legacy `memory.db`
    // sibling does is NOT "not found" — the sweep must see the real file the
    // explicit offline filename conversion must bring forward. This resolution never
    // renames/writes/creates anything; it applies the same split-brain and
    // compat-symlink rules as the write-side seam and FAILS LOUD on the
    // ambiguous both-real-files state, which is surfaced here as `Unreadable`
    // (never a silent NotFound/UpToDate on a conflicted directory). A path
    // whose filename is not the canonical one (custom DB paths, explicit
    // non-standard files) resolves to itself — no arbitrary filename rewrite
    // and no fallback for custom names.
    let probe_path = match resolve_memory_db_read_path(&lib.path) {
        Ok(path) => path,
        Err(err) => {
            return MigrateFinding {
                label: lib.label.clone(),
                path: lib.path.display().to_string(),
                stored_version: None,
                expected_version: EXPECTED_SCHEMA_VERSION,
                status: GapStatus::Unreadable,
                applied: None,
                note: format!("could not resolve this slot's database path read-only: {err}"),
            };
        }
    };
    let path_str = probe_path.display().to_string();
    let legacy_name_in_effect = probe_path != lib.path;

    // Immutable SQLite probes see only the main file, never pending committed
    // WAL frames or a hot rollback journal. Refuse a definitive version claim
    // on either the physical path or the legacy sibling (orphaned sidecar).
    let mut sidecar_paths = vec![probe_path.clone()];
    if lib.path.file_name().and_then(|name| name.to_str()) == Some(memcore::MEMORY_DB_FILENAME) {
        sidecar_paths.push(lib.path.with_file_name(memcore::LEGACY_MEMORY_DB_FILENAME));
    }
    for candidate in sidecar_paths {
        match pending_legacy_sidecar(&candidate) {
            Ok(Some(sidecar)) => return MigrateFinding {
                label: lib.label.clone(), path: path_str, stored_version: None,
                expected_version: EXPECTED_SCHEMA_VERSION, status: GapStatus::Unreadable,
                applied: None,
                note: format!("immutable plan cannot account for pending WAL/journal at {}; stop writers and use the explicit offline filename conversion before schema migration", sidecar.display()),
            },
            Err(err) => return MigrateFinding {
                label: lib.label.clone(), path: path_str, stored_version: None,
                expected_version: EXPECTED_SCHEMA_VERSION, status: GapStatus::Unreadable,
                applied: None, note: format!("cannot inspect SQLite sidecar state: {err}"),
            },
            Ok(None) => {}
        }
    }

    if !probe_path.exists() {
        return MigrateFinding {
            label: lib.label.clone(),
            path: path_str,
            stored_version: None,
            expected_version: EXPECTED_SCHEMA_VERSION,
            status: GapStatus::NotFound,
            applied: None,
            note: "no database file at this path".to_string(),
        };
    }

    match probe_schema_version(&probe_path) {
        Err(err) => MigrateFinding {
            label: lib.label.clone(),
            path: path_str,
            stored_version: None,
            expected_version: EXPECTED_SCHEMA_VERSION,
            status: GapStatus::Unreadable,
            applied: None,
            note: format!("could not read schema version: {err}"),
        },
        Ok(stored) => {
            let (status, note) = if stored == 0 {
                (
                    GapStatus::Unstamped,
                    "unstamped (PRAGMA user_version == 0): genuinely fresh, or a legacy \
                     pre-user_version file — out of this sweep's scope"
                        .to_string(),
                )
            } else if stored == EXPECTED_SCHEMA_VERSION {
                (GapStatus::UpToDate, "already current".to_string())
            } else if stored < EXPECTED_SCHEMA_VERSION {
                (
                    GapStatus::NeedsMigration,
                    format!("{stored} -> {EXPECTED_SCHEMA_VERSION} available"),
                )
            } else {
                (
                    GapStatus::AheadOfBinary,
                    format!(
                        "stamped {stored} is NEWER than this binary's {EXPECTED_SCHEMA_VERSION} \
                         — refused by check_schema_version_gate on any real open"
                    ),
                )
            };
            let note = if legacy_name_in_effect {
                format!(
                    "{note}; the legacy `memory.db` filename requires explicit offline \
                     conversion with `tachi migrate --rename-legacy --apply --offline` \
                     before a schema upgrade or ordinary open"
                )
            } else {
                note
            };
            MigrateFinding {
                label: lib.label.clone(),
                path: path_str,
                stored_version: Some(stored),
                expected_version: EXPECTED_SCHEMA_VERSION,
                status,
                applied: None,
                note,
            }
        }
    }
}

/// `--apply` provenance string for this subcommand's own explicit
/// authorization decision. Distinct from `serve.rs`'s
/// `"cli:--allow-schema-migration"` (a different flag, a different call
/// site) — never reuse that literal, several tests assert on it verbatim.
const MIGRATE_APPLY_APPROVED_BY: &str = "cli:migrate --apply";

/// True iff a live tachi daemon is holding the scoped-or-legacy singleton
/// lock for `app_home`/`global_db_path` right now. `Running` and `Foreign`
/// both carry a live, signalable PID (`Foreign` only differs in whether its
/// recorded identity matches this binary/global-db exactly) — either way a
/// real process holds the lock this app home's `serve` daemon holds for its
/// entire lifetime. `Unavailable` means the recorded owner cannot be ruled
/// out on this platform; it is also treated as "do not migrate out from under
/// it". `None`/`StalePid` mean no live process is attached to the lock.
fn a_live_daemon_holds_this_app_home(app_home: &Path, global_db_path: &Path) -> Option<i32> {
    match crate::status_ops::collect_daemon_status(app_home, global_db_path) {
        crate::status_ops::DaemonStatus::Running { pid, .. }
        | crate::status_ops::DaemonStatus::Foreign { pid, .. }
        | crate::status_ops::DaemonStatus::Unavailable { pid, .. } => Some(pid),
        crate::status_ops::DaemonStatus::StalePid { .. }
        | crate::status_ops::DaemonStatus::None => None,
    }
}

fn apply_one(
    lib: &Library,
    plan: MigrateFinding,
    app_home: &Path,
    global_db_path: &Path,
) -> MigrateFinding {
    let mut finding = plan;

    match finding.status {
        GapStatus::NotFound | GapStatus::Unreadable | GapStatus::AheadOfBinary => {
            finding.applied = Some(AppliedOutcome::NotAttempted);
            return finding;
        }
        GapStatus::Unstamped => {
            finding.applied = Some(AppliedOutcome::NotAttempted);
            finding.note = format!(
                "{} (not touched by --apply: see module doc comment)",
                finding.note
            );
            return finding;
        }
        GapStatus::UpToDate => {
            finding.applied = Some(AppliedOutcome::NoOpAlreadyCurrent);
            return finding;
        }
        GapStatus::NeedsMigration => {}
    }

    // #1119 liveness pre-check: refuse to even attempt the write-open while
    // this app home's daemon lock is held by a live process. Re-judged
    // RIGHT HERE, at this library's own apply attempt — not from a
    // snapshot computed once before the sweep's loop started (kckylechen1/
    // tachi#1223, second review round: reusing a pre-loop snapshot across
    // every `apply_one` call let a daemon that started mid-sweep race
    // straight past every subsequent library's guard). Per-open
    // SQLITE_BUSY classification below is not a substitute for this — WAL +
    // busy_timeout gives a live-but-momentarily-idle daemon no persistent
    // write lock, so an external Immediate open between its writes would
    // otherwise succeed and silently forward-migrate the DB out from under
    // it (the exact #1119 incident). See module doc.
    if let Some(pid) = a_live_daemon_holds_this_app_home(app_home, global_db_path) {
        finding.applied = Some(AppliedOutcome::SkippedLocked);
        #[cfg(unix)]
        {
            finding.note = format!(
                "skipped: a live tachi daemon (pid {pid}) holds this app home's daemon lock; \
                 migrating now risks the #1119 race — stop it first (`tachi daemon kill`) and re-run"
            );
        }
        #[cfg(not(unix))]
        {
            finding.note = format!(
                "skipped: daemon owner pid {pid} has unavailable liveness; \
                 safe migration cannot be established on this platform"
            );
        }
        return finding;
    }

    let Some(path_str) = lib.path.to_str() else {
        finding.applied = Some(AppliedOutcome::Failed);
        finding.note = format!("{} (path is not valid UTF-8)", finding.note);
        return finding;
    };

    let ctx = DbOpenContext::open_existing_allow(MIGRATE_APPLY_APPROVED_BY);
    // Enumeration labels (`project:<dirname>`) are inventory names, not
    // store-identity claims. Opening with that claim against a stamped
    // role (`wiki`, a Plan-C hash, a bare project name) is
    // `StoreRoleConflict` and rolls the authorized migration back — the
    // 2026-08-14 leftover that left wiki/Sigil/Quant at schema 28 after
    // global had already moved (#1761 / #1579). `unknown` confers nothing
    // and lets `resolve_role` keep the stamp.
    match MemoryStore::open_with_label_and_context(path_str, UNKNOWN_DB_LABEL, &ctx) {
        Ok(_store) => {
            let old_version_display = plan_stored_version_display(&finding);
            finding.stored_version = Some(EXPECTED_SCHEMA_VERSION);
            finding.applied = Some(AppliedOutcome::Migrated);
            // The write-side filename seam may have renamed `memory.db`;
            // report the path now holding the migrated store, not the
            // plan-time location that has become a compat symlink.
            finding.path = lib.path.display().to_string();
            finding.note = format!("migrated {old_version_display} -> {EXPECTED_SCHEMA_VERSION}");
        }
        Err(memcore::MemoryError::Sqlite(ref sqlite_err))
            if memcore::db::sqlite_error_is_locked(sqlite_err) =>
        {
            finding.applied = Some(AppliedOutcome::SkippedLocked);
            finding.note =
                "skipped: database busy/locked (likely held by a live daemon)".to_string();
        }
        Err(err) => {
            finding.applied = Some(AppliedOutcome::Failed);
            finding.note = format!("migration attempt failed: {err}");
        }
    }
    finding
}

fn plan_stored_version_display(finding: &MigrateFinding) -> String {
    finding
        .stored_version
        .map(|v| v.to_string())
        .unwrap_or_else(|| "?".to_string())
}

fn render_report(report: &MigrateReport) -> String {
    let mut out = String::new();
    out.push_str(if report.apply {
        "tachi migrate --apply\n"
    } else {
        "tachi migrate (plan-only)\n"
    });
    out.push_str(&format!(
        "{:<16} {:>8} {:>8}  {:<18} {}\n",
        "LIBRARY", "STORED", "EXPECT", "STATUS", "NOTE"
    ));
    for f in &report.findings {
        let stored = plan_stored_version_display(f);
        let status_col = if let Some(applied) = f.applied {
            format!("{:?}", applied)
        } else {
            format!("{:?}", f.status)
        };
        out.push_str(&format!(
            "{:<16} {:>8} {:>8}  {:<18} {}\n",
            f.label, stored, f.expected_version, status_col, f.note
        ));
    }
    out
}

pub(super) async fn run_migrate_command(
    json_output: bool,
    apply: bool,
    app_home: &Path,
    global_db_path: &Path,
    project_db_path: Option<&Path>,
) -> Result<(), Box<dyn Error>> {
    let libraries = enumerate_known_libraries(global_db_path, project_db_path);

    // No pre-loop liveness snapshot here: `apply_one` re-queries
    // `a_live_daemon_holds_this_app_home` itself, fresh, at each library's
    // own apply attempt. This app home has at most one daemon lock scoped
    // to `global_db_path`, and that daemon's `FoundryScheduler` polls every
    // manifest-listed library (global, workspace, and every named project),
    // not only its own global DB — so the same query correctly gates every
    // library, but it must be asked anew each time, not cached once before
    // this loop started. See module doc and `apply_one`.
    let findings: Vec<MigrateFinding> = libraries
        .iter()
        .map(|lib| {
            let plan = plan_one(lib);
            if apply {
                apply_one(lib, plan, app_home, global_db_path)
            } else {
                plan
            }
        })
        .collect();

    let report = MigrateReport { apply, findings };

    if json_output {
        print_pretty_json(&serde_json::to_value(&report)?)?;
    } else {
        print!("{}", render_report(&report));
    }

    Ok(())
}

#[derive(Debug, Serialize)]
struct FilenameFinding {
    label: String,
    source: String,
    canonical: String,
    phase: &'static str,
    outcome: &'static str,
    note: String,
}

#[derive(Debug, Serialize)]
struct FilenameReport {
    rename_legacy: bool,
    apply: bool,
    offline_attested: bool,
    findings: Vec<FilenameFinding>,
}

fn filename_plan(lib: &Library) -> FilenameFinding {
    let path = &lib.path;
    let legacy = path.with_file_name(memcore::LEGACY_MEMORY_DB_FILENAME);
    let mut finding = FilenameFinding {
        label: lib.label.clone(),
        source: legacy.display().to_string(),
        canonical: path.display().to_string(),
        phase: "blocked",
        outcome: "not_attempted",
        note: String::new(),
    };
    if path.file_name().and_then(|name| name.to_str()) != Some(memcore::MEMORY_DB_FILENAME) {
        finding.phase = "custom_path";
        finding.note = "caller-selected noncanonical filename: no rename".to_string();
        return finding;
    }
    let source_meta = std::fs::symlink_metadata(&legacy);
    let target_meta = std::fs::symlink_metadata(path);
    match (source_meta, target_meta) {
        (Ok(source), Err(err))
            if err.kind() == std::io::ErrorKind::NotFound && source.file_type().is_file() =>
        {
            finding.phase = "legacy_only";
            finding.note = "requires whole-store backup, explicit offline conversion, then separate schema migration".to_string();
        }
        (Ok(source), Ok(target))
            if source.file_type().is_file() && target.file_type().is_file() =>
        {
            finding.note = "both real files exist; manual reconciliation required".to_string();
        }
        (Ok(source), Ok(target))
            if source.file_type().is_symlink() && target.file_type().is_file() =>
        {
            match memcore::resolve_memory_db_read_path(path) {
                Ok(_) => finding.phase = "already_converted",
                Err(err) => finding.note = err.to_string(),
            }
        }
        (Err(err), Ok(target))
            if err.kind() == std::io::ErrorKind::NotFound && target.file_type().is_file() =>
        {
            finding.phase = "canonical_only";
            finding.note = "possible interrupted conversion; explicit offline verification needed before restoring compatibility link".to_string();
        }
        (Err(source_err), Err(target_err))
            if source_err.kind() == std::io::ErrorKind::NotFound
                && target_err.kind() == std::io::ErrorKind::NotFound =>
        {
            finding.phase = "missing";
            finding.note = "no store at either filename".to_string();
        }
        (source, target) => {
            finding.note = format!("uncertain source/target state: {source:?}; {target:?}")
        }
    }
    for candidate in [&legacy, path] {
        match pending_legacy_sidecar(candidate) {
            Ok(Some(sidecar)) => {
                finding.note = format!(
                    "{}; pending WAL/journal at {} (immutable plan is incomplete)",
                    finding.note,
                    sidecar.display()
                )
            }
            Err(err) => {
                finding.phase = "blocked";
                finding.note = format!("sidecar state unknown: {err}");
            }
            Ok(None) => {}
        }
    }
    finding
}

pub(super) async fn run_filename_conversion_command(
    json_output: bool,
    apply: bool,
    offline: bool,
    app_home: &Path,
    global_db_path: &Path,
    project_db_path: Option<&Path>,
) -> Result<(), Box<dyn Error>> {
    if apply != offline {
        return Err("filename conversion requires --rename-legacy --apply --offline together (or plan without either flag)".into());
    }
    let authority = apply.then(memcore::OfflineFilenameAuthority::operator_attestation);
    let mut findings = Vec::new();
    let mut incomplete = false;
    for lib in enumerate_known_libraries(global_db_path, project_db_path) {
        let mut finding = filename_plan(&lib);
        if apply
            && matches!(
                finding.phase,
                "legacy_only" | "canonical_only" | "already_converted"
            )
        {
            let probe = if finding.phase == "legacy_only" {
                lib.path.with_file_name(memcore::LEGACY_MEMORY_DB_FILENAME)
            } else {
                lib.path.clone()
            };
            let guard =
                if let Some(pid) = a_live_daemon_holds_this_app_home(app_home, global_db_path) {
                    Some(format!("known daemon (pid {pid}) still holds this home"))
                } else {
                    match crate::db_ownership::daemon_ownership(&probe) {
                        crate::db_ownership::DbOwnership::NotOwned => None,
                        crate::db_ownership::DbOwnership::Owned => {
                            Some("SQLite file is held by another process".to_string())
                        }
                        crate::db_ownership::DbOwnership::Unknown(reason) => {
                            Some(format!("ownership unknown: {reason}"))
                        }
                    }
                };
            if let Some(reason) = guard {
                finding.outcome = "blocked";
                finding.note = reason;
            } else {
                match memcore::convert_legacy_filename_offline(
                    &lib.path,
                    authority.as_ref().expect("apply has authority"),
                ) {
                    Ok(outcome) => {
                        finding.outcome = match outcome {
                            memcore::OfflineFilenameOutcome::Converted => "converted",
                            memcore::OfflineFilenameOutcome::RecoveredLink => "recovered_link",
                            memcore::OfflineFilenameOutcome::AlreadyConverted => {
                                "already_converted"
                            }
                        };
                        finding.note = "verified filename conversion only; run ordinary `tachi migrate --apply` separately for schema".to_string();
                    }
                    Err(err) => {
                        finding.outcome = "failed";
                        finding.note = err.to_string();
                    }
                }
            }
        }
        if apply
            && !matches!(finding.phase, "custom_path")
            && !matches!(
                finding.outcome,
                "converted" | "recovered_link" | "already_converted"
            )
        {
            incomplete = true;
        }
        findings.push(finding);
    }
    let report = FilenameReport {
        rename_legacy: true,
        apply,
        offline_attested: offline,
        findings,
    };
    if json_output {
        print_pretty_json(&serde_json::to_value(&report)?)?;
    } else {
        println!(
            "tachi migrate --rename-legacy: {}",
            serde_json::to_string_pretty(&report)?
        );
    }
    if incomplete {
        return Err("filename conversion incomplete: inspect per-store findings; no schema migration was attempted".into());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::Connection;
    use std::io::Read;

    fn read_bytes(path: &Path) -> Vec<u8> {
        let mut f = std::fs::File::open(path).expect("open fixture for byte read");
        let mut buf = Vec::new();
        f.read_to_end(&mut buf).expect("read fixture bytes");
        buf
    }

    fn read_user_version(path: &Path) -> u32 {
        let conn = Connection::open(path).expect("open for version read");
        memcore::db::migrations::read_schema_version(&conn).expect("read schema version")
    }

    fn read_role_stamp(path: &Path) -> String {
        let conn = Connection::open(path).expect("open for role stamp");
        let value_json: String = conn
            .query_row(
                "SELECT value_json FROM hard_state WHERE namespace = 'store_identity' AND key = 'role'",
                [],
                |row| row.get(0),
            )
            .expect("role stamp present");
        let parsed: serde_json::Value = serde_json::from_str(&value_json).expect("role stamp json");
        parsed
            .get("value")
            .and_then(serde_json::Value::as_str)
            .expect("role stamp value")
            .to_string()
    }

    /// Build a fixture at a fully-current schema, then roll `PRAGMA
    /// user_version` back by `behind` versions to simulate "a real DB
    /// stamped older than this binary" (mirrors
    /// `bootstrap::backfill::tests::seed_and_stamp_older_schema_version`,
    /// the established pattern for this exact scenario across the #1181/#1119
    /// family of tests). The ticket's "v18 fixture" example is EXPECTED-2 at
    /// today's EXPECTED_SCHEMA_VERSION == 20; using an offset rather than the
    /// literal "18" keeps this test meaningful as EXPECTED_SCHEMA_VERSION
    /// bumps further.
    ///
    /// The `MemoryStore::open` seed call above is a REAL schema init —
    /// `memcore::db::schema::remember_migration_fingerprint` runs
    /// unconditionally at the end of every `init_schema_with_label_mut`
    /// (schema.rs:67), independent of whether anything actually migrated, so
    /// it always leaves a `<db>.migration-marker` sibling behind as a side
    /// effect of merely seeding the fixture — before this helper ever rolls
    /// the stamp back, and long before any `migrate` CLI code runs. A "real
    /// DB stamped older than this binary" that no `--apply`/#1188 migration
    /// has ever touched should not carry that trail; remove it here so
    /// callers asserting on plan-only's zero-write contract (no NEW
    /// `.migration-marker`/`.migration-bak.*` sibling) are testing plan-only
    /// itself, not an artifact of this helper's own seeding mechanics.
    fn make_stamped_older_fixture(dir: &Path, name: &str, behind: u32) -> PathBuf {
        let db_path = dir.join(name);
        MemoryStore::open(db_path.to_str().expect("utf8 path")).expect("seed current-schema db");
        let conn = Connection::open(&db_path).expect("reopen to roll back stamp");
        conn.execute_batch(&format!(
            "PRAGMA user_version = {}",
            EXPECTED_SCHEMA_VERSION - behind
        ))
        .expect("stamp older schema version");
        drop(conn);
        let _ = std::fs::remove_file(format!("{}.migration-marker", db_path.display()));
        db_path
    }

    fn make_current_fixture(dir: &Path, name: &str) -> PathBuf {
        let db_path = dir.join(name);
        MemoryStore::open(db_path.to_str().expect("utf8 path")).expect("seed current-schema db");
        db_path
    }

    /// `run_migrate_command` calls `enumerate_known_libraries`, which walks
    /// `crate::path_utils::list_named_projects()` — resolved against
    /// `tachi_home()`, i.e. the REAL `~/.tachi/projects/` unless overridden.
    /// Every test that goes through `run_migrate_command` (as opposed to
    /// calling `plan_one`/`apply_one` directly on a hand-built `Library`)
    /// MUST isolate `TACHI_HOME` first — otherwise it would enumerate, and
    /// under `--apply` actually migrate, whatever real named-project DBs
    /// happen to exist on the machine running the test suite. This helper
    /// wraps that isolation plus the sync/async bridge
    /// `crate::test_support::with_tachi_home`'s doc comment documents
    /// (mirrors `foundry_runtime_ops::handlers::capture_session`'s
    /// established "`#[test]`, block_on from inside the closure" pattern —
    /// `with_tachi_home` is a plain sync closure that restores `TACHI_HOME`
    /// the instant it returns, so it is not `#[tokio::test]`-compatible
    /// directly).
    fn with_isolated_home_run_migrate(
        json: bool,
        apply: bool,
        global_db_path: &Path,
        project_db_path: Option<&Path>,
    ) -> Result<(), Box<dyn Error>> {
        crate::test_support::with_tachi_home(|home| {
            let rt = tokio::runtime::Runtime::new().expect("tokio runtime");
            rt.block_on(run_migrate_command(
                json,
                apply,
                home,
                global_db_path,
                project_db_path,
            ))
        })
    }

    #[test]
    fn plan_only_makes_zero_byte_changes_to_a_stamped_older_fixture() {
        let dir = tempfile::tempdir().expect("tmp");
        let db_path = make_stamped_older_fixture(dir.path(), "global.db", 2);
        let before = read_bytes(&db_path);
        let before_version = read_user_version(&db_path);
        assert_eq!(before_version, EXPECTED_SCHEMA_VERSION - 2);

        with_isolated_home_run_migrate(false, false, &db_path, None)
            .expect("plan-only migrate must not error");

        let after = read_bytes(&db_path);
        assert_eq!(
            before, after,
            "plan-only mode must make zero byte changes to the fixture"
        );
        assert_eq!(
            read_user_version(&db_path),
            before_version,
            "plan-only mode must not touch the schema-version stamp"
        );
        assert!(
            !crate::doctor::make_immutable_uri(&db_path).is_empty(),
            "sanity: uri helper reachable"
        );
        // No #1188 migration-bak/marker trail either — plan-only never opens
        // for write, so nothing should exist beyond the fixture file itself.
        // `.migration-bak.<ts>` carries a timestamp suffix
        // (`schema.rs::sibling_with_suffix`), so scan the directory rather
        // than probing one exact filename.
        let marker = format!("{}.migration-marker", db_path.display());
        assert!(
            !std::path::Path::new(&marker).exists(),
            "plan-only must not create a migration-marker sibling"
        );
        let parent = db_path.parent().expect("fixture has a parent dir");
        let has_backup = std::fs::read_dir(parent)
            .expect("read fixture dir")
            .flatten()
            .any(|e| e.file_name().to_string_lossy().contains("migration-bak"));
        assert!(
            !has_backup,
            "plan-only must not create any migration-bak sibling"
        );
    }

    #[test]
    fn apply_migrates_a_stamped_older_fixture_and_leaves_the_1188_trail() {
        let dir = tempfile::tempdir().expect("tmp");
        let db_path = make_stamped_older_fixture(dir.path(), "global.db", 2);
        assert_eq!(read_user_version(&db_path), EXPECTED_SCHEMA_VERSION - 2);

        with_isolated_home_run_migrate(false, true, &db_path, None)
            .expect("apply migrate must not error");

        assert_eq!(
            read_user_version(&db_path),
            EXPECTED_SCHEMA_VERSION,
            "--apply must re-stamp the fixture at the current schema version"
        );

        let marker = format!("{}.migration-marker", db_path.display());
        assert!(
            std::path::Path::new(&marker).exists(),
            "#1188 migration-marker trail must be left behind by an authorized migration"
        );

        let parent = db_path.parent().expect("fixture has a parent dir");
        let has_backup = std::fs::read_dir(parent)
            .expect("read fixture dir")
            .flatten()
            .any(|e| e.file_name().to_string_lossy().contains("migration-bak"));
        assert!(
            has_backup,
            "#1188 migration-bak trail must be left behind by an authorized migration"
        );
    }

    #[test]
    fn apply_migrates_when_enumeration_label_disagrees_with_store_stamp() {
        crate::test_support::with_tachi_home(|home| {
            let dir = tempfile::tempdir().expect("tmp");
            let db_path = dir.path().join("wiki.db");
            MemoryStore::open_with_label(db_path.to_str().expect("utf8"), "wiki")
                .expect("seed wiki-stamped store");
            {
                let conn = Connection::open(&db_path).expect("reopen to roll back stamp");
                conn.execute_batch(&format!(
                    "PRAGMA user_version = {}",
                    EXPECTED_SCHEMA_VERSION - 2
                ))
                .expect("stamp older schema version");
            }
            let _ = std::fs::remove_file(format!("{}.migration-marker", db_path.display()));
            assert_eq!(read_role_stamp(&db_path), "wiki");
            assert_eq!(read_user_version(&db_path), EXPECTED_SCHEMA_VERSION - 2);

            let lib = Library {
                label: "project:wiki".to_string(),
                path: db_path.clone(),
            };
            let plan = plan_one(&lib);
            assert_eq!(plan.status, GapStatus::NeedsMigration);
            // Acceptance #2: the pre-fix open-with-`lib.label` path must
            // StoreRoleConflict this fixture. Without that red, a future
            // revert to conferring the inventory name would still look green.
            match MemoryStore::open_with_label_and_context(
                db_path.to_str().expect("utf8"),
                &lib.label,
                &DbOpenContext::open_existing_allow(MIGRATE_APPLY_APPROVED_BY),
            ) {
                Err(memcore::MemoryError::StoreRoleConflict {
                    claimed, stored, ..
                }) => {
                    assert_eq!(claimed, "project:wiki");
                    assert_eq!(stored, "wiki");
                }
                Err(err) => panic!(
                    "pre-fix open-with-lib.label must StoreRoleConflict this fixture, got: {err}"
                ),
                Ok(_) => panic!(
                    "pre-fix open-with-lib.label must StoreRoleConflict this fixture, but the open succeeded"
                ),
            }
            assert_eq!(
                read_user_version(&db_path),
                EXPECTED_SCHEMA_VERSION - 2,
                "the conflicting open must roll back and leave the leftover stamp"
            );
            let finding = apply_one(&lib, plan, home, &db_path);
            assert_eq!(
                finding.applied,
                Some(AppliedOutcome::Migrated),
                "enumeration label must not StoreRoleConflict a stamped wiki store: {finding:?}"
            );
            assert_eq!(read_user_version(&db_path), EXPECTED_SCHEMA_VERSION);
            assert_eq!(
                read_role_stamp(&db_path),
                "wiki",
                "migrate must not rewrite the write-once role stamp"
            );
        });
    }

    #[test]
    fn apply_reports_no_op_for_an_already_current_library() {
        let dir = tempfile::tempdir().expect("tmp");
        let db_path = make_current_fixture(dir.path(), "global.db");
        assert_eq!(read_user_version(&db_path), EXPECTED_SCHEMA_VERSION);

        with_isolated_home_run_migrate(false, true, &db_path, None)
            .expect("apply migrate must not error");

        assert_eq!(
            read_user_version(&db_path),
            EXPECTED_SCHEMA_VERSION,
            "an already-current library must remain at the current version"
        );
    }

    #[test]
    fn plan_one_classifies_every_gap_status() {
        let dir = tempfile::tempdir().expect("tmp");

        let missing = Library {
            label: "global".to_string(),
            path: dir.path().join("does-not-exist.db"),
        };
        assert_eq!(plan_one(&missing).status, GapStatus::NotFound);

        let behind = Library {
            label: "global".to_string(),
            path: make_stamped_older_fixture(dir.path(), "behind.db", 1),
        };
        assert_eq!(plan_one(&behind).status, GapStatus::NeedsMigration);

        let current = Library {
            label: "global".to_string(),
            path: make_current_fixture(dir.path(), "current.db"),
        };
        assert_eq!(plan_one(&current).status, GapStatus::UpToDate);

        let ahead_path = dir.path().join("ahead.db");
        MemoryStore::open(ahead_path.to_str().expect("utf8")).expect("seed");
        {
            let conn = Connection::open(&ahead_path).expect("reopen");
            conn.execute_batch(&format!(
                "PRAGMA user_version = {}",
                EXPECTED_SCHEMA_VERSION + 1
            ))
            .expect("stamp ahead");
        }
        let ahead = Library {
            label: "global".to_string(),
            path: ahead_path,
        };
        assert_eq!(plan_one(&ahead).status, GapStatus::AheadOfBinary);

        let unstamped_path = dir.path().join("unstamped.db");
        // A bare 0-byte file: SQLite treats this as a valid, empty,
        // never-initialized database, and `PRAGMA user_version` reads back 0
        // without any Tachi init having run. Created directly via
        // `std::fs::File` (not `Connection::open`+drop) because SQLite
        // defers actually materializing a database file on disk until its
        // first real write — an open-then-drop with no statement executed
        // is not guaranteed to leave a file behind at all.
        std::fs::File::create(&unstamped_path).expect("create empty sqlite file");
        let unstamped = Library {
            label: "global".to_string(),
            path: unstamped_path,
        };
        assert_eq!(plan_one(&unstamped).status, GapStatus::Unstamped);
    }

    /// Discriminative test for the "skip a library another process holds"
    /// requirement: hold a real `BEGIN EXCLUSIVE` transaction open on a
    /// stamped-older fixture from a separate connection (same in-process
    /// mechanism `memcore`'s own busy-retry tests use to simulate contention
    /// from a live daemon), then run `--apply` against it. `--apply` must
    /// classify the resulting SQLITE_BUSY as `SkippedLocked` and return `Ok`
    /// (continue the sweep) rather than propagating the error — and must
    /// leave the fixture's stamp untouched, since the migration never
    /// actually committed.
    ///
    /// This blocks for up to memcore's 5s connection `busy_timeout` before
    /// the lock holder is dropped and SQLite reports BUSY — the exact
    /// mechanism `MemoryStore::open_with_label_and_context` uses in
    /// production, not a shortened test-only timeout.
    #[test]
    fn apply_skips_a_library_locked_by_another_connection() {
        let dir = tempfile::tempdir().expect("tmp");
        let db_path = make_stamped_older_fixture(dir.path(), "global.db", 2);
        let before_version = read_user_version(&db_path);

        let lock_holder = Connection::open(&db_path).expect("open lock-holder connection");
        lock_holder
            .execute_batch("BEGIN EXCLUSIVE;")
            .expect("hold an exclusive lock");

        with_isolated_home_run_migrate(false, true, &db_path, None)
            .expect("a locked library must not abort the whole sweep");

        drop(lock_holder);

        assert_eq!(
            read_user_version(&db_path),
            before_version,
            "a migration that never committed (lock held throughout) must leave the stamp untouched"
        );
    }

    /// Discriminative test for the #1119 liveness pre-check (this review's
    /// fix): a live daemon holding this app home's scoped lock must block
    /// `--apply` BEFORE any write-open is even attempted — not merely be
    /// caught after the fact via `SQLITE_BUSY` like the previous test.
    /// Proven two ways: (1) `applied == SkippedLocked` and the stamp is
    /// untouched (same outward contract as the BUSY-locked case above), AND
    /// (2) no `-wal`/`-shm` sidecar exists afterward — `CONNECTION_PRAGMA_SQL`
    /// sets `journal_mode = WAL` on ANY real open, authorized or not, so a
    /// sidecar appearing here would prove a real open was attempted and only
    /// the after-the-fact BUSY catch (not this liveness pre-check) saved the
    /// stamp.
    #[test]
    fn apply_refuses_when_this_app_homes_daemon_lock_is_held_by_a_live_process() {
        crate::test_support::with_tachi_home(|home| {
            let dir = tempfile::tempdir().expect("tmp");
            let db_path = make_stamped_older_fixture(dir.path(), "global.db", 2);
            let before_version = read_user_version(&db_path);

            // Simulate "a live tachi daemon is running for this app home"
            // exactly the way `serve_http_daemon` does: hold the scoped
            // singleton lock. `DaemonLock::acquire` stamps the CURRENT
            // process's own pid, which `process_alive` finds alive — this
            // test process stands in for the daemon.
            let lock_path = crate::daemon_lock::scoped_daemon_lock_path(home, &db_path);
            let _daemon_lock = crate::daemon_lock::DaemonLock::acquire(&lock_path)
                .expect("acquire scoped daemon lock to simulate a live daemon");

            let rt = tokio::runtime::Runtime::new().expect("tokio runtime");
            rt.block_on(run_migrate_command(false, true, home, &db_path, None))
                .expect("a live-daemon skip must not abort the whole sweep");

            assert_eq!(
                read_user_version(&db_path),
                before_version,
                "a library skipped by the liveness pre-check must be left completely untouched"
            );

            let wal = format!("{}-wal", db_path.display());
            let shm = format!("{}-shm", db_path.display());
            assert!(
                !std::path::Path::new(&wal).exists() && !std::path::Path::new(&shm).exists(),
                "the liveness pre-check must refuse BEFORE any real open — a WAL/SHM sidecar \
                 appearing here would mean the guard was bypassed and only the after-the-fact \
                 SQLITE_BUSY catch saved this test"
            );
        });
    }

    #[cfg(not(unix))]
    #[test]
    fn platform_refusal_apply_preserves_db_for_unobservable_daemon_owner() {
        crate::test_support::with_tachi_home(|home| {
            let dir = tempfile::tempdir().expect("tmp");
            let db_path = make_stamped_older_fixture(dir.path(), "global.db", 2);
            let before = std::fs::read(&db_path).expect("read fixture before apply");
            let lock_path = crate::daemon_lock::scoped_daemon_lock_path(home, &db_path);
            std::fs::write(&lock_path, "2\n").expect("seed unobservable owner receipt");
            let legacy_lock = crate::daemon_lock::legacy_daemon_lock_path(home);
            std::fs::write(&legacy_lock, "1\n").expect("seed stale legacy receipt");

            let lib = Library {
                label: "global".to_string(),
                path: db_path.clone(),
            };
            let plan = plan_one(&lib);
            assert_eq!(plan.status, GapStatus::NeedsMigration);
            let result = apply_one(&lib, plan, home, &db_path);

            assert_eq!(result.applied, Some(AppliedOutcome::SkippedLocked));
            assert_eq!(std::fs::read(&db_path).unwrap(), before);
            assert_eq!(std::fs::read(&lock_path).unwrap(), b"2\n");
            assert_eq!(std::fs::read(legacy_lock).unwrap(), b"1\n");
        });
    }

    /// Discriminative test for this review's own fix (kckylechen1/tachi#1223,
    /// second round): a daemon that starts AFTER the sweep's own plan-time
    /// probe but BEFORE a given library's apply attempt must still be
    /// caught, even though that earlier plan pass saw no daemon at all.
    ///
    /// The version of this file this round is fixing computed
    /// `live_daemon_pid` exactly once — before `run_migrate_command`'s loop
    /// over libraries started — and threaded that one `Option<i32>`
    /// snapshot into every `apply_one` call. A daemon that appeared partway
    /// through a multi-library sweep would sail straight past every
    /// subsequent library's guard on that stale `None`. This test proves
    /// `apply_one` no longer trusts a caller-supplied snapshot at all: it
    /// re-queries the daemon's liveness itself, at its own call site, from
    /// `app_home`/`global_db_path` alone — so a daemon that appears strictly
    /// between the precheck and the apply attempt is still caught. Against
    /// this file's prior `apply_one(&Library, MigrateFinding, Option<i32>)`
    /// signature this test does not even compile (the fix's whole point is
    /// that `apply_one` must stop accepting a pre-computed liveness value as
    /// an argument) — the mismatch itself is the regression this test
    /// pins down.
    #[test]
    fn apply_one_catches_a_daemon_that_appears_after_the_precheck_but_before_its_own_apply() {
        crate::test_support::with_tachi_home(|home| {
            let dir = tempfile::tempdir().expect("tmp");
            let db_path = make_stamped_older_fixture(dir.path(), "global.db", 2);
            let before_version = read_user_version(&db_path);

            let lib = Library {
                label: "global".to_string(),
                path: db_path.clone(),
            };

            // Step 1: the sweep's own precheck/plan pass, run while no
            // daemon lock exists at all — matches `run_migrate_command`'s
            // own `plan_one` call for this library.
            let plan = plan_one(&lib);
            assert_eq!(plan.status, GapStatus::NeedsMigration);
            assert_eq!(
                a_live_daemon_holds_this_app_home(home, &db_path),
                None,
                "sanity: no daemon lock exists yet at precheck time"
            );

            // Step 2: a daemon appears — strictly AFTER the precheck above,
            // strictly BEFORE the apply attempt below. `DaemonLock::acquire`
            // stamps the CURRENT process's own pid, which `process_alive`
            // finds alive — this test process stands in for the daemon,
            // exactly as `apply_refuses_when_this_app_homes_daemon_lock_is_held_by_a_live_process`
            // does above.
            let lock_path = crate::daemon_lock::scoped_daemon_lock_path(home, &db_path);
            let _daemon_lock = crate::daemon_lock::DaemonLock::acquire(&lock_path)
                .expect("acquire scoped daemon lock to simulate a daemon starting mid-sweep");

            // Step 3: apply this library using the plan computed back in
            // step 1 (mirrors `run_migrate_command` reusing `plan_one`'s
            // result) — `apply_one` must see the daemon NOW, at its own
            // call site, not the stale "no daemon" reading from step 1.
            let result = apply_one(&lib, plan, home, &db_path);

            assert_eq!(
                result.applied,
                Some(AppliedOutcome::SkippedLocked),
                "a daemon that appears after the precheck but before this library's own apply \
                 attempt must still be caught — re-judged at apply_one's own call site, not a \
                 precheck snapshot"
            );
            assert_eq!(
                read_user_version(&db_path),
                before_version,
                "a library skipped by the re-judged liveness check must be left completely \
                 untouched"
            );

            let wal = format!("{}-wal", db_path.display());
            let shm = format!("{}-shm", db_path.display());
            assert!(
                !std::path::Path::new(&wal).exists() && !std::path::Path::new(&shm).exists(),
                "the re-judged liveness check must refuse BEFORE any real open — a WAL/SHM \
                 sidecar appearing here would mean the guard was bypassed"
            );
        });
    }

    // ------------------------------------------------------------------
    // Legacy `memory.db` filename visibility (this release's repair): a
    // library whose canonical `tachi-memory.db` does not exist yet but whose
    // pre-#1132 `memory.db` sibling does is a REAL library the sweep must
    // see — ordinary `MemoryStore::open` refuses until explicit offline
    // filename conversion, which plan mode never performs. These tests use the established
    // modern-schema-rolled-back fixture (`make_stamped_older_fixture`), NOT a
    // synthesized schema-28 file: a true v1.9.2 (schema 28) fixture must be
    // generated by the owner's v1.9.2 official release. Its separate
    // whole-directory WAL copy rehearsal is recorded in the task receipt.
    // ------------------------------------------------------------------

    /// Default-global shape: directory holds ONLY the legacy `memory.db`,
    /// stamped older. Plan must report `NeedsMigration` against the legacy
    /// file (not `NotFound`), and a full plan-only run must leave every byte
    /// and filename exactly as it was: no rename, no symlink, no sidecars.
    #[test]
    fn plan_reports_a_legacy_named_global_library_instead_of_not_found() {
        let dir = tempfile::tempdir().expect("tmp");
        let legacy_path = make_stamped_older_fixture(dir.path(), "memory.db", 2);
        let canonical_path = dir.path().join("tachi-memory.db");
        assert!(!canonical_path.exists());
        let before = read_bytes(&legacy_path);

        let lib = Library {
            label: "global".to_string(),
            path: canonical_path.clone(),
        };
        let plan = plan_one(&lib);
        assert_eq!(
            plan.status,
            GapStatus::NeedsMigration,
            "a legacy-named stamped-older library must be visible to the plan"
        );
        assert_eq!(
            plan.path,
            legacy_path.display().to_string(),
            "the finding must name the physical file that was probed"
        );
        assert!(
            plan.note.contains("#1132 rename"),
            "the finding must explain the legacy filename is still in effect: {note}",
            note = plan.note
        );

        with_isolated_home_run_migrate(false, false, &canonical_path, None)
            .expect("plan-only migrate must not error");

        assert_eq!(
            read_bytes(&legacy_path),
            before,
            "plan-only must make zero byte changes to the legacy-named fixture"
        );
        assert!(
            !canonical_path.exists(),
            "plan-only must not perform the #1132 rename"
        );
        assert!(
            std::fs::symlink_metadata(&legacy_path)
                .expect("legacy metadata")
                .file_type()
                .is_file(),
            "plan-only must not convert the legacy file into a compat symlink"
        );
        let parent = dir.path();
        let unexpected: Vec<String> = std::fs::read_dir(parent)
            .expect("read fixture dir")
            .flatten()
            .map(|e| e.file_name().to_string_lossy().to_string())
            .filter(|name| {
                name.contains("-wal")
                    || name.contains("-shm")
                    || name.contains("migration-marker")
                    || name.contains("migration-bak")
            })
            .collect();
        assert!(
            unexpected.is_empty(),
            "plan-only must not leave sidecars or migration trails: {unexpected:?}"
        );
    }

    /// Workspace shape: the current workspace's `.tachi/tachi-memory.db` slot
    /// resolved against a directory still carrying only the legacy name.
    #[test]
    fn plan_reports_a_legacy_named_workspace_library_instead_of_not_found() {
        let dir = tempfile::tempdir().expect("tmp");
        let tachi_dir = dir.path().join(".tachi");
        std::fs::create_dir(&tachi_dir).expect("mkdir .tachi");
        let legacy_path = make_stamped_older_fixture(&tachi_dir, "memory.db", 1);
        let canonical_path = tachi_dir.join("tachi-memory.db");

        let lib = Library {
            label: "workspace".to_string(),
            path: canonical_path,
        };
        let plan = plan_one(&lib);
        assert_eq!(plan.status, GapStatus::NeedsMigration);
        assert_eq!(plan.path, legacy_path.display().to_string());
    }

    #[test]
    fn immutable_plan_refuses_to_call_a_pending_legacy_wal_up_to_date() {
        let dir = tempfile::tempdir().expect("tmp");
        let old = make_current_fixture(dir.path(), "memory.db");
        let canonical = dir.path().join("tachi-memory.db");
        let wal = dir.path().join("memory.db-wal");
        std::fs::write(&wal, b"pending WAL: plan must not ignore this file").unwrap();
        let old_before = read_bytes(&old);
        let wal_before = read_bytes(&wal);
        let lib = Library {
            label: "global".to_string(),
            path: canonical.clone(),
        };
        let plan = plan_one(&lib);
        assert_eq!(plan.status, GapStatus::Unreadable);
        assert!(plan
            .note
            .contains("immutable plan cannot account for pending WAL/journal"));
        let filename_plan = filename_plan(&lib);
        assert_eq!(filename_plan.phase, "legacy_only");
        assert!(filename_plan.note.contains("immutable plan is incomplete"));
        assert_eq!(read_bytes(&old), old_before);
        assert_eq!(read_bytes(&wal), wal_before);
        assert!(!canonical.exists());
    }

    /// Conflict semantics: BOTH a real canonical file and a real legacy file
    /// in one directory. The write-side seam refuses this state loudly on
    /// open; the plan must not paper over it as `UpToDate`/`NotFound` — it
    /// surfaces the same refusal as `Unreadable`, touching nothing.
    #[test]
    fn plan_surfaces_the_both_real_files_conflict_loudly() {
        let dir = tempfile::tempdir().expect("tmp");
        let canonical_path = make_current_fixture(dir.path(), "tachi-memory.db");
        let legacy_path = dir.path().join("memory.db");
        std::fs::write(&legacy_path, b"stale-but-real legacy bytes").expect("write legacy");
        let canonical_before = read_bytes(&canonical_path);
        let legacy_before = read_bytes(&legacy_path);

        let lib = Library {
            label: "global".to_string(),
            path: canonical_path.clone(),
        };
        let plan = plan_one(&lib);
        assert_eq!(plan.status, GapStatus::Unreadable);
        assert!(
            plan.note.contains("both a canonical"),
            "the conflict note must name the both-real-files ambiguity: {note}",
            note = plan.note
        );
        assert_eq!(read_bytes(&canonical_path), canonical_before);
        assert_eq!(read_bytes(&legacy_path), legacy_before);
    }

    /// The healthy migrated shape: canonical file present, legacy name a
    /// compat symlink to it. Resolution follows the seam's rules and probes
    /// the canonical file once — `UpToDate`, no conflict, no legacy note.
    #[cfg(unix)]
    #[test]
    fn plan_follows_the_compat_symlink_to_the_canonical_file() {
        let dir = tempfile::tempdir().expect("tmp");
        let canonical_path = make_current_fixture(dir.path(), "tachi-memory.db");
        let legacy_path = dir.path().join("memory.db");
        std::os::unix::fs::symlink("tachi-memory.db", &legacy_path).expect("plant compat link");

        let lib = Library {
            label: "global".to_string(),
            path: canonical_path.clone(),
        };
        let plan = plan_one(&lib);
        assert_eq!(plan.status, GapStatus::UpToDate);
        assert_eq!(plan.path, canonical_path.display().to_string());
        assert!(
            !plan.note.contains("#1132 rename"),
            "a canonical-named finding must not carry the legacy-name note: {note}",
            note = plan.note
        );
    }

    /// Filename conversion and schema upgrade have separate authorities.
    /// Ordinary `migrate --apply` cannot rename even with schema authority;
    /// offline conversion preserves rows, then schema apply upgrades them.
    #[cfg(unix)]
    #[test]
    fn apply_migrates_a_legacy_named_library_preserving_rows() {
        let dir = tempfile::tempdir().expect("tmp");
        let legacy_path = make_current_fixture(dir.path(), "memory.db");
        let canonical_path = dir.path().join("tachi-memory.db");
        {
            let mut store = MemoryStore::open(legacy_path.to_str().expect("utf8 path"))
                .expect("open fixture through production store");
            let entry = memcore::MemoryEntry {
                id: "legacy-row-preservation-probe".to_string(),
                path: "/facts/release-upgrade".to_string(),
                summary: "Upgrade preservation probe".to_string(),
                text: "row that must survive the rename and schema migration".to_string(),
                importance: 0.8,
                timestamp: chrono::Utc::now().to_rfc3339(),
                valid_from: String::new(),
                valid_until: None,
                category: "fact".to_string(),
                topic: "upgrade".to_string(),
                keywords: vec![],
                persons: vec![],
                entities: vec![],
                location: String::new(),
                source: "test".to_string(),
                scope: "project".to_string(),
                archived: false,
                access_count: 0,
                scored_count: 0,
                last_access: None,
                last_use_at: None,
                revision: 1,
                metadata: serde_json::json!({}),
                vector: None,
                retention_policy: Some("durable".to_string()),
                domain: Some("coding".to_string()),
                recall_count: 0,
                query_diversity: 0,
                tier: "raw".to_string(),
            };
            store.upsert(&entry).expect("insert through guarded store");
        }
        // Like the existing migration fixtures, simulate an older stamp on a
        // current-schema DB; this is not a genuine v1.9.2 schema-28 file.
        let conn = Connection::open(&legacy_path).expect("open for version stamp");
        conn.execute_batch(&format!(
            "PRAGMA user_version = {}",
            EXPECTED_SCHEMA_VERSION - 2
        ))
        .expect("roll back version stamp for CLI migration");
        drop(conn);
        assert_eq!(read_user_version(&legacy_path), EXPECTED_SCHEMA_VERSION - 2);

        let ordinary = MemoryStore::open_with_label_and_context(
            canonical_path.to_str().unwrap(),
            UNKNOWN_DB_LABEL,
            &DbOpenContext::open_existing_allow(MIGRATE_APPLY_APPROVED_BY),
        );
        match ordinary {
            Err(err) => assert!(err
                .to_string()
                .contains("offline filename conversion required")),
            Ok(_) => panic!("ordinary schema-authorized open must refuse filename conversion"),
        }
        assert!(!canonical_path.exists());
        assert_eq!(read_user_version(&legacy_path), EXPECTED_SCHEMA_VERSION - 2);

        crate::test_support::with_tachi_home(|home| {
            let rt = tokio::runtime::Runtime::new().expect("tokio runtime");
            rt.block_on(run_filename_conversion_command(
                true,
                true,
                true,
                home,
                &canonical_path,
                None,
            ))
            .expect("offline filename conversion must succeed");
        });
        assert_eq!(
            read_user_version(&canonical_path),
            EXPECTED_SCHEMA_VERSION - 2,
            "filename conversion itself does not upgrade schema"
        );
        with_isolated_home_run_migrate(false, true, &canonical_path, None)
            .expect("subsequent schema apply must not error");

        assert!(
            canonical_path.exists(),
            "the authorized apply must converge the canonical filename"
        );
        assert_eq!(
            read_user_version(&canonical_path),
            EXPECTED_SCHEMA_VERSION,
            "the legacy-named library must be migrated to the current schema"
        );
        assert!(
            std::fs::symlink_metadata(&legacy_path)
                .expect("legacy metadata after apply")
                .file_type()
                .is_symlink(),
            "the #1132 compat symlink must be left at the legacy name"
        );
        assert_eq!(
            std::fs::read_link(&legacy_path).expect("compat link target"),
            PathBuf::from("tachi-memory.db"),
            "the compat symlink must point at the canonical sibling"
        );
        {
            let conn = Connection::open(&canonical_path).expect("open canonical after apply");
            let count: i64 = conn
                .query_row(
                    "SELECT COUNT(*) FROM memories WHERE id = 'legacy-row-preservation-probe'",
                    [],
                    |row| row.get(0),
                )
                .expect("count probe rows");
            assert_eq!(count, 1, "rows must survive the rename + schema migration");
        }
        let final_finding = plan_one(&Library {
            label: "global".to_string(),
            path: canonical_path.clone(),
        });
        assert_eq!(final_finding.status, GapStatus::UpToDate);
        assert_eq!(final_finding.path, canonical_path.display().to_string());
    }

    /// Canonical and explicit legacy paths to one physical file dedup in
    /// either order. Separate real files in a conflict stay separate, so the
    /// canonical slot reports `Unreadable` instead of hiding the ambiguity.
    #[test]
    fn dedup_collapses_slots_sharing_one_physical_legacy_file() {
        let dir = tempfile::tempdir().expect("tmp");
        let canonical = dir.path().join("tachi-memory.db");
        let legacy = make_stamped_older_fixture(dir.path(), "memory.db", 1);

        for (first, second) in [(&canonical, &legacy), (&legacy, &canonical)] {
            let libs = vec![
                Library {
                    label: "first".to_string(),
                    path: first.clone(),
                },
                Library {
                    label: "second".to_string(),
                    path: second.clone(),
                },
            ];
            let deduped = dedup_by_canonical_path(libs);
            assert_eq!(deduped.len(), 1, "both names address the same legacy file");
            assert_eq!(deduped[0].label, "first");
        }
        assert!(!canonical.exists(), "dedup must not rename the legacy file");
        assert!(std::fs::symlink_metadata(&legacy)
            .unwrap()
            .file_type()
            .is_file());

        // Plant the second real file without opening the guarded filename
        // seam: this is exactly the conflicting on-disk state it must refuse.
        std::fs::write(&canonical, b"separate real canonical bytes").expect("plant conflict");
        let libs = vec![
            Library {
                label: "workspace".to_string(),
                path: canonical,
            },
            Library {
                label: "project:legacy".to_string(),
                path: legacy.clone(),
            },
        ];
        let deduped = dedup_by_canonical_path(libs);
        assert_eq!(deduped.len(), 2, "distinct real files must not collapse");
        assert_eq!(plan_one(&deduped[0]).status, GapStatus::Unreadable);
        assert!(std::fs::symlink_metadata(&legacy)
            .unwrap()
            .file_type()
            .is_file());
    }
}
