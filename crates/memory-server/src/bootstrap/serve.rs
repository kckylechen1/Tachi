use super::{print_pretty_json, DEFAULT_STANDARD_PROFILE_NOTICE};
use crate::kanban::{gc_expired_kanban_cards, DEFAULT_KANBAN_GC_MAX_AGE_DAYS};
use crate::mcp_proxy::filter_mcp_tools_by_permissions;
use crate::server_state::MemoryServer;
use crate::utils::{find_project_git_root, lock_or_recover, parse_env_bool, parse_env_u64};
use chrono::Utc;
use memory_core::MemoryStore;
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

fn should_load_project_local_env(no_project_db: bool) -> bool {
    !no_project_db
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
    let _ = dotenvy::from_path_override(app_home.join("config.env"));
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
}

#[tokio::main]
pub(super) async fn tokio_main(cli: Cli) -> Result<(), Box<dyn std::error::Error>> {
    // Load config from dotenv files (same as before)
    let home = dirs::home_dir().unwrap_or_else(|| PathBuf::from("."));
    let expand_user_path = |raw: &str| {
        if raw == "~" {
            home.clone()
        } else if let Some(rest) = raw.strip_prefix("~/") {
            home.join(rest)
        } else {
            PathBuf::from(raw)
        }
    };

    let app_home = crate::path_utils::tachi_home();

    // PR7 — install the tracing sink before doing anything else so early errors
    // (config load, manifest resolution, daemon bind) are captured.
    init_tracing(&app_home);
    tracing::info!(
        version = env!("CARGO_PKG_VERSION"),
        app_home = %app_home.display(),
        "tachi memory-server starting"
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
    let load_project_local_env = should_load_project_local_env(cli.no_project_db);
    let defer_manifest_startup =
        should_defer_manifest_startup(&command, cli.daemon, cli.no_project_db);
    let git_root = if load_project_local_env {
        find_project_git_root()
    } else {
        None
    };

    let expand_cli_path = |raw: &PathBuf| expand_user_path(raw.to_string_lossy().as_ref());

    load_env_files(
        &home,
        &app_home,
        load_project_local_env,
        git_root.as_deref(),
    );

    // Resolve global DB path
    let global_db_path = if let Some(p) = cli.global_db.as_ref() {
        expand_cli_path(p)
    } else if let Ok(p) = std::env::var("MEMORY_DB_PATH") {
        expand_user_path(&p)
    } else {
        let default_global = app_home.join("global/memory.db");
        // Migration: move legacy DBs into ${TACHI_HOME}/global/memory.db
        let legacy_candidates = vec![
            app_home.join("memory.db"),
            home.join(".sigil/global/memory.db"),
            home.join(".sigil/memory.db"),
        ];
        if !default_global.exists() {
            for legacy in legacy_candidates {
                if legacy.exists() {
                    if let Some(parent) = default_global.parent() {
                        tokio::fs::create_dir_all(parent).await?;
                    }
                    tokio::fs::copy(&legacy, &default_global).await?;
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
    let manifest_path = app_home.join("manifest.json");
    // PR-2: hygiene pass before any manifest consumer reads. Idempotent.
    // Failures are logged and swallowed — startup must never block on GC.
    if defer_manifest_startup {
        tracing::info!(
            target: "tachi::manifest::startup",
            "--daemon --no-project-db serve deferred manifest startup hygiene"
        );
    } else if matches!(command, Commands::Serve) && manifest_path.exists() {
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
    let manifest_opt = if !defer_manifest_startup && manifest_path.exists() {
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
                            &app_home,
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

    // Backfill commands need async LLM clients, so handle them before generic CLI dispatch.
    if run_if_backfill_command(&command, &home, &global_db_path).await? {
        return Ok(());
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
    let project_db_path = if cli.no_project_db {
        if cli.project_db.is_some() {
            eprintln!("--project-db is ignored because --no-project-db is set");
        }
        None
    } else if let Some(p) = cli.project_db.as_ref() {
        Some(expand_cli_path(p))
    } else if let Some(root) = git_root.as_ref() {
        let project_default = root.join(".tachi/memory.db");
        let project_legacy = root.join(".sigil/memory.db");

        if project_legacy.exists() && !project_default.exists() {
            if let Some(parent) = project_default.parent() {
                tokio::fs::create_dir_all(parent).await?;
            }
            tokio::fs::copy(&project_legacy, &project_default).await?;
            eprintln!(
                "Migrated legacy project DB: {} -> {}",
                project_legacy.display(),
                project_default.display()
            );
        }

        Some(project_default)
    } else {
        None
    };

    // Plan C: link <tachi_home>/projects/<sanitized-dir>/memory.db -> repo-local DB.
    // Explicit project DBs are caller-owned (for example embedded agent workspaces)
    // and must not rewrite the repo's global named-project alias.
    if should_refresh_plan_c_symlink(
        project_db_path.as_deref(),
        git_root.as_deref(),
        explicit_project_db,
    ) {
        if let (Some(db_path), Some(root)) = (project_db_path.as_ref(), git_root.as_ref()) {
            if let crate::path_utils::PlanCLinkOutcome::SplitBrain(issue) =
                crate::path_utils::ensure_plan_c_symlink(db_path, root)
            {
                eprintln!("[!] {}", issue.warning_message());
            }
        }
    }

    if let Commands::Distill { action } = &command {
        use tachi_bootstrap::cli::DistillAction;
        let DistillAction::Run { db } = action;
        let target_project = db
            .clone()
            .map(|p| expand_user_path(p.to_string_lossy().as_ref()))
            .or(project_db_path.clone())
            .ok_or_else(|| {
                "distill run requires a project DB: pass --project-db PATH or `distill run --db PATH`"
                    .to_string()
            })?;
        if !target_project.exists() {
            return Err(format!("project DB not found: {}", target_project.display()).into());
        }
        let server = MemoryServer::new(global_db_path.clone(), Some(target_project.clone()))?;
        let report = crate::foundry_runtime_ops::run_daily_batch_distill(&server).await?;
        print_pretty_json(&serde_json::to_value(report)?)?;
        return Ok(());
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
        &command,
        &home,
        &app_home,
        &global_db_path,
        project_db_path.as_ref(),
        git_root.as_ref(),
    )
    .await?
    {
        return Ok(());
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

    // An explicit TACHI_PROJECT pin wins over the path-derived (git-hash) name so
    // the label injected into proxied WRITES matches the label used for reads —
    // otherwise briefing reads the pinned library while proxy writes still land
    // in the git-hash library (a silent read/write split). Only bind a label
    // when a project DB is bound, preserving the prior "global-only ⇒ no label".
    let client_project_name = project_db_path.as_deref().and_then(|p| {
        crate::memory_search_ops::client_project_precedence(
            crate::memory_search_ops::explicit_workspace_project(),
            crate::memory_search_ops::named_project_from_db_path(p),
            crate::memory_search_ops::resolve_workspace_named_project(),
        )
    });

    if !cli.daemon {
        if let Some(info) =
            stdio::ensure_stdio_proxy_daemon(&app_home, &global_db_path, project_db_path.as_deref())
                .await
        {
            if stdio::proxy_can_preserve_project_context(
                &info,
                &global_db_path,
                project_db_path.as_deref(),
                client_project_name.as_deref(),
            ) {
                eprintln!(
                    "[stdio-proxy] forwarding stdio MCP to daemon {} (project={})",
                    info.url,
                    client_project_name.as_deref().unwrap_or("<none>")
                );
                stdio::serve_stdio_proxy(
                    info,
                    app_home.clone(),
                    global_db_path.clone(),
                    project_db_path.clone(),
                    client_project_name,
                )
                .await?;
                return Ok(());
            }
            eprintln!(
                "[stdio-proxy] local fallback: project DB {} has no named-project route and daemon project scope differs",
                project_db_path
                    .as_ref()
                    .map(|path| path.display().to_string())
                    .unwrap_or_else(|| "<none>".to_string())
            );
        }
    }

    if cli.daemon {
        std::env::set_var("TACHI_DAEMON", "1");
    } else {
        std::env::remove_var("TACHI_DAEMON");
    }

    let server = MemoryServer::new(global_db_path.clone(), project_db_path.clone())?;
    let recovered = crate::dispatch_ops::recover_orphaned_dispatch_runs();
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
        match tachi_hub::parse_tool_profile(raw_profile) {
            Some(profile) => server.set_tool_profile(Some(profile)),
            None => eprintln!(
                "Ignoring unknown tool profile '{}'; expected observe | remember | coordinate | operate | admin or a compatible host alias",
                raw_profile
            ),
        }
    } else {
        eprintln!("{DEFAULT_STANDARD_PROFILE_NOTICE}");
    }

    // Auto-unlock vault from macOS Keychain (service: tachi-vault, account: default)
    crate::provider_config::bootstrap_provider_runtime(&server);
    match server.llm.provider_secret_count() {
        0 => eprintln!("[provider] no provider keys materialized (Vault locked or empty)"),
        n => eprintln!("[provider] {n} provider key(s) ready for LLM/embed"),
    }

    if embedded_mcp_facade() {
        eprintln!("[embedded-mcp] owner background tasks disabled; forwarding to scoped daemon");
    } else {
        spawn_idle_connection_cleanup(&server);
        spawn_wal_checkpoint(&server);
        spawn_background_gc(&server, gc_enabled, gc_initial_delay_secs, gc_interval_secs);
        run_startup_integrity_checks(&server, project_db_path.is_some())?;
        load_cached_hub_tools(&server);
        report_pipeline_and_spawn_daily_distill(&server, &app_home);
    }

    eprintln!("Starting Tachi MCP Server v{}", env!("CARGO_PKG_VERSION"));
    eprintln!(
        "Transport: {}",
        if cli.daemon {
            format!("HTTP daemon on port {}", cli.port)
        } else {
            "stdio".to_string()
        }
    );
    eprintln!("Global DB: {}", global_db_path.display());
    if let Some(ref p) = project_db_path {
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

    if cli.daemon {
        serve_http_daemon(
            server,
            app_home.clone(),
            global_db_path.clone(),
            project_db_path.clone(),
            cli.port,
        )
        .await?;
    } else {
        serve_stdio(server).await?;
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

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
    fn project_local_env_loading_is_disabled_for_no_project_db() {
        assert!(!should_load_project_local_env(true));
        assert!(should_load_project_local_env(false));
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
}
