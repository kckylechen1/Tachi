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
//!   pattern, not `tidy --execute`'s whole-run abort (there is no reusable
//!   single global daemon lock scoped to an arbitrary library path; busy
//!   detection here is per-open, via
//!   [`memcore::db::sqlite_error_is_locked`]).

use std::error::Error;
use std::path::{Path, PathBuf};

use memcore::db::migrations::EXPECTED_SCHEMA_VERSION;
use memcore::{DbOpenContext, MemoryStore};
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
fn enumerate_known_libraries(global_db_path: &Path, project_db_path: Option<&Path>) -> Vec<Library> {
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
fn dedup_by_canonical_path(libs: Vec<Library>) -> Vec<Library> {
    let mut seen = std::collections::HashSet::new();
    let mut out = Vec::with_capacity(libs.len());
    for lib in libs {
        let key = std::fs::canonicalize(&lib.path).unwrap_or_else(|_| lib.path.clone());
        if seen.insert(key) {
            out.push(lib);
        }
    }
    out
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
    let path_str = lib.path.display().to_string();

    if !lib.path.exists() {
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

    match probe_schema_version(&lib.path) {
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

fn apply_one(lib: &Library, plan: MigrateFinding) -> MigrateFinding {
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

    let Some(path_str) = lib.path.to_str() else {
        finding.applied = Some(AppliedOutcome::Failed);
        finding.note = format!("{} (path is not valid UTF-8)", finding.note);
        return finding;
    };

    let ctx = DbOpenContext::open_existing_allow(MIGRATE_APPLY_APPROVED_BY);
    match MemoryStore::open_with_label_and_context(path_str, &lib.label, &ctx) {
        Ok(_store) => {
            let old_version_display = plan_stored_version_display(&finding);
            finding.stored_version = Some(EXPECTED_SCHEMA_VERSION);
            finding.applied = Some(AppliedOutcome::Migrated);
            finding.note =
                format!("migrated {old_version_display} -> {EXPECTED_SCHEMA_VERSION}");
        }
        Err(memcore::MemoryError::Sqlite(ref sqlite_err))
            if memcore::db::sqlite_error_is_locked(sqlite_err) =>
        {
            finding.applied = Some(AppliedOutcome::SkippedLocked);
            finding.note = "skipped: database busy/locked (likely held by a live daemon)"
                .to_string();
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
    global_db_path: &Path,
    project_db_path: Option<&Path>,
) -> Result<(), Box<dyn Error>> {
    let libraries = enumerate_known_libraries(global_db_path, project_db_path);

    let findings: Vec<MigrateFinding> = libraries
        .iter()
        .map(|lib| {
            let plan = plan_one(lib);
            if apply {
                apply_one(lib, plan)
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

    /// Build a fixture at a fully-current schema, then roll `PRAGMA
    /// user_version` back by `behind` versions to simulate "a real DB
    /// stamped older than this binary" (mirrors
    /// `bootstrap::backfill::tests::seed_and_stamp_older_schema_version`,
    /// the established pattern for this exact scenario across the #1181/#1119
    /// family of tests). The ticket's "v18 fixture" example is EXPECTED-2 at
    /// today's EXPECTED_SCHEMA_VERSION == 20; using an offset rather than the
    /// literal "18" keeps this test meaningful as EXPECTED_SCHEMA_VERSION
    /// bumps further.
    fn make_stamped_older_fixture(dir: &Path, name: &str, behind: u32) -> PathBuf {
        let db_path = dir.join(name);
        MemoryStore::open(db_path.to_str().expect("utf8 path")).expect("seed current-schema db");
        let conn = Connection::open(&db_path).expect("reopen to roll back stamp");
        conn.execute_batch(&format!(
            "PRAGMA user_version = {}",
            EXPECTED_SCHEMA_VERSION - behind
        ))
        .expect("stamp older schema version");
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
        crate::test_support::with_tachi_home(|_home| {
            let rt = tokio::runtime::Runtime::new().expect("tokio runtime");
            rt.block_on(run_migrate_command(json, apply, global_db_path, project_db_path))
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
}
