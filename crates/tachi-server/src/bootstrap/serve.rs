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
#[cfg(test)]
mod http_tests;
mod logging;
mod malformed_json_middleware;
mod runtime;

use self::backfill_commands::run_if_backfill_command;
use self::background::*;
use self::cli_commands::{run_if_a2a_command, run_if_agent_acp_command, run_pre_serve_command};
use self::daemon::serve_http_daemon;
#[cfg(test)]
pub(crate) use self::daemon::{daemon_http_router, observe_daemon_health_reads_for_test};
use self::logging::*;
use self::runtime::*;

fn no_project_serve_detaches_launch_cwd(command: &Commands, no_project_db: bool) -> bool {
    no_project_db && matches!(command, Commands::Serve)
}

fn should_load_project_local_env(command: &Commands, no_project_db: bool) -> bool {
    !no_project_db && !matches!(command, Commands::Vault { .. })
}

fn should_defer_manifest_startup(command: &Commands, daemon: bool, no_project_db: bool) -> bool {
    daemon && no_project_db && matches!(command, Commands::Serve)
}

fn is_a2a_sticky_cutover(command: &Commands) -> bool {
    matches!(
        command,
        Commands::A2a {
            action: tachi_bootstrap::cli::A2aAction::StickyCutover { .. }
        }
    )
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
    // Load provider configuration for explicit daemon and CLI startup.
    // `from_path` (not override): already-set process env — Cursor inject,
    // shell export, prior dotenv — wins over the file.
    let _ = dotenvy::from_path(app_home.join("secrets.env"));
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

fn workspace_home_selection_notice(
    source: crate::path_utils::TachiHomeSource,
    app_home: &Path,
) -> Option<String> {
    (source == crate::path_utils::TachiHomeSource::WorkspaceData).then(|| {
        format!(
            "[tachi-home] selected workspace home {} from cwd ancestry; set TACHI_HOME to pin a different home",
            app_home.display()
        )
    })
}

fn initialize_startup_observability(
    cli: &Cli,
    command: &Commands,
    app_home: &Path,
) -> Result<(), Box<dyn std::error::Error>> {
    // Unix retains early tracing; other platforms first refuse unsupported
    // startup modes using the fully loaded configuration.
    init_tracing(app_home);
    tracing::info!(
        version = env!("CARGO_PKG_VERSION"),
        app_home = %app_home.display(),
        "tachi tachi-server starting"
    );
    if no_project_serve_detaches_launch_cwd(command, cli.no_project_db) {
        let runtime_cwd = detach_launch_cwd_to_runtime(app_home).map_err(|error| {
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
    Ok(())
}

fn initialize_startup_context(cli: &Cli) -> Result<StartupContext, Box<dyn std::error::Error>> {
    // Load config from dotenv files (same as before)
    let home = dirs::home_dir().unwrap_or_else(|| PathBuf::from("."));
    let app_home_resolution = crate::path_utils::resolve_tachi_home();
    if let Some(notice) =
        workspace_home_selection_notice(app_home_resolution.source, &app_home_resolution.path)
    {
        eprintln!("{notice}");
    }
    let app_home = app_home_resolution.path;

    let command = cli.command.clone().unwrap_or(Commands::Serve);
    if let Commands::Migrate {
        apply,
        rename_legacy,
        offline,
        ..
    } = &command
    {
        if (*rename_legacy && *apply != *offline) || (!*rename_legacy && *offline) {
            return Err("filename conversion requires --rename-legacy --apply --offline together (or a plan without --apply/--offline)".into());
        }
    }
    // A plan is genuinely zero-write, including startup logging. In
    // particular it must not create the logs directory in an uninitialized
    // home merely to report an on-disk filename state.
    let read_only_migrate_plan = matches!(&command, Commands::Migrate { apply: false, .. });
    #[cfg(unix)]
    if !read_only_migrate_plan {
        initialize_startup_observability(cli, &command, &app_home)?;
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

    #[cfg(not(unix))]
    {
        // Refuse unsupported singleton ownership before tracing or cwd setup.
        if matches!(command, Commands::Serve) {
            return Err(crate::daemon_lock::unsupported_daemon_lock_error().into());
        }
        if !read_only_migrate_plan {
            initialize_startup_observability(cli, &command, &app_home)?;
        }
    }

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
        ctx.app_home
            .join("global")
            .join(memcore::MEMORY_DB_FILENAME)
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
                if !bypass && !is_a2a_sticky_cutover(&ctx.command) {
                    if let Err(err) = m.check_writable(&entry.path) {
                        // Before failing hard, check if any live process
                        // is already holding this DB — that is the most
                        // common cause of a false-positive WalOrphan
                        // classification. Two checks:
                        //   (a) HTTP daemon pid file (~/.tachi/daemon.pid),
                        //       only exists when started via `tachi --daemon`;
                        //   (b) `lsof` on the DB path itself — covers
                        //       historical MCP instances that never wrote a pid
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

    if !is_a2a_sticky_cutover(&ctx.command) && !matches!(ctx.command, Commands::Migrate { .. }) {
        let Some(parent) = global_db_path.parent() else {
            return Ok(global_db_path);
        };
        tokio::fs::create_dir_all(parent).await?;
    }

    Ok(global_db_path)
}

async fn migrate_legacy_global_db_if_needed(
    cli: &Cli,
    ctx: &StartupContext,
    global_db_path: &Path,
    daemon_lock: Option<&crate::daemon_lock::DaemonLock>,
) -> Result<(), Box<dyn std::error::Error>> {
    let default_global = ctx
        .app_home
        .join("global")
        .join(memcore::MEMORY_DB_FILENAME);
    if cli.global_db.is_some()
        || std::env::var_os("MEMORY_DB_PATH").is_some()
        || global_db_path != default_global
        || is_a2a_sticky_cutover(&ctx.command)
        || matches!(ctx.command, Commands::Migrate { .. })
    {
        return Ok(());
    }
    let destination_exists = || match std::fs::symlink_metadata(global_db_path) {
        Ok(_) => Ok(true),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error),
    };
    if destination_exists()? {
        return Ok(());
    }
    // These pre-layout files remain eligible for the existing guarded copy.
    // A memory.db already inside global/ needs explicit offline conversion.
    let legacy_candidates = [
        ctx.app_home.join(memcore::LEGACY_MEMORY_DB_FILENAME),
        ctx.home
            .join(".sigil/global")
            .join(memcore::LEGACY_MEMORY_DB_FILENAME),
        ctx.home
            .join(".sigil")
            .join(memcore::LEGACY_MEMORY_DB_FILENAME),
    ];
    if !legacy_candidates.iter().any(|path| path.exists()) {
        return Ok(());
    }

    // A daemon already owns this guard before any database copy or open. A
    // non-daemon migration must take the same destination guard, otherwise a
    // CLI starting beside the daemon could overwrite its newly opened DB.
    // HTTP daemon ownership is unsupported on non-Unix platforms; preserve
    // their existing non-daemon migration behavior.
    #[cfg(unix)]
    let _migration_lock = if daemon_lock.is_none() {
        Some(daemon::acquire_startup_daemon_lock(
            &ctx.app_home,
            global_db_path,
        )?)
    } else {
        None
    };
    #[cfg(not(unix))]
    let _ = daemon_lock;
    // Recheck after ownership: the prior owner may have completed migration
    // between the first existence check and our acquisition.
    if destination_exists()? {
        return Ok(());
    }
    for legacy in legacy_candidates {
        if legacy.exists() {
            copy_legacy_db_guarded(&legacy, global_db_path).await?;
            eprintln!(
                "Migrated legacy DB: {} -> {}",
                legacy.display(),
                global_db_path.display()
            );
            break;
        }
    }
    Ok(())
}

async fn run_startup_hygiene(
    cli: &Cli,
    ctx: &StartupContext,
    global_db_path: &PathBuf,
) -> Result<Option<StartupHygiene>, Box<dyn std::error::Error>> {
    // Operator cutover must open exactly the already-resolved global DB and
    // exit before project aliases, legacy copies, GC, or server startup can
    // mutate unrelated state.
    if run_if_agent_acp_command(&ctx.command, global_db_path)? {
        return Ok(None);
    }
    if run_if_a2a_command(&ctx.command, &ctx.app_home, global_db_path)? {
        return Ok(None);
    }

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
        // Pre-.tachi-layout legacy source. An already-standard-layout
        // `.tachi/memory.db` is NOT renamed on open: since 2.0 (#1132) an
        // ordinary MemoryStore::open refuses a real legacy filename, and
        // conversion requires `tachi migrate --rename-legacy --apply --offline`.
        let project_legacy = root.join(".sigil").join(memcore::LEGACY_MEMORY_DB_FILENAME);

        if project_legacy.exists() && !project_default.exists() {
            legacy_project_copy = Some((project_legacy, project_default.clone()));
        }

        Some(project_default)
    } else {
        None
    };

    // Both schema-plan and filename-plan must reach their CLI handler before
    // Plan-C alias creation or pre-layout legacy copying can mutate DB paths.
    if matches!(ctx.command, Commands::Migrate { .. }) {
        run_pre_serve_command(
            &ctx.command,
            &ctx.home,
            &ctx.app_home,
            global_db_path,
            project_db_path.as_ref(),
            ctx.git_root.as_ref(),
            &ctx.schema_migration,
        )
        .await?;
        return Ok(None);
    }

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
    match crate::dispatch_ops::signature_evidence::seed_signature_taxonomy_evidence(&server) {
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
                    "unsupported tool profile '{raw_profile}'; ordinary agents use the default entry. Ops/Admin selectors are retired; use supported owner CLI commands. Legacy standard, observe, remember, coordinate and delegate restrictions remain supported"
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

    // #1681 D7 PR-B: record what the env chains resolved to as
    // `catalog_source='env'` deployment rows. This is the serve path — daemon
    // — which is where a long-lived process's resolved config is
    // worth describing; CLI one-shots have already returned in
    // `run_pre_serve_command` and deliberately do not import.
    //
    // Non-fatal, like the seeds above: the import is idempotent, so a boot
    // that fails to record leaves the previous rows intact and the next boot
    // re-attempts. A *refusal* (userinfo in a base URL) is the one case an
    // operator has to act on, so it is logged at every boot rather than once.
    match crate::provider_config::import_env_catalog_deployments(&server) {
        Ok(summary) => {
            if summary.changed > 0 {
                eprintln!(
                    "[catalog] env deployment rows: {} imported, {} changed",
                    summary.rows, summary.changed
                );
            }
            if let Some(reason) = summary.embedding_refused {
                eprintln!(
                    "[catalog] embedding deployment row not recorded (configuration refused): {reason}"
                );
            }
        }
        Err(err) => eprintln!(
            "[catalog] env deployment import failed (previous rows kept, will retry next boot; boot continues): {err}"
        ),
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
    daemon_lock: Option<&crate::daemon_lock::DaemonLock>,
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
    eprintln!("Transport: HTTP daemon on port {}", cli.port);
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

    let serve_result = serve_http_daemon(
        server,
        ctx.app_home.clone(),
        global_db_path.to_path_buf(),
        hygiene.project_db_path.clone(),
        cli.port,
        daemon_lock.ok_or("daemon transport requires startup singleton ownership")?,
    )
    .await;

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
    Ok(())
}

pub(super) fn tokio_main(cli: Cli) -> Result<(), Box<dyn std::error::Error>> {
    if !cli.daemon && matches!(cli.command.as_ref(), None | Some(Commands::Serve)) {
        return Err("Tachi MCP stdio has been retired. Start the resident HTTP server with `tachi --daemon --port 6919 serve`, then configure a native Streamable HTTP client at http://127.0.0.1:6919/mcp. Existing command/args MCP entries must be replaced with an HTTP URL; no stdin proxy or automatic daemon fallback is available.".into());
    }
    let mut owner = StartupRuntime::new()?;
    owner
        .runtime
        .block_on(tokio_main_inner(cli, &mut owner.daemon_lock))
}

async fn tokio_main_inner(
    cli: Cli,
    daemon_lock: &mut Option<crate::daemon_lock::DaemonLock>,
) -> Result<(), Box<dyn std::error::Error>> {
    if let Some(Commands::Mcp {
        action: tachi_bootstrap::cli::McpAction::Task(args),
    }) = cli.command.as_ref()
    {
        // A task request is only a client of an already-running daemon.
        // Keep it before tracing, store resolution, and startup hygiene.
        return super::cli_tool::run_mcp_task_command(args).await;
    }
    if let Some(Commands::RecallCoverage(args)) = cli.command.as_ref() {
        // Keep this in-process read-only operation before all startup writes.
        return super::recall_coverage_cli::run_recall_coverage_command(
            &args.db,
            args.top_k,
            args.candidates_per_channel,
            args.limit,
            args.equivalence_file.as_deref(),
            args.human,
        );
    }

    // Refuse unsupported write modes before startup can create state.
    #[cfg(not(unix))]
    {
        if matches!(
            cli.command.as_ref(),
            Some(Commands::Wiki {
                action: tachi_bootstrap::cli::WikiAction::Export { .. }
            })
        ) {
            crate::wiki_ops::ensure_wiki_export_supported()?;
        }
        if cli.daemon && matches!(cli.command.as_ref(), None | Some(Commands::Serve)) {
            return Err(crate::daemon_lock::unsupported_daemon_lock_error().into());
        }
    }

    let ctx = initialize_startup_context(&cli)?;
    let global_db_path = resolve_global_db(&cli, &ctx).await?;
    *daemon_lock = if cli.daemon && matches!(ctx.command, Commands::Serve) {
        Some(daemon::acquire_startup_daemon_lock(
            &ctx.app_home,
            &global_db_path,
        )?)
    } else {
        None
    };
    migrate_legacy_global_db_if_needed(&cli, &ctx, &global_db_path, daemon_lock.as_ref()).await?;
    let Some(hygiene) = run_startup_hygiene(&cli, &ctx, &global_db_path).await? else {
        return Ok(());
    };

    let state = build_server_state(&cli, &ctx, &global_db_path, &hygiene)?;
    start_server_transport(
        &cli,
        &ctx,
        &global_db_path,
        &hygiene,
        state,
        daemon_lock.as_ref(),
    )
    .await
}

#[cfg(test)]
mod tests;
