use super::{print_pretty_json, DEFAULT_STANDARD_PROFILE_NOTICE};
use crate::kanban::{gc_expired_kanban_cards, DEFAULT_KANBAN_GC_MAX_AGE_DAYS};
use crate::mcp_proxy::filter_mcp_tools_by_permissions;
use crate::server_state::MemoryServer;
use crate::utils::{find_project_git_root, lock_or_recover, parse_env_bool, parse_env_u64};
use chrono::Utc;
use memcore::MemoryStore;
use serde_json::json;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};
use tachi_bootstrap::cli::{Cli, Commands};
use tachi_hub::should_expose_skill_tool;

mod backfill_commands;
mod background;
mod cli_commands;
mod daemon;
mod logging;
mod malformed_json_middleware;
mod runtime;
mod stdio;

use self::backfill_commands::run_if_backfill_command;
use self::background::*;
use self::cli_commands::run_pre_serve_command;
use self::daemon::serve_http_daemon;
use self::logging::*;
use self::runtime::*;
use self::stdio::serve_stdio;

fn no_project_serve_detaches_launch_cwd(command: &Commands, no_project_db: bool) -> bool {
    no_project_db && matches!(command, Commands::Serve)
}

fn should_load_project_local_env(command: &Commands, no_project_db: bool) -> bool {
    !no_project_db && !matches!(command, Commands::Vault { .. })
}

fn should_defer_manifest_startup(command: &Commands, daemon: bool, no_project_db: bool) -> bool {
    daemon && no_project_db && matches!(command, Commands::Serve)
}

fn should_refresh_plan_c_symlink(
    project_db_path: Option<&Path>,
    git_root: Option<&Path>,
    explicit_project_db: bool,
) -> bool {
    !explicit_project_db && project_db_path.is_some() && git_root.is_some()
}

fn detach_launch_cwd_to_runtime(app_home: &Path) -> Result<PathBuf, std::io::Error> {
    let runtime = app_home.join("runtime");
    std::fs::create_dir_all(&runtime)?;
    std::env::set_current_dir(&runtime)?;
    Ok(runtime)
}

fn load_env_files(
    home: &Path,
    app_home: &Path,
    load_project_local_env: bool,
    git_root: Option<&Path>,
) {
    let _ = dotenvy::from_path(home.join(".secrets/master.env"));
    std::env::remove_var(crate::host_profile::HOST_PROFILE_ENV);
    let _ = dotenvy::from_path_override(app_home.join("config.env"));
    let canonical_host_profile = std::env::var(crate::host_profile::HOST_PROFILE_ENV).ok();
    let _ = dotenvy::from_path_override(home.join(".sigil/config.env"));

    if load_project_local_env {
        let _ = dotenvy::from_path_override(PathBuf::from(".tachi/config.env"));
        let _ = dotenvy::from_path_override(PathBuf::from(".sigil/config.env"));

        if let Ok(cwd) = std::env::current_dir() {
            let _ = dotenvy::from_path(cwd.join(".env"));
            if let Some(root) = git_root {
                if root != cwd.as_path() {
                    let _ = dotenvy::from_path(root.join(".env"));
                }
            }
        }
    }

    match canonical_host_profile {
        Some(value) => std::env::set_var(crate::host_profile::HOST_PROFILE_ENV, value),
        None => std::env::remove_var(crate::host_profile::HOST_PROFILE_ENV),
    }
}

struct StartupContext {
    home: PathBuf,
    app_home: PathBuf,
    command: Commands,
    defer_manifest_startup: bool,
    git_root: Option<PathBuf>,
    /// #1119: resolved-once schema-migration authority. Set to `Allow` only
    /// when `--allow-schema-migration` is passed; `Deny` otherwise. The
    /// pre-serve `remember` fallback and the serve/daemon constructor receive
    /// this typed decision explicitly — never through a process env var.
    schema_migration: memcore::MigrationAuthority,
}

struct StartupHygiene {
    gc_enabled: bool,
    gc_initial_delay_secs: u64,
    gc_interval_secs: u64,
    project_db_path: Option<PathBuf>,
}

struct ServerState {
    server: MemoryServer,
    background_shutdown: tokio_util::sync::CancellationToken,
    bg_handles: Vec<tokio::task::JoinHandle<()>>,
}

fn expand_user_path(raw: &str, home: &Path) -> PathBuf {
    if raw == "~" {
        home.to_path_buf()
    } else if let Some(rest) = raw.strip_prefix("~/") {
        home.join(rest)
    } else {
        PathBuf::from(raw)
    }
}

fn expand_cli_path(raw: &Path, home: &Path) -> PathBuf {
    expand_user_path(raw.to_string_lossy().as_ref(), home)
}

fn distill_db_override(command: &Commands, home: &Path) -> Option<PathBuf> {
    use tachi_bootstrap::cli::DistillAction;

    match command {
        Commands::Distill {
            action: DistillAction::Run { db: Some(db), .. },
        } => Some(expand_cli_path(db, home)),
        _ => None,
    }
}

/// `<main>-wal` / `<main>-shm` sidecar path for a SQLite main file.
fn sidecar_path(main: &Path, suffix: &str) -> PathBuf {
    let mut s = main.as_os_str().to_owned();
    s.push(suffix);
    PathBuf::from(s)
}

/// Copy a legacy DB (main file + `-wal`/`-shm` sidecars, if present) to
/// `dest`, refusing when a live daemon holds `src` open (`Owned`) or
/// ownership cannot be determined (`Unknown`) — either case risks copying a
/// torn snapshot. Only a confirmed `NotOwned` proceeds. Shared by both
/// legacy-DB migration call sites below (global + project).
///
/// Sidecar semantics: a WAL-mode source that crashed (or was never
/// checkpointed) before this migration ran has committed rows living ONLY in
/// `-wal` — copying just the main file silently drops them. So once `NotOwned`
/// clears the copy, each sidecar that exists on disk is copied too; a missing
/// sidecar is a normal (non-WAL or already-checkpointed) source and is
/// skipped. Any sidecar copy failure discards the *whole* destination (main +
/// any sidecars already copied for this call) and returns `Err` — never a
/// main-file-only (silently data-losing) copy left behind. The discard
/// itself is honest about its own failures: any file it could not delete
/// (path + underlying error; a missing file is success, not a failure) is
/// named in the returned error message rather than swallowed, so an
/// orphaned partial copy is reported, not silently left on disk.
///
/// Accepted, documented race: there is a narrow window between the ownership
/// probe above and the copy below in which a daemon could start and begin
/// writing `src`. This call site cannot hold a lock across another process's
/// startup for a path outside its own control, so — same acceptance as
/// doctor's `checkpoint_wal_copy` — the probe-then-copy gap is a known,
/// accepted residual race, not something this guard eliminates.
async fn copy_legacy_db_guarded(src: &Path, dest: &Path) -> Result<(), Box<dyn std::error::Error>> {
    match crate::db_ownership::daemon_ownership(src) {
        crate::db_ownership::DbOwnership::Owned => {
            return Err(format!(
                "legacy DB {} is held open by a live daemon; refusing to copy a possibly torn snapshot to {}. Stop the daemon first.",
                src.display(),
                dest.display()
            )
            .into());
        }
        crate::db_ownership::DbOwnership::Unknown(reason) => {
            return Err(format!(
                "cannot determine whether legacy DB {} is held by a live daemon ({reason}); refusing to copy a possibly torn snapshot to {}",
                src.display(),
                dest.display()
            )
            .into());
        }
        crate::db_ownership::DbOwnership::NotOwned => {}
    }
    if let Some(parent) = dest.parent() {
        tokio::fs::create_dir_all(parent).await?;
    }
    tokio::fs::copy(src, dest).await?;

    for suffix in ["-wal", "-shm"] {
        let side_src = sidecar_path(src, suffix);
        if !side_src.exists() {
            continue;
        }
        let side_dest = sidecar_path(dest, suffix);
        if let Err(e) = tokio::fs::copy(&side_src, &side_dest).await {
            // Discard the whole destination — main file plus any sidecar
            // already copied in this call — rather than leave a partial,
            // silently data-losing copy at `dest`. A delete failure here
            // must never be swallowed (拒必有声): collect every one (path +
            // error, a missing file is success — nothing to discard) and
            // fold them into the returned error so an orphaned partial copy
            // is reported, not silently left on disk.
            let mut cleanup_failures = Vec::new();
            if let Err(rm_err) = tokio::fs::remove_file(dest).await {
                if rm_err.kind() != std::io::ErrorKind::NotFound {
                    cleanup_failures.push(format!("{}: {rm_err}", dest.display()));
                }
            }
            for cleanup_suffix in ["-wal", "-shm"] {
                let cleanup_path = sidecar_path(dest, cleanup_suffix);
                if let Err(rm_err) = tokio::fs::remove_file(&cleanup_path).await {
                    if rm_err.kind() != std::io::ErrorKind::NotFound {
                        cleanup_failures.push(format!("{}: {rm_err}", cleanup_path.display()));
                    }
                }
            }
            let discard_note = if cleanup_failures.is_empty() {
                format!("discarded partial legacy-DB copy at {}", dest.display())
            } else {
                format!(
                    "partial copy could not be fully discarded: {}",
                    cleanup_failures.join(", ")
                )
            };
            return Err(format!(
                "failed to copy sidecar {} to {}: {e}; {discard_note}",
                side_src.display(),
                side_dest.display(),
            )
            .into());
        }
    }
    Ok(())
}

fn initialize_startup_context(cli: &Cli) -> Result<StartupContext, Box<dyn std::error::Error>> {
    // Load config from dotenv files (same as before)
    let home = dirs::home_dir().unwrap_or_else(|| PathBuf::from("."));
    let app_home = crate::path_utils::tachi_home();

    // PR7 — install the tracing sink before doing anything else so early errors
    // (config load, manifest resolution, daemon bind) are captured.
    init_tracing(&app_home);
    tracing::info!(
        version = env!("CARGO_PKG_VERSION"),
        app_home = %app_home.display(),
        "tachi tachi-server starting"
    );
    let command = cli.command.clone().unwrap_or(Commands::Serve);
    if no_project_serve_detaches_launch_cwd(&command, cli.no_project_db) {
        let runtime_cwd = detach_launch_cwd_to_runtime(&app_home).map_err(|error| {
            format!(
                "failed to detach --no-project-db serve cwd to {}: {error}",
                app_home.join("runtime").display()
            )
        })?;
        tracing::info!(
            runtime_cwd = %runtime_cwd.display(),
            "--no-project-db serve detached launch cwd to runtime"
        );
    }
    let load_project_local_env = should_load_project_local_env(&command, cli.no_project_db);
    let defer_manifest_startup =
        should_defer_manifest_startup(&command, cli.daemon, cli.no_project_db);
    let git_root = if load_project_local_env {
        find_project_git_root()
    } else {
        None
    };

    load_env_files(
        &home,
        &app_home,
        load_project_local_env,
        git_root.as_deref(),
    );

    // #1119: resolve the schema-migration authority ONCE, from the CLI flag,
    // into a typed value. Then defensively remove the legacy opt-in env var
    // (the reverted first attempt's `TACHI_ALLOW_SCHEMA_MIGRATION`) so that no
    // code path anywhere in this process — including subprocesses that would
    // otherwise inherit it — can resurrect the ambient-capability antipattern.
    // The authority now lives only in the typed value threaded below; the env
    // is never read again. This is the single removal that replaces the old
    // route's 40+ per-spawn `env_remove` calls: if the var is never set here,
    // there is nothing to scrub at each spawn boundary.
    std::env::remove_var(memcore::db::SCHEMA_MIGRATION_LEGACY_ENV);
    let schema_migration = if cli.allow_schema_migration {
        memcore::MigrationAuthority::Allow {
            approved_by: "cli:--allow-schema-migration".to_string(),
        }
    } else {
        memcore::MigrationAuthority::Deny
    };

    Ok(StartupContext {
        home,
        app_home,
        command,
        defer_manifest_startup,
        git_root,
        schema_migration,
    })
}

async fn resolve_global_db(
    cli: &Cli,
    ctx: &StartupContext,
) -> Result<PathBuf, Box<dyn std::error::Error>> {
    let global_db_path = if let Some(p) = cli.global_db.as_ref() {
        expand_cli_path(p, &ctx.home)
    } else if let Ok(p) = std::env::var("MEMORY_DB_PATH") {
        expand_user_path(&p, &ctx.home)
    } else {
        let default_global = ctx
            .app_home
            .join("global")
            .join(memcore::MEMORY_DB_FILENAME);
        // Migration: move legacy (pre-app_home-layout AND pre-#1132-filename)
        // DBs into ${TACHI_HOME}/global/tachi-memory.db. These candidates are
        // literally named `memory.db` on disk (they predate the app_home/
        // global/ layout entirely) — the #1132 rename-on-open seam inside
        // MemoryStore::open then takes over for the already-standard-layout
        // case (an existing ${TACHI_HOME}/global/memory.db sitting right next
        // to where `default_global` now points).
        let legacy_candidates = vec![
            ctx.app_home.join(memcore::LEGACY_MEMORY_DB_FILENAME),
            ctx.home
                .join(".sigil/global")
                .join(memcore::LEGACY_MEMORY_DB_FILENAME),
            ctx.home
                .join(".sigil")
                .join(memcore::LEGACY_MEMORY_DB_FILENAME),
        ];
        if !default_global.exists() {
            for legacy in legacy_candidates {
                if legacy.exists() {
                    copy_legacy_db_guarded(&legacy, &default_global).await?;
                    eprintln!(
                        "Migrated legacy DB: {} -> {}",
                        legacy.display(),
                        default_global.display()
                    );
                    break;
                }
            }
        }
        default_global
    };

    // ─── Manifest-driven resolution (PR5) ───────────────────────────────────
    //
    // The manifest (`~/.tachi/manifest.json`, populated by `tachi doctor` /
    // `tachi manifest refresh`) is the durable source of truth for "which
    // memory.db file is the global one". Heuristic discovery above can pick a
    // freshly created empty DB when the user actually has data living at a
    // non-standard path. Consult the manifest:
    //
    //   * No CLI override + manifest has a global entry  → switch to manifest path.
    //   * Explicit --global-db / MEMORY_DB_PATH that disagrees with the manifest
    //     → honor the override but warn loudly so the user can spot misconfig.
    //   * Manifest entry exists but is marked non-writable (last_classification
    //     other than "healthy") → refuse with an actionable hint.
    //
    // Failure to read the manifest is non-fatal: we proceed with the heuristic
    // result so first-run installs still work without `tachi doctor`.
    let manifest_path = ctx.app_home.join("manifest.json");
    // PR-2: hygiene pass before any manifest consumer reads. Idempotent.
    // Failures are logged and swallowed — startup must never block on GC.
    if ctx.defer_manifest_startup {
        tracing::info!(
            target: "tachi::manifest::startup",
            "--daemon --no-project-db serve deferred manifest startup hygiene"
        );
    } else if matches!(ctx.command, Commands::Serve) && manifest_path.exists() {
        match crate::manifest::gc_manifest(&manifest_path) {
            Ok(report) => {
                if report.aborted {
                    tracing::warn!(
                        target: "tachi::manifest::gc",
                        reason = report.abort_reason.as_deref().unwrap_or(""),
                        "manifest GC aborted at startup"
                    );
                } else if report.canonicalized
                    + report.removed_missing
                    + report.removed_fixture
                    + report.schema_kind_fixed
                    + report.dedup_collapsed
                    > 0
                {
                    tracing::info!(
                        target: "tachi::manifest::gc",
                        before = report.entries_before,
                        after = report.entries_after,
                        canonicalized = report.canonicalized,
                        removed_missing = report.removed_missing,
                        removed_fixture = report.removed_fixture,
                        schema_kind_fixed = report.schema_kind_fixed,
                        dedup_collapsed = report.dedup_collapsed,
                        "manifest GC applied at startup"
                    );
                }
            }
            Err(e) => {
                tracing::warn!(target: "tachi::manifest::gc", error = %e, "manifest GC failed; continuing")
            }
        }
    }
    let manifest_opt = if !ctx.defer_manifest_startup && manifest_path.exists() {
        crate::manifest::Manifest::load(&manifest_path).ok()
    } else {
        None
    };
    let global_db_path = if let Some(m) = manifest_opt.as_ref() {
        if let Some(entry) = m.global() {
            let manifest_global = PathBuf::from(&entry.path);
            let user_overrode =
                cli.global_db.is_some() || std::env::var_os("MEMORY_DB_PATH").is_some();
            if user_overrode {
                if global_db_path != manifest_global {
                    eprintln!(
                        "warning: --global-db ({}) does not match manifest global ({}). \
                         Honoring CLI override; run `tachi manifest refresh` to update the manifest.",
                        global_db_path.display(),
                        manifest_global.display()
                    );
                }
                global_db_path
            } else {
                // No override → trust the manifest.
                // Escape hatch: TACHI_BYPASS_MANIFEST=1 lets users (and the
                // daemon itself when restarting after a crash) skip the
                // manifest write guard so a misclassified-but-healthy DB
                // does not self-lock the CLI. Doctor / manifest commands
                // never go through this path.
                let bypass = std::env::var("TACHI_BYPASS_MANIFEST")
                    .ok()
                    .map(|v| matches!(v.trim().to_lowercase().as_str(), "1" | "true" | "yes"))
                    .unwrap_or(false);
                if !bypass {
                    if let Err(err) = m.check_writable(&entry.path) {
                        // Before failing hard, check if any live process
                        // is already holding this DB — that is the most
                        // common cause of a false-positive WalOrphan
                        // classification. Two checks:
                        //   (a) HTTP daemon pid file (~/.tachi/daemon.pid),
                        //       only exists when started via `tachi --daemon`;
                        //   (b) `lsof` on the DB path itself — covers
                        //       stdio MCP instances that never write a pid
                        //       file (the common case: editor/IDE spawned
                        //       tachi as a subprocess).
                        let daemon_alive = crate::cli_client::detect_daemon_for_global_db(
                            &ctx.app_home,
                            &manifest_global,
                        )
                        .await
                        .is_some();
                        let db_held = db_path_held_by_other_process(&entry.path);
                        if !daemon_alive && !db_held {
                            return Err(format!(
                                "manifest global DB is not writable: {err}. \
                                 Run `tachi doctor` to inspect, then `tachi manifest refresh` once resolved. \
                                 To force-bypass: TACHI_BYPASS_MANIFEST=1"
                            )
                            .into());
                        }
                        eprintln!(
                            "info: manifest reports {} as not writable, but a live holder was detected (daemon={daemon_alive}, lsof={db_held}) — proceeding.",
                            entry.path
                        );
                    }
                }
                if global_db_path != manifest_global {
                    eprintln!(
                        "info: using manifest global DB ({}) instead of heuristic default ({}).",
                        manifest_global.display(),
                        global_db_path.display()
                    );
                }
                manifest_global
            }
        } else {
            global_db_path
        }
    } else {
        global_db_path
    };

    if let Some(parent) = global_db_path.parent() {
        tokio::fs::create_dir_all(parent).await?;
    }

    Ok(global_db_path)
}

async fn run_startup_hygiene(
    cli: &Cli,
    ctx: &StartupContext,
    global_db_path: &PathBuf,
) -> Result<Option<StartupHygiene>, Box<dyn std::error::Error>> {
    // Backfill commands need async LLM clients, so handle them before generic CLI dispatch.
    // #1181: thread the top-level `--allow-schema-migration` decision through
    // so a rehearsal `backfill-*` run against a disposable copy of a real
    // legacy DB is not blocked by the fail-closed default.
    if run_if_backfill_command(
        &ctx.command,
        &ctx.home,
        global_db_path,
        &ctx.schema_migration,
    )
    .await?
    {
        return Ok(None);
    }

    let gc_enabled = cli
        .gc_enabled
        .or_else(|| parse_env_bool("MEMORY_GC_ENABLED"))
        .unwrap_or(true);
    let gc_initial_delay_secs = cli
        .gc_initial_delay_secs
        .or_else(|| parse_env_u64("MEMORY_GC_INITIAL_DELAY_SECS"))
        .unwrap_or(300);
    let mut gc_interval_secs = cli
        .gc_interval_secs
        .or_else(|| parse_env_u64("MEMORY_GC_INTERVAL_SECS"))
        .unwrap_or(6 * 3600);
    if gc_interval_secs == 0 {
        eprintln!("MEMORY_GC_INTERVAL_SECS/--gc-interval-secs must be >= 1; using 1 second");
        gc_interval_secs = 1;
    }

    // Resolve project DB path
    let explicit_project_db = cli.project_db.is_some();
    let mut legacy_project_copy = None;
    let project_db_path = if cli.no_project_db {
        if cli.project_db.is_some() {
            eprintln!("--project-db is ignored because --no-project-db is set");
        }
        None
    } else if let Some(p) = cli.project_db.as_ref() {
        Some(expand_cli_path(p, &ctx.home))
    } else if let Some(root) = ctx.git_root.as_ref() {
        let project_default = root.join(".tachi").join(memcore::MEMORY_DB_FILENAME);
        // Pre-.tachi-layout legacy source; the #1132 rename-on-open seam
        // inside MemoryStore::open handles an already-standard-layout
        // `.tachi/memory.db` sitting next to where `project_default` points.
        let project_legacy = root.join(".sigil").join(memcore::LEGACY_MEMORY_DB_FILENAME);

        if project_legacy.exists() && !project_default.exists() {
            legacy_project_copy = Some((project_legacy, project_default.clone()));
        }

        Some(project_default)
    } else {
        None
    };

    let distill_db_override = distill_db_override(&ctx.command, &ctx.home);
    // A distill override is the canonical project data file for this command,
    // not a managed Plan C alias. Classify its leaf before startup can create
    // the auto project's alias or copy legacy data.
    if let Some(db_path) = distill_db_override.as_deref() {
        crate::path_utils::canonical_db_leaf_exists_without_symlink(db_path)?;
    }

    // The repo-local project DB is the canonical data file, never the Plan C
    // alias. Reject a symlink leaf before alias inspection/creation or a
    // legacy copy can follow it into an external target.
    if let Some(db_path) = project_db_path.as_deref() {
        crate::path_utils::canonical_db_leaf_exists_without_symlink(db_path)?;
    }

    // Plan C: link <tachi_home>/projects/<sanitized-dir>/tachi-memory.db -> repo-local DB.
    // Explicit project DBs are caller-owned (for example embedded agent workspaces)
    // and must not rewrite the repo's global named-project alias.
    let refresh_plan_c_symlink = should_refresh_plan_c_symlink(
        project_db_path.as_deref(),
        ctx.git_root.as_deref(),
        explicit_project_db,
    );
    if refresh_plan_c_symlink {
        if let (Some(db_path), Some(root)) = (project_db_path.as_ref(), ctx.git_root.as_ref()) {
            match crate::path_utils::inspect_plan_c_alias_in_home(db_path, root, &ctx.app_home) {
                crate::path_utils::PlanCAliasInspection::SplitBrain(issue) => {
                    return Err(issue.warning_message().into());
                }
                crate::path_utils::PlanCAliasInspection::Integrity(issue) => {
                    return Err(issue.warning_message().into());
                }
                crate::path_utils::PlanCAliasInspection::Absent
                | crate::path_utils::PlanCAliasInspection::MatchingSymlink => {}
            }
        }
    }

    if let Some((project_legacy, project_default)) = legacy_project_copy {
        copy_legacy_db_guarded(&project_legacy, &project_default).await?;
        eprintln!(
            "Migrated legacy project DB: {} -> {}",
            project_legacy.display(),
            project_default.display()
        );
    }

    if refresh_plan_c_symlink {
        if let (Some(db_path), Some(root)) = (project_db_path.as_ref(), ctx.git_root.as_ref()) {
            match crate::path_utils::ensure_plan_c_symlink(db_path, root) {
                crate::path_utils::PlanCLinkOutcome::SplitBrain(issue) => {
                    return Err(issue.warning_message().into());
                }
                crate::path_utils::PlanCLinkOutcome::AliasIntegrity(issue) => {
                    return Err(issue.warning_message().into());
                }
                crate::path_utils::PlanCLinkOutcome::Failed { path, error } => {
                    return Err(format!(
                        "Plan C alias symlink failed at {}: {}; refusing startup",
                        path.display(),
                        error
                    )
                    .into());
                }
                crate::path_utils::PlanCLinkOutcome::AlreadyLinked
                | crate::path_utils::PlanCLinkOutcome::Created(_)
                | crate::path_utils::PlanCLinkOutcome::Skipped(_) => {}
            }
        }
    }

    if let Commands::Distill { action } = &ctx.command {
        use tachi_bootstrap::cli::DistillAction;
        let DistillAction::Run {
            db: _,
            no_consolidate,
        } = action;
        let target_project = distill_db_override
            .clone()
            .or(project_db_path.clone())
            .ok_or_else(|| {
                "distill run requires a project DB: pass --project-db PATH or `distill run --db PATH`"
                    .to_string()
            })?;
        if !crate::path_utils::canonical_db_leaf_exists_without_symlink(&target_project)? {
            return Err(format!("project DB not found: {}", target_project.display()).into());
        }
        let server = MemoryServer::new(global_db_path.to_path_buf(), Some(target_project.clone()))?;
        let report = crate::foundry_runtime_ops::run_daily_batch_distill_with_options(
            &server,
            !no_consolidate,
        )
        .await?;
        print_pretty_json(&serde_json::to_value(report)?)?;
        return Ok(None);
    }

    if cli.daemon && project_db_path.is_some() {
        if explicit_project_db {
            eprintln!(
                "Daemon mode: using explicit project DB path {} (single-project mode)",
                project_db_path
                    .as_ref()
                    .map(|p| p.display().to_string())
                    .unwrap_or_else(|| "<unknown>".to_string())
            );
        } else {
            // PR-4: previously this branch *disabled* the auto-detected
            // project DB to avoid mixed project context. With the multi-DB
            // FoundryScheduler in place, all manifest DBs receive foundry
            // coverage equally, so we keep the auto-detected project but
            // surface a clear warning so the operator knows the daemon's
            // bound project follows whichever cwd it was launched from.
            eprintln!(
                "Daemon mode: auto-detected project DB {} retained (multi-DB scheduler is active). \
                 Use --project-db PATH to pin a specific project, or unset to run global-only.",
                project_db_path
                    .as_ref()
                    .map(|p| p.display().to_string())
                    .unwrap_or_else(|| "<unknown>".to_string())
            );
        }
    }

    if run_pre_serve_command(
        &ctx.command,
        &ctx.home,
        &ctx.app_home,
        global_db_path,
        project_db_path.as_ref(),
        ctx.git_root.as_ref(),
        &ctx.schema_migration,
    )
    .await?
    {
        return Ok(None);
    }

    // Ensure parent dirs exist
    if let Some(parent) = global_db_path.parent() {
        tokio::fs::create_dir_all(parent).await?;
    }
    if let Some(ref p) = project_db_path {
        if let Some(parent) = p.parent() {
            tokio::fs::create_dir_all(parent).await?;
        }
    }

    Ok(Some(StartupHygiene {
        gc_enabled,
        gc_initial_delay_secs,
        gc_interval_secs,
        project_db_path,
    }))
}

async fn start_stdio_proxy_transport(
    cli: &Cli,
    ctx: &StartupContext,
    global_db_path: &Path,
    project_db_path: Option<&PathBuf>,
) -> Result<bool, Box<dyn std::error::Error>> {
    // An explicit TACHI_PROJECT pin wins over the path-derived (git-hash) name so
    // the label injected into proxied WRITES matches the label used for reads —
    // otherwise briefing reads the pinned library while proxy writes still land
    // in the git-hash library (a silent read/write split). Only bind a label
    // when a project DB is bound, preserving the prior "global-only ⇒ no label".
    let client_project_name = project_db_path.and_then(|p| {
        crate::memory_search_ops::client_project_precedence(
            crate::memory_search_ops::explicit_workspace_project(),
            crate::memory_search_ops::named_project_from_db_path_in_home(p, &ctx.app_home),
            crate::memory_search_ops::resolve_workspace_named_project(),
        )
    });

    if !cli.daemon {
        if let Some(info) = stdio::ensure_stdio_proxy_daemon(
            &ctx.app_home,
            global_db_path,
            project_db_path.map(|path| path.as_path()),
            client_project_name.as_deref(),
        )
        .await
        {
            if stdio::proxy_can_preserve_project_context(
                &info,
                &ctx.app_home,
                global_db_path,
                project_db_path.map(|path| path.as_path()),
                client_project_name.as_deref(),
            ) {
                eprintln!(
                    "[stdio-proxy] forwarding stdio MCP to daemon {} (project={})",
                    info.url,
                    client_project_name.as_deref().unwrap_or("<none>")
                );
                stdio::serve_stdio_proxy(
                    info,
                    ctx.app_home.clone(),
                    global_db_path.to_path_buf(),
                    project_db_path.cloned(),
                    client_project_name,
                )
                .await?;
                return Ok(true);
            }
            eprintln!(
                "[stdio-proxy] local fallback: project DB {} has no named-project route and daemon project scope differs",
                project_db_path
                    .map(|path| path.display().to_string())
                    .unwrap_or_else(|| "<none>".to_string())
            );
        }
    }

    if !cli.daemon && !stdio::stdio_proxy_disabled() {
        return Err("No compatible Tachi daemon available for stdio proxy. \
             A stdio serve without the daemon would open the DB directly, \
             contending for write locks with other processes (#520). \
             Start a daemon with `tachi --daemon`, or set \
             TACHI_DISABLE_STDIO_PROXY=1 to force local serve (debugging only)."
            .into());
    }

    Ok(false)
}

fn build_server_state(
    cli: &Cli,
    ctx: &StartupContext,
    global_db_path: &Path,
    hygiene: &StartupHygiene,
) -> Result<ServerState, Box<dyn std::error::Error>> {
    if cli.daemon {
        std::env::set_var("TACHI_DAEMON", "1");
    } else {
        std::env::remove_var("TACHI_DAEMON");
    }

    if let Some(project_db_path) = hygiene.project_db_path.as_deref() {
        crate::path_utils::canonical_db_leaf_exists_without_symlink(project_db_path)?;
    }

    // #1119: the serve/daemon path is the ONE process allowed to migrate a
    // live DB forward — and only when the operator passed
    // `--allow-schema-migration` (resolved into `ctx.schema_migration`).
    let server = MemoryServer::new_with_migration_authority(
        global_db_path.to_path_buf(),
        hygiene.project_db_path.clone(),
        ctx.schema_migration.clone(),
    )?;
    match crate::signature_evidence::seed_signature_taxonomy_evidence(&server) {
        Ok(true) => eprintln!("[signatures] seeded 2026-07-05 error-signature taxonomy evidence"),
        Ok(false) => {}
        Err(err) => eprintln!("[signatures] taxonomy evidence seed skipped: {err}"),
    }
    match crate::component_governance_ops::seed_component_records(&server) {
        Ok(true) => eprintln!("[components] seeded v0 component governance records"),
        Ok(false) => {}
        // Non-fatal: boot continues. The seed-once marker is claimed only after
        // all writes succeed, so a failed seed leaves no marker — any partially
        // written governance edges are effectively dropped for this boot and the
        // idempotent upserts are re-attempted on the next boot. This does NOT
        // block startup.
        Err(err) => eprintln!(
            "[components] governance record seed failed (edges dropped, will re-seed next boot; boot continues): {err}"
        ),
    }
    let recovered = crate::dispatch_ops::recover_orphaned_dispatch_runs(&server);
    if !recovered.is_empty() {
        eprintln!(
            "[dispatch] recovered {} orphaned run(s): {}",
            recovered.len(),
            recovered.join(", ")
        );
    }

    let requested_tool_profile = cli
        .profile
        .clone()
        .or_else(|| std::env::var("TACHI_PROFILE").ok());
    if let Some(raw_profile) = requested_tool_profile.as_deref() {
        // #919 CRITICAL: an unrecognized CLI/env profile used to be logged and
        // ignored, silently falling back to the (unrestricted) `standard`
        // default — a fail-open widening of whatever narrower surface the
        // caller intended. Reject at startup instead, matching the HTTP
        // direct-connect path (`parse_http_tool_profile`), which already
        // hard-errors on an unknown profile rather than defaulting.
        let profile = tachi_hub::parse_tool_profile(raw_profile).ok_or_else(
            || -> Box<dyn std::error::Error> {
                format!(
                    "unknown tool profile '{raw_profile}'; expected observe | remember | coordinate | operate | delegate | admin or a compatible host alias"
                )
                .into()
            },
        )?;
        server.set_tool_profile(Some(profile));
    } else {
        eprintln!("{DEFAULT_STANDARD_PROFILE_NOTICE}");
    }

    // Auto-unlock vault from macOS Keychain (service: tachi-vault, account: default)
    crate::provider_config::bootstrap_provider_runtime(&server);
    match server.llm.provider_secret_count() {
        0 => eprintln!("[provider] no provider keys materialized (Vault locked or empty)"),
        n => eprintln!("[provider] {n} provider key(s) ready for LLM/embed"),
    }

    let background_shutdown = tokio_util::sync::CancellationToken::new();
    let mut bg_handles: Vec<tokio::task::JoinHandle<()>> = Vec::new();
    if embedded_mcp_facade() {
        eprintln!("[embedded-mcp] owner background tasks disabled; forwarding to scoped daemon");
    } else {
        bg_handles.push(spawn_idle_connection_cleanup(
            &server,
            background_shutdown.clone(),
        ));
        let maintain_named_projects = !cli.daemon
            || daemon::daemon_uses_manifest_background(
                &ctx.app_home,
                global_db_path,
                hygiene.project_db_path.as_deref(),
            );
        bg_handles.push(spawn_wal_checkpoint(
            &server,
            maintain_named_projects,
            background_shutdown.clone(),
        ));
        bg_handles.push(spawn_background_gc(
            &server,
            hygiene.gc_enabled,
            hygiene.gc_initial_delay_secs,
            hygiene.gc_interval_secs,
            background_shutdown.clone(),
        ));
        run_startup_integrity_checks(&server, hygiene.project_db_path.is_some())?;
        load_cached_hub_tools(&server);
        bg_handles.push(report_pipeline_and_spawn_daily_distill(
            &server,
            &ctx.app_home,
            background_shutdown.clone(),
        ));
        // #605 background CI watcher: polls GitHub check state for tracked open
        // PRs and records transitions into flow check_state ledgers. Observes +
        // records only — no webhook, no repair, no merge. Self-gated on
        // TACHI_CI_WATCH / TACHI_DAEMON, so stdio/CLI invocations no-op.
        let ci_reader = crate::gh_ops::daemon_ci_reader(std::sync::Arc::new(server.clone()));
        bg_handles.push(crate::gh_ops::spawn_ci_watch(
            ci_reader,
            background_shutdown.clone(),
        ));
    }

    Ok(ServerState {
        server,
        background_shutdown,
        bg_handles,
    })
}

async fn start_server_transport(
    cli: &Cli,
    ctx: &StartupContext,
    global_db_path: &Path,
    hygiene: &StartupHygiene,
    state: ServerState,
) -> Result<(), Box<dyn std::error::Error>> {
    let ServerState {
        server,
        background_shutdown,
        bg_handles,
    } = state;

    eprintln!(
        "Starting Tachi MCP Server v{} ({})",
        crate::build_info::build_version_string(),
        crate::build_info::GIT_SHA
    );
    eprintln!(
        "Transport: {}",
        if cli.daemon {
            format!("HTTP daemon on port {}", cli.port)
        } else {
            "stdio".to_string()
        }
    );
    eprintln!("Global DB: {}", global_db_path.display());
    if let Some(ref p) = hygiene.project_db_path {
        eprintln!("Project DB: {}", p.display());
    } else {
        eprintln!("Project DB: none (not in a git repository)");
    }
    eprintln!(
        "Vector search: global={}, project={}",
        server.global_vec_available(),
        server.project_vec_available()
    );
    eprintln!(
        "Tool surface: {}",
        server
            .active_tool_profile()
            .map(|profile| profile.as_str())
            .unwrap_or_else(|| tachi_hub::default_tool_profile().as_str())
    );

    let serve_result = if cli.daemon {
        serve_http_daemon(
            server,
            ctx.app_home.clone(),
            global_db_path.to_path_buf(),
            hygiene.project_db_path.clone(),
            cli.port,
        )
        .await
    } else {
        serve_stdio(server).await
    };

    background_shutdown.cancel();
    if !bg_handles.is_empty() {
        eprintln!(
            "[shutdown] waiting for {} background task(s) to finish...",
            bg_handles.len()
        );
        for handle in bg_handles {
            // Deliberately swallows task-panic JoinError so a panicking bg task doesn't take down the daemon.
            let _ = handle.await;
        }
    }

    serve_result?;
    if !cli.daemon {
        // #1273 Gap 1: the direct (non-proxy, `TACHI_DISABLE_STDIO_PROXY=1`
        // debugging-only) stdio path holds a live `MemoryServer`/DB
        // connection; the WAL-checkpoint and GC background tasks were
        // already cancelled and joined above, so their cleanup has already
        // run. From here, force-exit rather than let control fall back
        // through `tokio_main` into `#[tokio::main]`'s implicit
        // `Runtime::drop`, which would block indefinitely on the same
        // un-cancellable stdin blocking-read thread documented on
        // `stdio::stdio_hard_exit`. The HTTP daemon path never reaches this
        // branch and is unaffected: it has no stdin transport, so its normal
        // `Ok(())` return already terminates cleanly.
        stdio::stdio_hard_exit();
    }
    Ok(())
}

#[tokio::main]
pub(super) async fn tokio_main(cli: Cli) -> Result<(), Box<dyn std::error::Error>> {
    let ctx = initialize_startup_context(&cli)?;
    let global_db_path = resolve_global_db(&cli, &ctx).await?;
    let Some(hygiene) = run_startup_hygiene(&cli, &ctx, &global_db_path).await? else {
        return Ok(());
    };

    if start_stdio_proxy_transport(
        &cli,
        &ctx,
        &global_db_path,
        hygiene.project_db_path.as_ref(),
    )
    .await?
    {
        return Ok(());
    }

    let state = build_server_state(&cli, &ctx, &global_db_path, &hygiene)?;
    start_server_transport(&cli, &ctx, &global_db_path, &hygiene, state).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::EnvRestore;

    #[cfg(unix)]
    struct ClearOwnershipInject;
    #[cfg(unix)]
    impl Drop for ClearOwnershipInject {
        fn drop(&mut self) {
            crate::db_ownership::set_ownership_inject_for_test(None);
        }
    }

    fn startup_test_cli() -> Cli {
        Cli {
            daemon: false,
            port: 6919,
            global_db: None,
            project_db: None,
            no_project_db: false,
            allow_schema_migration: false,
            profile: None,
            gc_enabled: None,
            gc_initial_delay_secs: None,
            gc_interval_secs: None,
            command: Some(Commands::Serve),
        }
    }

    fn startup_test_context(root: &Path, app_home: &Path) -> StartupContext {
        StartupContext {
            home: root.to_path_buf(),
            app_home: app_home.to_path_buf(),
            command: Commands::Serve,
            defer_manifest_startup: false,
            git_root: Some(root.to_path_buf()),
            schema_migration: memcore::MigrationAuthority::Deny,
        }
    }

    fn startup_distill_cli_and_context(
        root: &Path,
        app_home: &Path,
        db: PathBuf,
    ) -> (Cli, StartupContext) {
        use tachi_bootstrap::cli::DistillAction;

        let mut cli = startup_test_cli();
        cli.command = Some(Commands::Distill {
            action: DistillAction::Run {
                db: Some(db.clone()),
                no_consolidate: false,
            },
        });
        let mut ctx = startup_test_context(root, app_home);
        ctx.command = Commands::Distill {
            action: DistillAction::Run {
                db: Some(db),
                no_consolidate: false,
            },
        };
        (cli, ctx)
    }

    #[cfg(unix)]
    fn startup_file_identity(path: &Path) -> (u64, u64) {
        use std::os::unix::fs::MetadataExt;

        let metadata = std::fs::symlink_metadata(path).expect("path metadata");
        (metadata.dev(), metadata.ino())
    }

    #[cfg(unix)]
    fn assert_startup_canonical_symlink_refusal_side_effects(
        root: &Path,
        app_home: &Path,
        canonical_db: &Path,
        expected_target: &Path,
        canonical_identity: (u64, u64),
        legacy_db: &Path,
        manifest_before: &[u8],
    ) {
        assert_eq!(startup_file_identity(canonical_db), canonical_identity);
        assert_eq!(
            std::fs::read_link(canonical_db).expect("canonical symlink preserved"),
            expected_target
        );
        assert_eq!(
            std::fs::read(legacy_db).expect("legacy DB preserved"),
            b"legacy-source"
        );
        assert_eq!(
            std::fs::read(app_home.join("manifest.json")).expect("manifest preserved"),
            manifest_before
        );
        let project = crate::path_utils::plan_c_dir_name_from_root(root).expect("project name");
        let alias = crate::path_utils::plan_c_global_db_path(&project);
        assert!(
            std::fs::symlink_metadata(alias).is_err(),
            "canonical refusal must precede Plan C alias creation"
        );
    }

    #[cfg(unix)]
    fn assert_distill_override_refusal_side_effects(
        root: &Path,
        app_home: &Path,
        global_db: &Path,
        manifest_before: &[u8],
    ) {
        let canonical_db = root.join(".tachi").join(memcore::MEMORY_DB_FILENAME);
        assert!(
            std::fs::symlink_metadata(&canonical_db).is_err(),
            "override refusal must not create the auto project DB"
        );
        let project = crate::path_utils::plan_c_dir_name_from_root(root).expect("project name");
        let alias = crate::path_utils::plan_c_global_db_path(&project);
        assert!(
            std::fs::symlink_metadata(alias).is_err(),
            "override refusal must precede Plan C alias creation"
        );
        assert!(
            std::fs::symlink_metadata(global_db).is_err(),
            "override refusal must precede global DB open"
        );
        assert_eq!(
            std::fs::read(app_home.join("manifest.json")).expect("manifest preserved"),
            manifest_before
        );
    }

    #[tokio::test(flavor = "current_thread")]
    #[cfg(unix)]
    #[allow(clippy::await_holding_lock)]
    async fn distill_db_override_dangling_symlink_refuses_before_side_effects() {
        let _guard = crate::utils::global_test_lock()
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let fixture = crate::test_support::non_skipped_fixture_tempdir("distill-db-override-");
        let root = fixture.path().join("Dangling-Override-Repo");
        let app_home = fixture.path().join("home");
        let _tachi_home = EnvRestore::set_path("TACHI_HOME", &app_home);
        let _sigil_home = EnvRestore::remove("SIGIL_HOME");
        let _app_home = EnvRestore::remove("TACHI_APP_HOME");
        std::fs::create_dir_all(root.join(".git")).expect("git root");
        std::fs::create_dir_all(&app_home).expect("app home");
        let manifest_before = b"distill-manifest-preimage";
        std::fs::write(app_home.join("manifest.json"), manifest_before).expect("manifest");
        let external_target = fixture.path().join("external/dangling.db");
        let override_db = fixture.path().join("override.db");
        std::os::unix::fs::symlink(&external_target, &override_db).expect("dangling override");
        let override_identity = startup_file_identity(&override_db);
        let (cli, ctx) = startup_distill_cli_and_context(&root, &app_home, override_db.clone());
        let global_db = app_home.join("global").join(memcore::MEMORY_DB_FILENAME);

        let error = match run_startup_hygiene(&cli, &ctx, &global_db).await {
            Err(error) => error,
            Ok(_) => panic!("dangling distill DB override must refuse startup"),
        };

        assert!(
            error.to_string().contains("canonical repo DB path")
                && error.to_string().contains("must not be a symlink"),
            "expected canonical leaf refusal, got: {error}"
        );
        assert_eq!(startup_file_identity(&override_db), override_identity);
        assert_eq!(std::fs::read_link(&override_db).unwrap(), external_target);
        assert!(std::fs::symlink_metadata(&external_target).is_err());
        assert_distill_override_refusal_side_effects(&root, &app_home, &global_db, manifest_before);
    }

    #[tokio::test(flavor = "current_thread")]
    #[cfg(unix)]
    #[allow(clippy::await_holding_lock)]
    async fn distill_db_override_wrong_symlink_refuses_without_external_mutation() {
        let _guard = crate::utils::global_test_lock()
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let fixture = crate::test_support::non_skipped_fixture_tempdir("distill-db-override-");
        let root = fixture.path().join("Wrong-Override-Repo");
        let app_home = fixture.path().join("home");
        let _tachi_home = EnvRestore::set_path("TACHI_HOME", &app_home);
        let _sigil_home = EnvRestore::remove("SIGIL_HOME");
        let _app_home = EnvRestore::remove("TACHI_APP_HOME");
        std::fs::create_dir_all(root.join(".git")).expect("git root");
        std::fs::create_dir_all(&app_home).expect("app home");
        let manifest_before = b"distill-manifest-preimage";
        std::fs::write(app_home.join("manifest.json"), manifest_before).expect("manifest");
        let external_target = fixture.path().join("external/foreign.db");
        std::fs::create_dir_all(external_target.parent().unwrap()).expect("external parent");
        let store = memcore::MemoryStore::open(external_target.to_str().expect("UTF-8 DB"))
            .expect("seed foreign DB");
        drop(store);
        let external_identity = startup_file_identity(&external_target);
        let external_before = std::fs::read(&external_target).expect("foreign DB bytes");
        let override_db = fixture.path().join("override.db");
        std::os::unix::fs::symlink(&external_target, &override_db).expect("wrong override");
        let override_identity = startup_file_identity(&override_db);
        let (cli, ctx) = startup_distill_cli_and_context(&root, &app_home, override_db.clone());
        let global_db = app_home.join("global").join(memcore::MEMORY_DB_FILENAME);

        let error = match run_startup_hygiene(&cli, &ctx, &global_db).await {
            Err(error) => error,
            Ok(_) => panic!("wrong-target distill DB override must refuse startup"),
        };

        assert!(
            error.to_string().contains("canonical repo DB path")
                && error.to_string().contains("must not be a symlink"),
            "expected canonical leaf refusal, got: {error}"
        );
        assert_eq!(startup_file_identity(&override_db), override_identity);
        assert_eq!(std::fs::read_link(&override_db).unwrap(), external_target);
        assert_eq!(startup_file_identity(&external_target), external_identity);
        assert_eq!(std::fs::read(&external_target).unwrap(), external_before);
        assert_distill_override_refusal_side_effects(&root, &app_home, &global_db, manifest_before);
    }

    #[tokio::test(flavor = "current_thread")]
    #[cfg(unix)]
    #[allow(clippy::await_holding_lock)]
    async fn distill_db_override_symlink_loop_refuses_before_side_effects() {
        let _guard = crate::utils::global_test_lock()
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let fixture = crate::test_support::non_skipped_fixture_tempdir("distill-db-override-");
        let root = fixture.path().join("Loop-Override-Repo");
        let app_home = fixture.path().join("home");
        let _tachi_home = EnvRestore::set_path("TACHI_HOME", &app_home);
        let _sigil_home = EnvRestore::remove("SIGIL_HOME");
        let _app_home = EnvRestore::remove("TACHI_APP_HOME");
        std::fs::create_dir_all(root.join(".git")).expect("git root");
        std::fs::create_dir_all(&app_home).expect("app home");
        let manifest_before = b"distill-manifest-preimage";
        std::fs::write(app_home.join("manifest.json"), manifest_before).expect("manifest");
        let override_db = fixture.path().join("override.db");
        std::os::unix::fs::symlink(&override_db, &override_db).expect("looped override");
        let override_identity = startup_file_identity(&override_db);
        let (cli, ctx) = startup_distill_cli_and_context(&root, &app_home, override_db.clone());
        let global_db = app_home.join("global").join(memcore::MEMORY_DB_FILENAME);

        let error = match run_startup_hygiene(&cli, &ctx, &global_db).await {
            Err(error) => error,
            Ok(_) => panic!("looped distill DB override must refuse startup"),
        };

        assert!(
            error.to_string().contains("canonical repo DB path")
                && error.to_string().contains("must not be a symlink"),
            "expected canonical leaf refusal, got: {error}"
        );
        assert_eq!(startup_file_identity(&override_db), override_identity);
        assert_eq!(std::fs::read_link(&override_db).unwrap(), override_db);
        assert_distill_override_refusal_side_effects(&root, &app_home, &global_db, manifest_before);
    }

    #[tokio::test(flavor = "current_thread")]
    #[cfg(unix)]
    #[allow(clippy::await_holding_lock)]
    async fn distill_db_override_regular_file_opens_normally() {
        let _guard = crate::utils::global_test_lock()
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let fixture = crate::test_support::non_skipped_fixture_tempdir("distill-db-override-");
        let root = fixture.path().join("Regular-Override-Repo");
        let app_home = fixture.path().join("home");
        let _tachi_home = EnvRestore::set_path("TACHI_HOME", &app_home);
        let _sigil_home = EnvRestore::remove("SIGIL_HOME");
        let _app_home = EnvRestore::remove("TACHI_APP_HOME");
        std::fs::create_dir_all(root.join(".git")).expect("git root");
        let override_db = fixture.path().join("override.db");
        let store = memcore::MemoryStore::open(override_db.to_str().expect("UTF-8 DB"))
            .expect("seed regular override DB");
        drop(store);
        let global_db = app_home.join("global").join(memcore::MEMORY_DB_FILENAME);
        std::fs::create_dir_all(global_db.parent().unwrap()).expect("global parent");
        let global = memcore::MemoryStore::open(global_db.to_str().expect("UTF-8 global DB"))
            .expect("seed global DB");
        drop(global);
        let (cli, ctx) = startup_distill_cli_and_context(&root, &app_home, override_db.clone());

        let result = run_startup_hygiene(&cli, &ctx, &global_db)
            .await
            .expect("regular distill DB override works");

        assert!(result.is_none(), "distill command completes during hygiene");
        let metadata = std::fs::symlink_metadata(override_db).expect("override metadata");
        assert!(metadata.file_type().is_file());
        assert!(!metadata.file_type().is_symlink());
    }

    #[tokio::test(flavor = "current_thread")]
    #[cfg(unix)]
    #[allow(clippy::await_holding_lock)]
    async fn startup_canonical_db_dangling_symlink_refuses_before_side_effects() {
        let _guard = crate::utils::global_test_lock()
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let fixture = crate::test_support::non_skipped_fixture_tempdir("startup-canonical-");
        let root = fixture.path().join("Dangling-Canonical-Repo");
        let app_home = fixture.path().join("home");
        let _tachi_home = EnvRestore::set_path("TACHI_HOME", &app_home);
        let _sigil_home = EnvRestore::remove("SIGIL_HOME");
        let _app_home = EnvRestore::remove("TACHI_APP_HOME");
        std::fs::create_dir_all(root.join(".git")).expect("git root");
        let canonical_db = root.join(".tachi").join(memcore::MEMORY_DB_FILENAME);
        std::fs::create_dir_all(canonical_db.parent().unwrap()).expect("canonical parent");
        let external_target = fixture.path().join("external/dangling.db");
        std::os::unix::fs::symlink(&external_target, &canonical_db)
            .expect("dangling canonical symlink");
        let canonical_identity = startup_file_identity(&canonical_db);
        let legacy_db = root.join(".sigil").join(memcore::LEGACY_MEMORY_DB_FILENAME);
        std::fs::create_dir_all(legacy_db.parent().unwrap()).expect("legacy parent");
        std::fs::write(&legacy_db, b"legacy-source").expect("legacy DB");
        std::fs::create_dir_all(&app_home).expect("app home");
        let manifest_before = b"startup-manifest-preimage";
        std::fs::write(app_home.join("manifest.json"), manifest_before).expect("manifest");
        let cli = startup_test_cli();
        let ctx = startup_test_context(&root, &app_home);
        let global_db = app_home.join("global").join(memcore::MEMORY_DB_FILENAME);
        let _clear = ClearOwnershipInject;
        crate::db_ownership::set_ownership_inject_for_test(Some(
            crate::db_ownership::DbOwnership::NotOwned,
        ));

        let error = match run_startup_hygiene(&cli, &ctx, &global_db).await {
            Err(error) => error,
            Ok(_) => panic!("dangling canonical DB symlink must refuse startup"),
        };

        assert!(
            error.to_string().contains("canonical repo DB path")
                && error.to_string().contains("must not be a symlink"),
            "expected canonical leaf refusal, got: {error}"
        );
        assert!(
            std::fs::symlink_metadata(&external_target).is_err(),
            "startup must not create the dangling external target"
        );
        assert_startup_canonical_symlink_refusal_side_effects(
            &root,
            &app_home,
            &canonical_db,
            &external_target,
            canonical_identity,
            &legacy_db,
            manifest_before,
        );
    }

    #[tokio::test(flavor = "current_thread")]
    #[cfg(unix)]
    #[allow(clippy::await_holding_lock)]
    async fn startup_canonical_db_wrong_target_symlink_refuses_before_side_effects() {
        let _guard = crate::utils::global_test_lock()
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let fixture = crate::test_support::non_skipped_fixture_tempdir("startup-canonical-");
        let root = fixture.path().join("Wrong-Canonical-Repo");
        let app_home = fixture.path().join("home");
        let _tachi_home = EnvRestore::set_path("TACHI_HOME", &app_home);
        let _sigil_home = EnvRestore::remove("SIGIL_HOME");
        let _app_home = EnvRestore::remove("TACHI_APP_HOME");
        std::fs::create_dir_all(root.join(".git")).expect("git root");
        let canonical_db = root.join(".tachi").join(memcore::MEMORY_DB_FILENAME);
        std::fs::create_dir_all(canonical_db.parent().unwrap()).expect("canonical parent");
        let external_target = fixture.path().join("external/wrong.db");
        std::fs::create_dir_all(external_target.parent().unwrap()).expect("external parent");
        std::fs::write(&external_target, b"foreign-external-db").expect("external DB");
        let external_identity = startup_file_identity(&external_target);
        std::os::unix::fs::symlink(&external_target, &canonical_db)
            .expect("wrong-target canonical symlink");
        let canonical_identity = startup_file_identity(&canonical_db);
        let legacy_db = root.join(".sigil").join(memcore::LEGACY_MEMORY_DB_FILENAME);
        std::fs::create_dir_all(legacy_db.parent().unwrap()).expect("legacy parent");
        std::fs::write(&legacy_db, b"legacy-source").expect("legacy DB");
        std::fs::create_dir_all(&app_home).expect("app home");
        let manifest_before = b"startup-manifest-preimage";
        std::fs::write(app_home.join("manifest.json"), manifest_before).expect("manifest");
        let cli = startup_test_cli();
        let ctx = startup_test_context(&root, &app_home);
        let global_db = app_home.join("global").join(memcore::MEMORY_DB_FILENAME);

        let error = match run_startup_hygiene(&cli, &ctx, &global_db).await {
            Err(error) => error,
            Ok(_) => panic!("wrong-target canonical DB symlink must refuse startup"),
        };

        assert!(
            error.to_string().contains("canonical repo DB path")
                && error.to_string().contains("must not be a symlink"),
            "expected canonical leaf refusal, got: {error}"
        );
        assert_eq!(startup_file_identity(&external_target), external_identity);
        assert_eq!(
            std::fs::read(&external_target).expect("external DB preserved"),
            b"foreign-external-db"
        );
        assert_startup_canonical_symlink_refusal_side_effects(
            &root,
            &app_home,
            &canonical_db,
            &external_target,
            canonical_identity,
            &legacy_db,
            manifest_before,
        );
    }

    #[tokio::test(flavor = "current_thread")]
    #[cfg(unix)]
    #[allow(clippy::await_holding_lock)]
    async fn startup_canonical_db_symlink_loop_refuses_before_side_effects() {
        let _guard = crate::utils::global_test_lock()
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let fixture = crate::test_support::non_skipped_fixture_tempdir("startup-canonical-");
        let root = fixture.path().join("Loop-Canonical-Repo");
        let app_home = fixture.path().join("home");
        let _tachi_home = EnvRestore::set_path("TACHI_HOME", &app_home);
        let _sigil_home = EnvRestore::remove("SIGIL_HOME");
        let _app_home = EnvRestore::remove("TACHI_APP_HOME");
        std::fs::create_dir_all(root.join(".git")).expect("git root");
        let canonical_db = root.join(".tachi").join(memcore::MEMORY_DB_FILENAME);
        std::fs::create_dir_all(canonical_db.parent().unwrap()).expect("canonical parent");
        std::os::unix::fs::symlink(&canonical_db, &canonical_db).expect("looped canonical symlink");
        let canonical_identity = startup_file_identity(&canonical_db);
        let legacy_db = root.join(".sigil").join(memcore::LEGACY_MEMORY_DB_FILENAME);
        std::fs::create_dir_all(legacy_db.parent().unwrap()).expect("legacy parent");
        std::fs::write(&legacy_db, b"legacy-source").expect("legacy DB");
        std::fs::create_dir_all(&app_home).expect("app home");
        let manifest_before = b"startup-manifest-preimage";
        std::fs::write(app_home.join("manifest.json"), manifest_before).expect("manifest");
        let cli = startup_test_cli();
        let ctx = startup_test_context(&root, &app_home);
        let global_db = app_home.join("global").join(memcore::MEMORY_DB_FILENAME);
        let _clear = ClearOwnershipInject;
        crate::db_ownership::set_ownership_inject_for_test(Some(
            crate::db_ownership::DbOwnership::NotOwned,
        ));

        let error = match run_startup_hygiene(&cli, &ctx, &global_db).await {
            Err(error) => error,
            Ok(_) => panic!("canonical DB symlink loop must refuse startup"),
        };

        assert!(
            error.to_string().contains("canonical repo DB path")
                && error.to_string().contains("must not be a symlink"),
            "expected canonical leaf refusal, got: {error}"
        );
        assert_startup_canonical_symlink_refusal_side_effects(
            &root,
            &app_home,
            &canonical_db,
            &canonical_db,
            canonical_identity,
            &legacy_db,
            manifest_before,
        );
    }

    #[tokio::test(flavor = "current_thread")]
    #[cfg(unix)]
    #[allow(clippy::await_holding_lock)]
    async fn startup_canonical_db_normal_absence_keeps_plan_c_alias_and_opens_regular_db() {
        let _guard = crate::utils::global_test_lock()
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let fixture = crate::test_support::non_skipped_fixture_tempdir("startup-canonical-");
        let root = fixture.path().join("Absent-Canonical-Repo");
        let app_home = fixture.path().join("home");
        let _tachi_home = EnvRestore::set_path("TACHI_HOME", &app_home);
        let _sigil_home = EnvRestore::remove("SIGIL_HOME");
        let _app_home = EnvRestore::remove("TACHI_APP_HOME");
        std::fs::create_dir_all(root.join(".git")).expect("git root");
        let canonical_db = root.join(".tachi").join(memcore::MEMORY_DB_FILENAME);
        assert!(std::fs::symlink_metadata(&canonical_db).is_err());
        let cli = startup_test_cli();
        let ctx = startup_test_context(&root, &app_home);
        let global_db = app_home.join("global").join(memcore::MEMORY_DB_FILENAME);

        let hygiene = run_startup_hygiene(&cli, &ctx, &global_db)
            .await
            .expect("normal absent startup hygiene")
            .expect("serve continues");
        let server = MemoryServer::new(global_db, hygiene.project_db_path.clone())
            .expect("startup opens absent canonical DB as a regular file");
        drop(server);

        let metadata = std::fs::symlink_metadata(&canonical_db).expect("canonical DB created");
        assert!(metadata.file_type().is_file());
        assert!(!metadata.file_type().is_symlink());
        let project = crate::path_utils::plan_c_dir_name_from_root(&root).expect("project name");
        let alias = crate::path_utils::plan_c_global_db_path(&project);
        assert_eq!(
            std::fs::canonicalize(alias).expect("Plan C alias resolves"),
            std::fs::canonicalize(canonical_db).expect("canonical DB resolves")
        );
    }

    #[tokio::test(flavor = "current_thread")]
    #[cfg(unix)]
    #[allow(clippy::await_holding_lock)]
    async fn startup_canonical_db_regular_file_keeps_plan_c_alias_and_opens() {
        let _guard = crate::utils::global_test_lock()
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let fixture = crate::test_support::non_skipped_fixture_tempdir("startup-canonical-");
        let root = fixture.path().join("Regular-Canonical-Repo");
        let app_home = fixture.path().join("home");
        let _tachi_home = EnvRestore::set_path("TACHI_HOME", &app_home);
        let _sigil_home = EnvRestore::remove("SIGIL_HOME");
        let _app_home = EnvRestore::remove("TACHI_APP_HOME");
        std::fs::create_dir_all(root.join(".git")).expect("git root");
        let canonical_db = root.join(".tachi").join(memcore::MEMORY_DB_FILENAME);
        std::fs::create_dir_all(canonical_db.parent().unwrap()).expect("canonical parent");
        let store = memcore::MemoryStore::open(canonical_db.to_str().expect("UTF-8 DB"))
            .expect("seed regular canonical DB");
        drop(store);
        let canonical_identity = startup_file_identity(&canonical_db);
        let cli = startup_test_cli();
        let ctx = startup_test_context(&root, &app_home);
        let global_db = app_home.join("global").join(memcore::MEMORY_DB_FILENAME);

        let hygiene = run_startup_hygiene(&cli, &ctx, &global_db)
            .await
            .expect("regular canonical startup hygiene")
            .expect("serve continues");
        let server = MemoryServer::new(global_db, hygiene.project_db_path.clone())
            .expect("startup opens regular canonical DB");
        drop(server);

        assert_eq!(startup_file_identity(&canonical_db), canonical_identity);
        let project = crate::path_utils::plan_c_dir_name_from_root(&root).expect("project name");
        let alias = crate::path_utils::plan_c_global_db_path(&project);
        assert_eq!(
            std::fs::canonicalize(alias).expect("Plan C alias resolves"),
            std::fs::canonicalize(canonical_db).expect("canonical DB resolves")
        );
    }

    #[tokio::test(flavor = "current_thread")]
    #[cfg(unix)]
    #[allow(clippy::await_holding_lock)]
    async fn startup_plan_c_alias_refusal_precedes_legacy_copy_for_regular_file() {
        let _guard = crate::utils::global_test_lock()
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let fixture = crate::test_support::non_skipped_fixture_tempdir("startup-alias-");
        let root = fixture.path().join("Regular-Alias-Repo");
        let app_home = fixture.path().join("home");
        let _tachi_home = EnvRestore::set_path("TACHI_HOME", &app_home);
        let _sigil_home = EnvRestore::remove("SIGIL_HOME");
        let _app_home = EnvRestore::remove("TACHI_APP_HOME");
        std::fs::create_dir_all(root.join(".git")).expect("git root");
        let legacy_db = root.join(".sigil").join(memcore::LEGACY_MEMORY_DB_FILENAME);
        std::fs::create_dir_all(legacy_db.parent().unwrap()).expect("legacy parent");
        std::fs::write(&legacy_db, b"legacy-source").expect("legacy DB");
        let canonical_db = root.join(".tachi").join(memcore::MEMORY_DB_FILENAME);
        let alias_name = crate::path_utils::plan_c_dir_name_from_root(&root).expect("alias name");
        let alias_db = crate::path_utils::plan_c_global_db_path(&alias_name);
        std::fs::create_dir_all(alias_db.parent().unwrap()).expect("alias parent");
        std::fs::write(&alias_db, b"divergent-alias").expect("alias DB");
        let global_db = app_home.join("global").join(memcore::MEMORY_DB_FILENAME);
        let cli = startup_test_cli();
        let ctx = startup_test_context(&root, &app_home);
        let _clear = ClearOwnershipInject;
        crate::db_ownership::set_ownership_inject_for_test(Some(
            crate::db_ownership::DbOwnership::NotOwned,
        ));

        let error = match run_startup_hygiene(&cli, &ctx, &global_db).await {
            Err(error) => error,
            Ok(_) => panic!("divergent alias must refuse startup"),
        };

        assert!(error.to_string().contains("split-brain"), "{error}");
        assert!(!canonical_db.exists(), "canonical DB must remain absent");
        assert!(
            !canonical_db.parent().unwrap().exists(),
            "canonical parent must remain absent"
        );
        assert_eq!(std::fs::read(&legacy_db).unwrap(), b"legacy-source");
        assert_eq!(std::fs::read(&alias_db).unwrap(), b"divergent-alias");
    }

    #[tokio::test(flavor = "current_thread")]
    #[cfg(unix)]
    #[allow(clippy::await_holding_lock)]
    async fn startup_plan_c_alias_refusal_precedes_legacy_copy_for_wrong_symlink() {
        let _guard = crate::utils::global_test_lock()
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let fixture = crate::test_support::non_skipped_fixture_tempdir("startup-alias-");
        let root = fixture.path().join("Wrong-Symlink-Repo");
        let app_home = fixture.path().join("home");
        let _tachi_home = EnvRestore::set_path("TACHI_HOME", &app_home);
        let _sigil_home = EnvRestore::remove("SIGIL_HOME");
        let _app_home = EnvRestore::remove("TACHI_APP_HOME");
        std::fs::create_dir_all(root.join(".git")).expect("git root");
        let legacy_db = root.join(".sigil").join(memcore::LEGACY_MEMORY_DB_FILENAME);
        std::fs::create_dir_all(legacy_db.parent().unwrap()).expect("legacy parent");
        std::fs::write(&legacy_db, b"legacy-source").expect("legacy DB");
        let canonical_db = root.join(".tachi").join(memcore::MEMORY_DB_FILENAME);
        let alias_name = crate::path_utils::plan_c_dir_name_from_root(&root).expect("alias name");
        let alias_db = crate::path_utils::plan_c_global_db_path(&alias_name);
        let wrong_db = fixture.path().join("wrong-target.db");
        std::fs::write(&wrong_db, b"wrong-target").expect("wrong target");
        std::fs::create_dir_all(alias_db.parent().unwrap()).expect("alias parent");
        std::os::unix::fs::symlink(&wrong_db, &alias_db).expect("wrong symlink");
        let global_db = app_home.join("global").join(memcore::MEMORY_DB_FILENAME);
        let cli = startup_test_cli();
        let ctx = startup_test_context(&root, &app_home);
        let _clear = ClearOwnershipInject;
        crate::db_ownership::set_ownership_inject_for_test(Some(
            crate::db_ownership::DbOwnership::NotOwned,
        ));

        let error = match run_startup_hygiene(&cli, &ctx, &global_db).await {
            Err(error) => error,
            Ok(_) => panic!("wrong-target alias must refuse startup"),
        };

        assert!(
            error.to_string().contains("instead of canonical DB"),
            "{error}"
        );
        assert!(!canonical_db.exists(), "canonical DB must remain absent");
        assert!(
            !canonical_db.parent().unwrap().exists(),
            "canonical parent must remain absent"
        );
        assert_eq!(std::fs::read(&legacy_db).unwrap(), b"legacy-source");
        assert_eq!(std::fs::read(&wrong_db).unwrap(), b"wrong-target");
        assert_eq!(std::fs::read_link(&alias_db).unwrap(), wrong_db);
    }

    #[tokio::test]
    #[cfg(unix)]
    async fn copy_legacy_db_guarded_refuses_when_owned() {
        let dir = tempfile::tempdir().expect("tempdir");
        let src = dir.path().join("legacy.db");
        let dest = dir.path().join("global").join("memory.db");
        std::fs::write(&src, b"legacy-bytes").expect("seed legacy db");

        let _clear = ClearOwnershipInject;
        crate::db_ownership::set_ownership_inject_for_test(Some(
            crate::db_ownership::DbOwnership::Owned,
        ));

        let result = copy_legacy_db_guarded(&src, &dest).await;

        assert!(
            result.is_err(),
            "must refuse to copy a DB a live daemon holds open"
        );
        assert!(
            !dest.exists(),
            "refused copy must leave zero bytes at the destination"
        );
    }

    #[tokio::test]
    #[cfg(unix)]
    async fn copy_legacy_db_guarded_refuses_when_unknown() {
        let dir = tempfile::tempdir().expect("tempdir");
        let src = dir.path().join("legacy.db");
        let dest = dir.path().join("global").join("memory.db");
        std::fs::write(&src, b"legacy-bytes").expect("seed legacy db");

        let _clear = ClearOwnershipInject;
        crate::db_ownership::set_ownership_inject_for_test(Some(
            crate::db_ownership::DbOwnership::Unknown("lsof unavailable: test".to_string()),
        ));

        let result = copy_legacy_db_guarded(&src, &dest).await;

        let err = result.expect_err("undetermined ownership must refuse");
        assert!(
            err.to_string().contains("cannot determine"),
            "error must say ownership was undetermined, got: {err}"
        );
        assert!(
            !dest.exists(),
            "refused copy must leave zero bytes at the destination"
        );
    }

    #[tokio::test]
    #[cfg(unix)]
    async fn copy_legacy_db_guarded_proceeds_when_not_owned() {
        let dir = tempfile::tempdir().expect("tempdir");
        let src = dir.path().join("legacy.db");
        let dest = dir.path().join("global").join("memory.db");
        std::fs::write(&src, b"legacy-bytes").expect("seed legacy db");

        let _clear = ClearOwnershipInject;
        crate::db_ownership::set_ownership_inject_for_test(Some(
            crate::db_ownership::DbOwnership::NotOwned,
        ));

        let result = copy_legacy_db_guarded(&src, &dest).await;

        assert!(result.is_ok(), "not-owned must proceed: {result:?}");
        assert_eq!(
            std::fs::read(&dest).expect("dest must exist"),
            b"legacy-bytes"
        );
    }

    #[tokio::test]
    #[cfg(unix)]
    async fn copy_legacy_db_guarded_copies_wal_and_shm_sidecars_when_present() {
        // Crashed-WAL fixture: committed rows can live only in `-wal` until
        // checkpointed. Copying just the main file would silently drop them.
        let dir = tempfile::tempdir().expect("tempdir");
        let src = dir.path().join("legacy.db");
        let dest = dir.path().join("global").join("memory.db");
        std::fs::write(&src, b"legacy-main").expect("seed legacy main");
        std::fs::write(sidecar_path(&src, "-wal"), b"legacy-wal-rows").expect("seed legacy wal");
        std::fs::write(sidecar_path(&src, "-shm"), b"legacy-shm").expect("seed legacy shm");

        let _clear = ClearOwnershipInject;
        crate::db_ownership::set_ownership_inject_for_test(Some(
            crate::db_ownership::DbOwnership::NotOwned,
        ));

        let result = copy_legacy_db_guarded(&src, &dest).await;

        assert!(result.is_ok(), "not-owned must proceed: {result:?}");
        assert_eq!(std::fs::read(&dest).expect("dest main"), b"legacy-main");
        assert_eq!(
            std::fs::read(sidecar_path(&dest, "-wal")).expect("dest wal must exist"),
            b"legacy-wal-rows",
            "committed-only-in-WAL rows must not be silently dropped"
        );
        assert_eq!(
            std::fs::read(sidecar_path(&dest, "-shm")).expect("dest shm must exist"),
            b"legacy-shm"
        );
    }

    #[tokio::test]
    #[cfg(unix)]
    async fn copy_legacy_db_guarded_skips_absent_sidecars() {
        // No -wal/-shm on disk (already checkpointed / non-WAL source): the
        // copy must still succeed and must not fabricate sidecar files.
        let dir = tempfile::tempdir().expect("tempdir");
        let src = dir.path().join("legacy.db");
        let dest = dir.path().join("global").join("memory.db");
        std::fs::write(&src, b"legacy-main").expect("seed legacy main");

        let _clear = ClearOwnershipInject;
        crate::db_ownership::set_ownership_inject_for_test(Some(
            crate::db_ownership::DbOwnership::NotOwned,
        ));

        let result = copy_legacy_db_guarded(&src, &dest).await;

        assert!(result.is_ok(), "not-owned must proceed: {result:?}");
        assert!(!sidecar_path(&dest, "-wal").exists());
        assert!(!sidecar_path(&dest, "-shm").exists());
    }

    #[tokio::test]
    #[cfg(unix)]
    async fn copy_legacy_db_guarded_discards_partial_copy_when_sidecar_copy_fails() {
        // Force the -wal sidecar copy to fail (source is a directory, not a
        // regular file) after the main file has already been copied. The
        // whole destination — main + any sidecar already copied — must be
        // discarded; a main-file-only leftover would silently drop the rows
        // that only exist in -wal.
        let dir = tempfile::tempdir().expect("tempdir");
        let src = dir.path().join("legacy.db");
        let dest = dir.path().join("global").join("memory.db");
        std::fs::write(&src, b"legacy-main").expect("seed legacy main");
        std::fs::create_dir(sidecar_path(&src, "-wal")).expect("wal as directory");

        let _clear = ClearOwnershipInject;
        crate::db_ownership::set_ownership_inject_for_test(Some(
            crate::db_ownership::DbOwnership::NotOwned,
        ));

        let result = copy_legacy_db_guarded(&src, &dest).await;

        assert!(
            result.is_err(),
            "sidecar copy failure must fail the whole operation"
        );
        assert!(
            !dest.exists(),
            "partial main-file-only copy must be discarded on sidecar failure"
        );
        assert!(!sidecar_path(&dest, "-wal").exists());
        assert!(!sidecar_path(&dest, "-shm").exists());
    }

    // No test exercises the "cleanup itself fails" branch (the
    // `partial copy could not be fully discarded: ...` message) — there is
    // no cheap injection available for it. `unlink(2)`/`remove_file` needs
    // WRITE permission on the file's *parent directory*, not the file
    // itself; that is the exact same permission `tokio::fs::copy` needs to
    // *create* `dest` a few lines earlier in this same call. Chmod'ing
    // `dest`'s parent read-only before the call blocks the initial copy
    // (a different, earlier failure) rather than isolating a delete-only
    // failure; chmod'ing it read-only *between* the main-file copy and the
    // sidecar-failure trigger would require instrumenting the function
    // itself (a test seam this function does not otherwise need), and a
    // platform-specific immutable-file flag (e.g. macOS `chflags uchg`) is
    // not portable enough to call "cheap". The message-formatting logic
    // itself is straight-line and covered by review; if a cheap injection
    // seam is added to this function later, add the test alongside it.

    fn remember_cli(global_db: std::path::PathBuf, allow_schema_migration: bool) -> Cli {
        Cli {
            daemon: false,
            port: 6919,
            global_db: Some(global_db),
            project_db: None,
            no_project_db: true,
            allow_schema_migration,
            profile: None,
            gc_enabled: None,
            gc_initial_delay_secs: None,
            gc_interval_secs: None,
            command: Some(Commands::Remember {
                text: "#1131 cli migration authority regression".to_string(),
                tags: vec![],
                scope: None,
                project: None,
                path: None,
                importance: None,
                category: None,
                topic: None,
                domain: None,
                retention_policy: None,
                summary: None,
                force: true,
            }),
        }
    }

    #[test]
    fn remember_cli_requires_flag_to_migrate_stamped_older_db_in_process() {
        let _guard = crate::utils::global_test_lock()
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let fixture = crate::test_support::non_skipped_fixture_tempdir("cli-remember-schema-");
        let app_home = fixture.path().join("home");
        let global_db = app_home.join("global/memory.db");
        std::fs::create_dir_all(global_db.parent().expect("global DB parent"))
            .expect("create global DB parent");
        let _tachi_home = crate::test_support::EnvRestore::set_path("TACHI_HOME", &app_home);
        let _sigil_home = crate::test_support::EnvRestore::remove("SIGIL_HOME");
        let _app_home = crate::test_support::EnvRestore::remove("TACHI_APP_HOME");

        let server = MemoryServer::new(global_db.clone(), None).expect("seed current DB");
        drop(server);
        let conn = memcore::db::open_raw(&global_db).expect("open seeded DB");
        conn.execute_batch(&format!(
            "PRAGMA user_version = {}",
            memcore::db::migrations::EXPECTED_SCHEMA_VERSION - 1
        ))
        .expect("stamp older schema version");
        drop(conn);

        let err = tokio_main(remember_cli(global_db.clone(), false))
            .expect_err("remember without the flag must preserve OpenExisting + Deny");
        assert!(
            err.to_string().contains("refusing to migrate db schema"),
            "unexpected deny error: {err}"
        );

        let conn = memcore::db::open_raw(&global_db).expect("reopen denied DB");
        assert_eq!(
            memcore::db::migrations::read_schema_version(&conn).expect("read denied version"),
            memcore::db::migrations::EXPECTED_SCHEMA_VERSION - 1,
            "deny must not mutate the old schema stamp"
        );
        drop(conn);

        tokio_main(remember_cli(global_db.clone(), true))
            .expect("remember with the flag must migrate the isolated DB");

        let conn = memcore::db::open_raw(&global_db).expect("reopen migrated DB");
        assert_eq!(
            memcore::db::migrations::read_schema_version(&conn).expect("read migrated version"),
            memcore::db::migrations::EXPECTED_SCHEMA_VERSION,
            "allow must re-stamp the DB at the current schema version"
        );
    }

    fn backfill_fts_cli(global_db: std::path::PathBuf, allow_schema_migration: bool) -> Cli {
        Cli {
            daemon: false,
            port: 6919,
            global_db: Some(global_db),
            project_db: None,
            no_project_db: true,
            allow_schema_migration,
            profile: None,
            gc_enabled: None,
            gc_initial_delay_secs: None,
            gc_interval_secs: None,
            command: Some(Commands::BackfillFts {
                db: None,
                full: false,
                dry_run: false,
            }),
        }
    }

    /// Codex review (2026-07-17, checkpoint 3): the discrimination tests in
    /// `bootstrap::backfill::tests` call the private `run_backfill_*`
    /// functions directly with a hand-constructed `MigrationAuthority`,
    /// bypassing the actual CLI flag resolution (`initialize_startup_context`)
    /// and dispatch (`run_if_backfill_command`) #1181's contract is about.
    /// This test closes that gap for `backfill-fts` by going through the
    /// full `tokio_main` entry point, mirroring
    /// `remember_cli_requires_flag_to_migrate_stamped_older_db_in_process`
    /// immediately above.
    #[test]
    fn backfill_fts_cli_requires_flag_to_migrate_stamped_older_db_in_process() {
        let _guard = crate::utils::global_test_lock()
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let fixture = crate::test_support::non_skipped_fixture_tempdir("cli-backfill-fts-schema-");
        let app_home = fixture.path().join("home");
        let global_db = app_home.join("global/memory.db");
        std::fs::create_dir_all(global_db.parent().expect("global DB parent"))
            .expect("create global DB parent");
        let _tachi_home = crate::test_support::EnvRestore::set_path("TACHI_HOME", &app_home);
        let _sigil_home = crate::test_support::EnvRestore::remove("SIGIL_HOME");
        let _app_home = crate::test_support::EnvRestore::remove("TACHI_APP_HOME");

        let server = MemoryServer::new(global_db.clone(), None).expect("seed current DB");
        drop(server);
        let conn = memcore::db::open_raw(&global_db).expect("open seeded DB");
        conn.execute_batch(&format!(
            "PRAGMA user_version = {}",
            memcore::db::migrations::EXPECTED_SCHEMA_VERSION - 1
        ))
        .expect("stamp older schema version");
        drop(conn);

        let err = tokio_main(backfill_fts_cli(global_db.clone(), false))
            .expect_err("backfill-fts without the flag must preserve OpenExisting + Deny");
        assert!(
            err.to_string().contains("refusing to migrate db schema"),
            "unexpected deny error: {err}"
        );

        let conn = memcore::db::open_raw(&global_db).expect("reopen denied DB");
        assert_eq!(
            memcore::db::migrations::read_schema_version(&conn).expect("read denied version"),
            memcore::db::migrations::EXPECTED_SCHEMA_VERSION - 1,
            "deny must not mutate the old schema stamp"
        );
        drop(conn);

        tokio_main(backfill_fts_cli(global_db.clone(), true))
            .expect("backfill-fts with the flag must migrate the isolated DB, resolved through the real CLI flag path");

        let conn = memcore::db::open_raw(&global_db).expect("reopen migrated DB");
        assert_eq!(
            memcore::db::migrations::read_schema_version(&conn).expect("read migrated version"),
            memcore::db::migrations::EXPECTED_SCHEMA_VERSION,
            "allow must re-stamp the DB at the current schema version"
        );
    }

    #[test]
    fn primary_log_path_lives_under_app_home() {
        let app_home = std::path::Path::new("/tmp/tachi-app-home");
        assert_eq!(
            primary_log_path(app_home),
            app_home.join("logs").join("tachi.log")
        );
    }

    #[test]
    fn daily_distill_marker_path_lives_under_app_home() {
        let app_home = std::path::Path::new("/tmp/tachi-custom-home");
        assert_eq!(
            daily_distill_marker_path(app_home),
            app_home.join("foundry-runs").join(".last_distill_run")
        );
    }

    #[test]
    fn no_project_serve_detaches_launch_cwd_only_for_mcp_serve() {
        assert!(no_project_serve_detaches_launch_cwd(&Commands::Serve, true));
        assert!(!no_project_serve_detaches_launch_cwd(
            &Commands::Serve,
            false
        ));
        assert!(!no_project_serve_detaches_launch_cwd(
            &Commands::Stats,
            true
        ));
    }

    #[test]
    fn project_local_env_loading_skips_no_project_and_vault_commands() {
        assert!(!should_load_project_local_env(&Commands::Serve, true));
        assert!(!should_load_project_local_env(
            &Commands::Vault {
                action: tachi_bootstrap::cli::VaultAction::Status,
            },
            false
        ));
        assert!(should_load_project_local_env(&Commands::Serve, false));
    }

    #[test]
    #[cfg(unix)]
    fn vault_exec_does_not_create_project_alias_from_cwd() {
        let _guard = crate::utils::global_test_lock()
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let fixture = tempfile::tempdir().expect("vault exec fixture");
        let app_home = fixture.path().join("home");
        let repo = fixture.path().join("repo");
        std::fs::create_dir_all(repo.join(".git")).expect("create synthetic git root");

        let _tachi_home = crate::test_support::EnvRestore::set_path("TACHI_HOME", &app_home);
        let _sigil_home = crate::test_support::EnvRestore::remove("SIGIL_HOME");
        let _app_home = crate::test_support::EnvRestore::remove("TACHI_APP_HOME");
        let _tachi_root = crate::test_support::EnvRestore::remove("TACHI_ROOT");
        let _memory_db = crate::test_support::EnvRestore::remove("MEMORY_DB_PATH");
        let _cwd = crate::test_support::CwdRestore::set(&repo);

        let cli = Cli {
            daemon: false,
            port: 6919,
            global_db: Some(app_home.join("global/tachi-memory.db")),
            project_db: None,
            no_project_db: false,
            allow_schema_migration: false,
            profile: None,
            gc_enabled: None,
            gc_initial_delay_secs: None,
            gc_interval_secs: None,
            command: Some(Commands::Vault {
                action: tachi_bootstrap::cli::VaultAction::Exec {
                    stdin_password: false,
                    keychain: false,
                    password_file: None,
                    insecure_password_file: false,
                    consumer: None,
                    require: vec![],
                    // This regression test deliberately executes `/usr/bin/true`
                    // without a configured Vault to prove the CLI path does not
                    // derive a project alias from its cwd. Keep that synthetic
                    // credential-less path explicit now that production defaults
                    // to refusal.
                    allow_unauthenticated: true,
                    command: vec!["/usr/bin/true".to_string()],
                },
            }),
        };

        tokio_main(cli).expect("vault exec should run without project identity");

        assert!(
            !app_home.join("projects").exists(),
            "vault exec must not create a Plan C alias from its launch cwd"
        );
        assert!(
            !repo.join(".tachi").exists(),
            "vault exec must not initialize a repo-local project DB"
        );
    }

    #[test]
    fn manifest_startup_is_deferred_for_no_project_daemon_serve() {
        assert!(should_defer_manifest_startup(&Commands::Serve, true, true));
    }

    #[test]
    fn manifest_startup_stays_enabled_for_no_project_stdio_serve() {
        assert!(!should_defer_manifest_startup(
            &Commands::Serve,
            false,
            true
        ));
    }

    #[test]
    fn manifest_startup_stays_enabled_for_project_daemon_serve() {
        assert!(!should_defer_manifest_startup(
            &Commands::Serve,
            true,
            false
        ));
    }

    #[test]
    fn plan_c_symlink_refresh_skips_explicit_project_db() {
        let project_db = std::path::Path::new("/tmp/agent-workspace/memory.db");
        let git_root = std::path::Path::new("/tmp/repo");

        assert!(!should_refresh_plan_c_symlink(
            Some(project_db),
            Some(git_root),
            true
        ));
        assert!(should_refresh_plan_c_symlink(
            Some(project_db),
            Some(git_root),
            false
        ));
        assert!(!should_refresh_plan_c_symlink(None, Some(git_root), false));
        assert!(!should_refresh_plan_c_symlink(
            Some(project_db),
            None,
            false
        ));
    }

    #[cfg(unix)]
    #[test]
    fn restrict_file_permissions_sets_owner_only_mode() {
        use std::os::unix::fs::PermissionsExt;

        let temp = tempfile::NamedTempFile::new().expect("temp file");
        let path = temp.path();

        restrict_file_permissions(path).expect("set permissions");

        let meta = std::fs::metadata(path).expect("metadata");
        let mode = meta.permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "expected 0o600, got {mode:o}");
    }

    #[test]
    fn daily_distill_scheduler_stays_enabled_when_pipeline_is_disabled() {
        let _guard = crate::utils::global_test_lock()
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let previous = std::env::var("ENABLE_PIPELINE").ok();
        std::env::remove_var("ENABLE_PIPELINE");

        let temp = tempfile::tempdir().expect("tempdir");
        let global_db = temp.path().join("global.db");
        let project_db = temp.path().join("project").join("memory.db");
        std::fs::create_dir_all(project_db.parent().expect("project parent"))
            .expect("create project db parent");

        let server =
            crate::MemoryServer::new(global_db, Some(project_db)).expect("server with project db");
        assert!(
            !server.pipeline_enabled,
            "test precondition: pipeline should default to disabled"
        );
        assert!(daily_distill_scheduler_enabled(&server));

        match previous {
            Some(value) => std::env::set_var("ENABLE_PIPELINE", value),
            None => std::env::remove_var("ENABLE_PIPELINE"),
        }
    }

    #[test]
    fn daily_distill_scheduler_requires_project_db() {
        let _guard = crate::utils::global_test_lock()
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let previous = std::env::var("ENABLE_PIPELINE").ok();
        std::env::set_var("ENABLE_PIPELINE", "true");

        let temp = tempfile::tempdir().expect("tempdir");
        let server = crate::MemoryServer::new(temp.path().join("global.db"), None).expect("server");

        assert!(
            server.pipeline_enabled,
            "test precondition: pipeline enabled"
        );
        assert!(!daily_distill_scheduler_enabled(&server));

        match previous {
            Some(value) => std::env::set_var("ENABLE_PIPELINE", value),
            None => std::env::remove_var("ENABLE_PIPELINE"),
        }
    }

    struct HostProfileEnvFixture {
        _lock: std::sync::MutexGuard<'static, ()>,
        original_cwd: PathBuf,
        original_host_profile: Option<std::ffi::OsString>,
        original_fixture_key: Option<std::ffi::OsString>,
        home: tempfile::TempDir,
        app_home: tempfile::TempDir,
        repo: tempfile::TempDir,
    }

    impl HostProfileEnvFixture {
        fn new() -> Self {
            let lock = crate::utils::global_test_lock()
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            let original_cwd = std::env::current_dir().expect("cwd");
            let original_host_profile = std::env::var_os(crate::host_profile::HOST_PROFILE_ENV);
            let original_fixture_key = std::env::var_os("TACHI_TEST_HOST_PROFILE_FIXTURE");
            let home = tempfile::tempdir().expect("home");
            let app_home = tempfile::tempdir().expect("app_home");
            let repo = tempfile::tempdir().expect("repo");
            std::fs::create_dir_all(home.path().join(".secrets")).expect("secrets dir");
            std::fs::create_dir_all(home.path().join(".sigil")).expect("legacy home dir");
            std::fs::create_dir_all(repo.path().join(".tachi")).expect("repo tachi dir");
            std::fs::create_dir_all(repo.path().join(".sigil")).expect("repo sigil dir");
            std::env::set_current_dir(repo.path()).expect("set synthetic repo cwd");
            Self {
                _lock: lock,
                original_cwd,
                original_host_profile,
                original_fixture_key,
                home,
                app_home,
                repo,
            }
        }

        fn write_env(&self, path: PathBuf, body: &str) {
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent).expect("env parent");
            }
            std::fs::write(path, body).expect("write env file");
        }

        fn seed_elevated_sources(&self) {
            self.write_env(
                self.home.path().join(".secrets/master.env"),
                "TACHI_HOST_PROFILE=home_data\n",
            );
            self.write_env(
                self.home.path().join(".sigil/config.env"),
                "TACHI_HOST_PROFILE=home_data\n",
            );
            self.write_env(
                self.repo.path().join(".tachi/config.env"),
                "TACHI_HOST_PROFILE=home_data\nTACHI_TEST_HOST_PROFILE_FIXTURE=repo_local_loaded\n",
            );
            self.write_env(
                self.repo.path().join(".sigil/config.env"),
                "TACHI_HOST_PROFILE=home_data\n",
            );
            self.write_env(
                self.repo.path().join(".env"),
                "TACHI_HOST_PROFILE=home_data\n",
            );
            std::env::set_var(crate::host_profile::HOST_PROFILE_ENV, "home_data");
        }

        fn load(&self) {
            load_env_files(
                self.home.path(),
                self.app_home.path(),
                true,
                Some(self.repo.path()),
            );
        }
    }

    impl Drop for HostProfileEnvFixture {
        fn drop(&mut self) {
            let _ = std::env::set_current_dir(&self.original_cwd);
            match self.original_host_profile.as_ref() {
                Some(value) => std::env::set_var(crate::host_profile::HOST_PROFILE_ENV, value),
                None => std::env::remove_var(crate::host_profile::HOST_PROFILE_ENV),
            }
            match self.original_fixture_key.as_ref() {
                Some(value) => std::env::set_var("TACHI_TEST_HOST_PROFILE_FIXTURE", value),
                None => std::env::remove_var("TACHI_TEST_HOST_PROFILE_FIXTURE"),
            }
        }
    }

    #[test]
    fn app_home_is_only_host_profile_authority() {
        let fixture = HostProfileEnvFixture::new();
        fixture.seed_elevated_sources();
        fixture.write_env(
            fixture.app_home.path().join("config.env"),
            "TACHI_HOST_PROFILE=development\n",
        );

        fixture.load();

        assert_eq!(
            std::env::var(crate::host_profile::HOST_PROFILE_ENV).as_deref(),
            Ok("development")
        );
        assert_eq!(
            crate::host_profile::HostProfile::current().expect("profile"),
            crate::host_profile::HostProfile::Development
        );

        fixture.write_env(
            fixture.app_home.path().join("config.env"),
            "TACHI_HOST_PROFILE=home_data\n",
        );
        std::env::set_var(crate::host_profile::HOST_PROFILE_ENV, "development");
        fixture.write_env(
            fixture.repo.path().join(".tachi/config.env"),
            "TACHI_HOST_PROFILE=development\nTACHI_TEST_HOST_PROFILE_FIXTURE=repo_local_loaded\n",
        );
        fixture.load();
        assert_eq!(
            std::env::var(crate::host_profile::HOST_PROFILE_ENV).as_deref(),
            Ok("home_data")
        );
        assert_eq!(
            crate::host_profile::HostProfile::current().expect("profile"),
            crate::host_profile::HostProfile::HomeData
        );
    }

    #[test]
    fn missing_app_home_profile_rejects_inherited_and_repo_elevation() {
        let fixture = HostProfileEnvFixture::new();
        fixture.seed_elevated_sources();
        fixture.write_env(
            fixture.app_home.path().join("config.env"),
            "# intentionally missing TACHI_HOST_PROFILE\n",
        );

        fixture.load();

        assert!(
            std::env::var_os(crate::host_profile::HOST_PROFILE_ENV).is_none(),
            "missing app-home profile must leave the raw key absent"
        );
        assert_eq!(
            crate::host_profile::HostProfile::current().expect("default profile"),
            crate::host_profile::HostProfile::Development
        );
    }

    #[test]
    fn invalid_app_home_profile_is_not_repaired_by_repo_profile() {
        let fixture = HostProfileEnvFixture::new();
        fixture.seed_elevated_sources();
        fixture.write_env(
            fixture.app_home.path().join("config.env"),
            "TACHI_HOST_PROFILE=not-a-real-profile\n",
        );

        fixture.load();

        assert_eq!(
            std::env::var(crate::host_profile::HOST_PROFILE_ENV).as_deref(),
            Ok("not-a-real-profile")
        );
        let err = crate::host_profile::HostProfile::current().expect_err("invalid stays invalid");
        assert!(
            err.contains("invalid TACHI_HOST_PROFILE"),
            "expected invalid profile error, got {err}"
        );
    }

    #[test]
    fn project_local_non_host_env_still_loads() {
        let fixture = HostProfileEnvFixture::new();
        std::env::remove_var(crate::host_profile::HOST_PROFILE_ENV);
        std::env::remove_var("TACHI_TEST_HOST_PROFILE_FIXTURE");
        fixture.write_env(
            fixture.app_home.path().join("config.env"),
            "TACHI_HOST_PROFILE=development\n",
        );
        fixture.write_env(
            fixture.repo.path().join(".tachi/config.env"),
            "TACHI_TEST_HOST_PROFILE_FIXTURE=repo_local_loaded\n",
        );

        fixture.load();

        assert_eq!(
            std::env::var("TACHI_TEST_HOST_PROFILE_FIXTURE").as_deref(),
            Ok("repo_local_loaded"),
            "project-local non-host keys must still load"
        );
        assert_eq!(
            std::env::var(crate::host_profile::HOST_PROFILE_ENV).as_deref(),
            Ok("development")
        );
    }
}
