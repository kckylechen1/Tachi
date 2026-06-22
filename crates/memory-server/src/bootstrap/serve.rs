use super::*;

fn primary_log_path(app_home: &std::path::Path) -> std::path::PathBuf {
    app_home.join("logs").join("tachi.log")
}

fn init_tracing(app_home: &std::path::Path) {
    use std::io::Write as _;
    use tracing_subscriber::{fmt, layer::SubscriberExt, util::SubscriberInitExt, EnvFilter};

    let env_filter = EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| EnvFilter::new("info,memory_server=info,rmcp=warn"));

    // Try the configured app home first, then /tmp, then bare stderr.
    let primary = primary_log_path(app_home);
    let fallback = std::path::PathBuf::from("/tmp/tachi.log");

    let opener = |path: &std::path::Path| -> Option<std::fs::File> {
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .mode(0o600)
                .open(path)
                .ok()
        }
        #[cfg(not(unix))]
        {
            std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(path)
                .ok()
        }
    };

    let (writer, sink_label): (Box<dyn std::io::Write + Send + Sync>, String) =
        if let Some(file) = opener(&primary) {
            (Box::new(file), primary.display().to_string())
        } else if let Some(file) = opener(&fallback) {
            (Box::new(file), fallback.display().to_string())
        } else {
            (Box::new(std::io::stderr()), "stderr".to_string())
        };

    // Wrap the writer behind Mutex so MakeWriter can hand out shared refs.
    let shared: std::sync::Arc<std::sync::Mutex<Box<dyn std::io::Write + Send + Sync>>> =
        std::sync::Arc::new(std::sync::Mutex::new(writer));

    struct SharedWriter(std::sync::Arc<std::sync::Mutex<Box<dyn std::io::Write + Send + Sync>>>);
    impl std::io::Write for SharedWriter {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.0
                .lock()
                .map_err(|_| {
                    std::io::Error::new(std::io::ErrorKind::Other, "log writer mutex poisoned")
                })?
                .write(buf)
        }
        fn flush(&mut self) -> std::io::Result<()> {
            self.0
                .lock()
                .map_err(|_| {
                    std::io::Error::new(std::io::ErrorKind::Other, "log writer mutex poisoned")
                })?
                .flush()
        }
    }

    let make_writer = move || SharedWriter(shared.clone());

    let file_layer = fmt::Layer::new()
        .with_writer(make_writer)
        .with_ansi(false)
        .with_target(true);

    // Best-effort registration. A second call (e.g. from a test) becomes a
    // no-op because `set_global_default` was already set.
    let _ = tracing_subscriber::registry()
        .with(env_filter)
        .with(file_layer)
        .try_init();

    // Mirror the sink choice to stderr so operators can find their logs.
    let _ = writeln!(std::io::stderr(), "tachi: logging to {sink_label}");
}

/// Best-effort restrict a file to owner-read/write (0o600) on Unix.
#[cfg(test)]
#[cfg(unix)]
fn restrict_file_permissions(path: &std::path::Path) -> Result<(), std::io::Error> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
}

/// Liveness probe for the manifest self-lock fix: returns true iff some
/// process *other than us* currently holds the file at `db_path` open.
/// Uses `lsof` on Unix (best-effort: missing binary, non-zero exit, or
/// non-Unix platform → returns false, falling back to the prior pid-file
/// check). Critical for the stdio MCP case where the holder never wrote
/// `~/.tachi/daemon.pid`.
fn db_path_held_by_other_process(db_path: &str) -> bool {
    #[cfg(unix)]
    {
        use std::process::Command;
        let our_pid = std::process::id().to_string();
        // -t prints PIDs, one per line. -F p would also work but -t is portable.
        let output = Command::new("lsof")
            .arg("-t")
            .arg("--")
            .arg(db_path)
            .output();
        match output {
            Ok(o) if o.status.success() => {
                let stdout = String::from_utf8_lossy(&o.stdout);
                stdout
                    .lines()
                    .any(|line| line.trim() != our_pid && !line.trim().is_empty())
            }
            _ => false,
        }
    }
    #[cfg(not(unix))]
    {
        let _ = db_path;
        false
    }
}

fn daily_distill_scheduler_enabled(server: &crate::MemoryServer) -> bool {
    server.has_project_db()
}

fn daily_distill_marker_path(app_home: &std::path::Path) -> std::path::PathBuf {
    app_home.join("foundry-runs").join(".last_distill_run")
}

/// Idle window after which a detached daemon self-terminates. `None` disables
/// the reaper (env value `0`). Defaults to 30 minutes.
fn daemon_idle_timeout() -> Option<std::time::Duration> {
    let secs = std::env::var("TACHI_DAEMON_IDLE_TIMEOUT_SECS")
        .ok()
        .and_then(|v| v.trim().parse::<u64>().ok())
        .unwrap_or(1800);
    (secs > 0).then(|| std::time::Duration::from_secs(secs))
}

/// Resolves when the launching parent has exited. An orphaned stdio MCP server
/// has no host left to talk to, so it should exit rather than linger as an
/// init/launchd-reparented process. Unix-only; default on, disable with
/// `TACHI_STDIO_PARENT_DEATH_EXIT=0`. On non-Unix (or when the process was
/// already parentless at startup) it never resolves, leaving the other
/// `select!` arms in control.
async fn wait_for_parent_death() {
    #[cfg(unix)]
    {
        let disabled = std::env::var("TACHI_STDIO_PARENT_DEATH_EXIT")
            .map(|v| matches!(v.trim(), "0" | "false" | "no" | "off"))
            .unwrap_or(false);
        // SAFETY: getppid() is always safe — it reads the caller's parent pid.
        let original_ppid = unsafe { libc::getppid() };
        if disabled || original_ppid <= 1 {
            // Already parentless (or opted out): nothing to watch.
            std::future::pending::<()>().await;
            return;
        }
        loop {
            tokio::time::sleep(std::time::Duration::from_secs(5)).await;
            // Reparented to init/launchd (pid 1) ⇒ the host died.
            if unsafe { libc::getppid() } == 1 {
                return;
            }
        }
    }
    #[cfg(not(unix))]
    {
        std::future::pending::<()>().await;
    }
}

/// Resolves on SIGTERM (Unix). Hosts and the version-skew daemon-replace path
/// send SIGTERM, not SIGINT, so both serve loops must catch it for a graceful
/// shutdown (flush + pid-file cleanup) instead of an abrupt default kill. Never
/// resolves on non-Unix or if the handler can't be installed.
async fn sigterm() {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{signal, SignalKind};
        match signal(SignalKind::terminate()) {
            Ok(mut s) => {
                s.recv().await;
            }
            Err(_) => std::future::pending::<()>().await,
        }
    }
    #[cfg(not(unix))]
    {
        std::future::pending::<()>().await;
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
    let git_root = find_project_git_root();

    let expand_cli_path = |raw: &PathBuf| expand_user_path(raw.to_string_lossy().as_ref());

    let _ = dotenvy::from_path(home.join(".secrets/master.env"));
    let _ = dotenvy::from_path_override(app_home.join("config.env"));
    let _ = dotenvy::from_path_override(PathBuf::from(".tachi/config.env"));
    // Backward compatibility with old Sigil paths
    let _ = dotenvy::from_path_override(home.join(".sigil/config.env"));
    let _ = dotenvy::from_path_override(PathBuf::from(".sigil/config.env"));

    // Project-local dotenv support (non-overriding):
    // - current working directory .env
    // - git root .env (if different from cwd)
    if let Ok(cwd) = std::env::current_dir() {
        let _ = dotenvy::from_path(cwd.join(".env"));
        if let Some(root) = git_root.as_ref() {
            if root != &cwd {
                let _ = dotenvy::from_path(root.join(".env"));
            }
        }
    }

    let command = cli.command.clone().unwrap_or(Commands::Serve);

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
    if matches!(command, Commands::Serve) && manifest_path.exists() {
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
    let manifest_opt = if manifest_path.exists() {
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

    // BackfillVectors needs async (LLM client), handle it here before sync dispatch
    if let Commands::BackfillVectors {
        db,
        project,
        batch_size,
        dry_run,
        include_cache,
    } = &command
    {
        let target_path = if let Some(p) = db {
            expand_user_path(p.to_string_lossy().as_ref())
        } else if let Some(project) = project {
            let path = crate::path_utils::plan_c_global_db_path(project);
            if !path.exists() {
                return Err(format!(
                    "named project DB not found for '{project}': {}",
                    path.display()
                )
                .into());
            }
            path
        } else {
            global_db_path.clone()
        };
        return super::backfill::run_backfill_vectors(
            &target_path,
            &global_db_path,
            *batch_size,
            *dry_run,
            *include_cache,
        )
        .await;
    }

    if let Commands::BackfillSummaries { db, dry_run } = &command {
        let target_path = if let Some(p) = db {
            expand_user_path(p.to_string_lossy().as_ref())
        } else {
            global_db_path.clone()
        };
        return super::backfill::run_backfill_summaries(&target_path, *dry_run).await;
    }

    if let Commands::BackfillMetadata { db, dry_run } = &command {
        let target_path = if let Some(p) = db {
            expand_user_path(p.to_string_lossy().as_ref())
        } else {
            global_db_path.clone()
        };
        return super::backfill::run_backfill_metadata(&target_path, *dry_run).await;
    }

    if let Commands::BackfillFts { db, full, dry_run } = &command {
        let target_path = if let Some(p) = db {
            expand_user_path(p.to_string_lossy().as_ref())
        } else {
            global_db_path.clone()
        };
        return super::backfill::run_backfill_fts(&target_path, *full, *dry_run).await;
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
    if let Some(ref db_path) = project_db_path {
        if let Some(root) = git_root.as_ref() {
            if let crate::path_utils::PlanCLinkOutcome::SplitBrain(issue) =
                crate::path_utils::ensure_plan_c_symlink(db_path, root)
            {
                eprintln!("[!] {}", issue.warning_message());
            }
        }
    }

    if let Commands::Distill { action } = &command {
        use crate::cli::DistillAction;
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
        super::print_pretty_json(&serde_json::to_value(report)?)?;
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

    if let Commands::Setup {
        json,
        interactive,
        non_interactive,
    } = &command
    {
        return super::setup::run_setup_command(
            *json,
            *interactive,
            *non_interactive,
            &home,
            &app_home,
            &global_db_path,
            project_db_path.as_ref(),
            git_root.as_ref(),
        )
        .await;
    }

    if let Commands::Tidy {
        json,
        apply,
        dry_run,
        execute,
        yes,
        target_db,
    } = &command
    {
        let mut roots = vec![
            app_home.clone(),
            home.join(".sigil"),
            home.join(".gemini"),
            home.join(".openclaw"),
        ];
        if let Some(root) = git_root.as_ref() {
            roots.push(root.clone());
        }
        return super::tidy::run_tidy_command(
            *json,
            *apply,
            *dry_run,
            *execute,
            *yes,
            target_db.clone(),
            &home,
            &app_home,
            roots,
            git_root.as_ref(),
        )
        .await;
    }

    if let Commands::Clean { action } = &command {
        return super::clean_cli::run_clean_command(action.clone()).await;
    }

    if let Commands::Harness { action } = &command {
        return super::harness_cli::run_harness_command(action.clone()).await;
    }

    if let Commands::SkillSurface { action } = &command {
        return super::skill_surface_cli::run_skill_surface_command(action.clone()).await;
    }

    if let Commands::Doctor {
        json,
        fix,
        scan_only: _,
        roots,
        jobs,
        probe_keys,
    } = &command
    {
        return super::manifest_cli::run_doctor_command(
            *json,
            *fix,
            roots.clone(),
            *jobs,
            *probe_keys,
            &home,
            &app_home,
            git_root.as_ref(),
        )
        .await;
    }

    if let Commands::Manifest { action } = &command {
        return super::manifest_cli::run_manifest_command(
            action.clone(),
            &home,
            &app_home,
            git_root.as_ref(),
        )
        .await;
    }

    if let Commands::Rescue { action } = &command {
        return super::rescue_cli::run_rescue_command(action.clone(), &home).await;
    }

    if let Commands::Status {
        watch,
        json,
        hide_orphans,
        probe_keys,
    } = &command
    {
        return crate::status_ops::status_cli::run_status(
            *watch,
            *json,
            *hide_orphans,
            *probe_keys,
            &app_home,
            &global_db_path,
            project_db_path.as_deref(),
        )
        .await;
    }

    if let Commands::Daemon { action } = &command {
        return crate::status_ops::status_cli::run_daemon(
            action.clone(),
            &app_home,
            &global_db_path,
        )
        .await;
    }

    if let Commands::Watcher { action } = &command {
        return crate::status_ops::status_cli::run_watcher(
            action.clone(),
            &global_db_path,
            project_db_path.clone(),
        )
        .await;
    }

    if let Commands::Foundry { action } = &command {
        return crate::status_ops::status_cli::run_foundry(
            action.clone(),
            &app_home,
            &global_db_path,
        )
        .await;
    }

    if let Commands::Repair {
        action,
        db,
        rule,
        apply,
        no_backup,
        json,
        purge_failed,
    } = &command
    {
        return crate::repair::run_repair(
            action.clone(),
            db.clone(),
            rule.clone(),
            *apply,
            *no_backup,
            *json,
            *purge_failed,
            &app_home,
        )
        .await;
    }

    if let Commands::Vault { action } = &command {
        return super::vault_cli::run_vault_command(&global_db_path, &app_home, action.clone())
            .await;
    }

    if let Commands::Env {
        action,
        filter,
        env_only,
        stdin_password,
        keychain,
        password_file,
        insecure_password_file,
    } = &command
    {
        return super::env_cmd::run_env_command(
            &global_db_path,
            action.clone(),
            filter.as_deref(),
            *env_only,
            *stdin_password,
            *keychain,
            password_file.as_deref(),
            *insecure_password_file,
        )
        .await;
    }

    if let Commands::Poke { action } = &command {
        return super::poke_cli::run_poke_command(&app_home, action.clone()).await;
    }

    if !matches!(command, Commands::Serve) {
        return super::cli_tool::run_cli_command(
            command,
            &global_db_path,
            project_db_path.as_ref(),
            &app_home,
        )
        .await;
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
        match crate::profiles::parse_tool_profile(raw_profile) {
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

    // Spawn idle connection cleanup task
    {
        let pool = server.pool.clone();
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(Duration::from_secs(60));
            loop {
                interval.tick().await;
                for key in pool.remove_idle_connections(Instant::now()) {
                    eprintln!("Idle cleanup: disconnecting '{}'", key);
                }
            }
        });
    }

    // Spawn periodic WAL TRUNCATE checkpoint task. SQLite's default PASSIVE
    // auto-checkpoint merges WAL frames into the DB but never shrinks the `-wal`
    // file; a write burst — or long-lived readers blocking truncation — lets it
    // balloon (observed: a 25 MB orphaned WAL on a busy agent DB, causing slow
    // reads and lock contention). A periodic TRUNCATE reclaims it when readers
    // are quiet. Cadence via TACHI_WAL_CHECKPOINT_SECS (default 300s, min 30s;
    // set 0 to disable).
    {
        let ckpt_secs = parse_env_u64("TACHI_WAL_CHECKPOINT_SECS").unwrap_or(300);
        if ckpt_secs > 0 {
            let ckpt_secs = ckpt_secs.max(30);
            let ckpt_server = server.clone();
            tokio::spawn(async move {
                let mut interval = tokio::time::interval(Duration::from_secs(ckpt_secs));
                interval.tick().await; // consume the immediate first tick
                loop {
                    interval.tick().await;
                    if let Err(e) = ckpt_server.with_global_store(|store| {
                        store.checkpoint_wal_truncate().map_err(|e| e.to_string())
                    }) {
                        eprintln!("[wal] global checkpoint skipped: {e}");
                    }
                    if ckpt_server.has_project_db() {
                        if let Err(e) = ckpt_server.with_project_store(|store| {
                            store.checkpoint_wal_truncate().map_err(|e| e.to_string())
                        }) {
                            eprintln!("[wal] project checkpoint skipped: {e}");
                        }
                    }
                    // Named-project DBs under this home (e.g. hyperion, wiki)
                    // accumulate WAL too — checkpoint each that exists.
                    for name in crate::path_utils::list_named_projects() {
                        if let Err(e) = ckpt_server.with_named_project_store(&name, |store| {
                            store.checkpoint_wal_truncate().map_err(|e| e.to_string())
                        }) {
                            eprintln!("[wal] named-project '{name}' checkpoint skipped: {e}");
                        }
                    }
                }
            });
        }
    }

    if gc_enabled {
        eprintln!(
            "Background GC enabled (initial_delay={}s, interval={}s)",
            gc_initial_delay_secs, gc_interval_secs
        );
        let gc_server = server.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_secs(gc_initial_delay_secs)).await;
            let mut interval = tokio::time::interval(Duration::from_secs(gc_interval_secs));
            loop {
                interval.tick().await;
                eprintln!("[gc] Running scheduled garbage collection...");
                match gc_server.with_global_store(|store: &mut MemoryStore| {
                    let mut gc = store
                        .gc_tables(&memory_core::GcConfig::default())
                        .map_err(|e| format!("{e}"))?;
                    let kanban_deleted =
                        gc_expired_kanban_cards(store, DEFAULT_KANBAN_GC_MAX_AGE_DAYS)?;
                    let foundry_deleted =
                        memory_core::gc_foundry_jobs(store.connection(), 30).unwrap_or(0);
                    if let Some(object) = gc.as_object_mut() {
                        object.insert("kanban_cards_pruned".into(), json!(kanban_deleted));
                        object.insert("foundry_jobs_pruned".into(), json!(foundry_deleted));
                    }
                    // Auto-archive stale memories (configurable via MEMORY_GC_STALE_DAYS env var)
                    let stale_days: u32 = std::env::var("MEMORY_GC_STALE_DAYS")
                        .ok()
                        .and_then(|v| v.parse().ok())
                        .unwrap_or(90);
                    match store.archive_stale_memories(stale_days) {
                        Ok(archived) => {
                            if archived > 0 {
                                eprintln!("[gc] Archived {} stale memories", archived);
                            }
                            if let Some(object) = gc.as_object_mut() {
                                object.insert("memories_archived".into(), json!(archived));
                            }
                        }
                        Err(e) => eprintln!("[gc] archive_stale_memories error: {}", e),
                    }
                    Ok(gc)
                }) {
                    Ok(result) => eprintln!("[gc] Global DB: {}", result),
                    Err(e) => eprintln!("[gc] Global DB error: {}", e),
                }
                if gc_server.has_project_db() {
                    match gc_server.with_project_store(|store: &mut MemoryStore| {
                        let mut gc = store
                            .gc_tables(&memory_core::GcConfig::default())
                            .map_err(|e| format!("{e}"))?;
                        let kanban_deleted =
                            gc_expired_kanban_cards(store, DEFAULT_KANBAN_GC_MAX_AGE_DAYS)?;
                        let foundry_deleted =
                            memory_core::gc_foundry_jobs(store.connection(), 30).unwrap_or(0);
                        if let Some(object) = gc.as_object_mut() {
                            object.insert("kanban_cards_pruned".into(), json!(kanban_deleted));
                            object.insert("foundry_jobs_pruned".into(), json!(foundry_deleted));
                        }
                        // Auto-archive stale memories (configurable via MEMORY_GC_STALE_DAYS env var)
                        let stale_days: u32 = std::env::var("MEMORY_GC_STALE_DAYS")
                            .ok()
                            .and_then(|v| v.parse().ok())
                            .unwrap_or(90);
                        match store.archive_stale_memories(stale_days) {
                            Ok(archived) => {
                                if archived > 0 {
                                    eprintln!(
                                        "[gc] Archived {} stale memories (project)",
                                        archived
                                    );
                                }
                                if let Some(object) = gc.as_object_mut() {
                                    object.insert("memories_archived".into(), json!(archived));
                                }
                            }
                            Err(e) => eprintln!("[gc] archive_stale_memories error: {}", e),
                        }
                        Ok(gc)
                    }) {
                        Ok(result) => eprintln!("[gc] Project DB: {}", result),
                        Err(e) => eprintln!("[gc] Project DB error: {}", e),
                    }
                }
            }
        });
    } else {
        eprintln!("Background GC disabled");
    }

    // Integrity check on global DB
    server
        .with_global_store(|store| {
            match store.quick_check() {
                Ok(true) => eprintln!("Global database integrity: OK"),
                Ok(false) => eprintln!("WARNING: Global database may be corrupted!"),
                Err(e) => eprintln!("WARNING: Could not check global database integrity: {e}"),
            }
            Ok(())
        })
        .map_err(|e| format!("startup check: {e}"))?;

    // Integrity check on project DB
    if project_db_path.is_some() {
        server
            .with_project_store(|store| {
                match store.quick_check() {
                    Ok(true) => eprintln!("Project database integrity: OK"),
                    Ok(false) => eprintln!("WARNING: Project database may be corrupted!"),
                    Err(e) => {
                        eprintln!("WARNING: Could not check project database integrity: {e}")
                    }
                }
                Ok(())
            })
            .map_err(|e| format!("startup check: {e}"))?;
    }

    // Load cached proxy tools from Hub
    {
        let load_proxy_tools = |store: &mut MemoryStore| -> Result<(), String> {
            let mcp_caps = store
                .hub_list(Some("mcp"), true)
                .map_err(|e| format!("hub list: {e}"))?;
            for cap in mcp_caps {
                let def: serde_json::Value = match serde_json::from_str(&cap.definition) {
                    Ok(def) => def,
                    Err(e) => {
                        eprintln!(
                            "[startup] skip MCP '{}' due to invalid definition JSON: {e}",
                            cap.id
                        );
                        continue;
                    }
                };
                if let Some(tools_json) = def.get("discovered_tools") {
                    match serde_json::from_value::<Vec<rmcp::model::Tool>>(tools_json.clone()) {
                        Ok(tools) => {
                            let server_name = cap.id.strip_prefix("mcp:").unwrap_or(&cap.id);
                            let filtered_tools = filter_mcp_tools_by_permissions(&def, tools);
                            lock_or_recover(&server.tool_discovery.proxy_tools, "proxy_tools")
                                .insert(server_name.to_string(), filtered_tools);
                        }
                        Err(e) => {
                            eprintln!(
                                "[startup] skip cached tools for '{}' due to invalid tool payload: {e}",
                                cap.id
                            );
                        }
                    }
                }
            }
            Ok(())
        };
        if let Err(e) = server.with_global_store(load_proxy_tools) {
            eprintln!("[startup] failed loading global MCP proxy cache: {e}");
        }
        if server.has_project_db() {
            if let Err(e) = server.with_project_store(load_proxy_tools) {
                eprintln!("[startup] failed loading project MCP proxy cache: {e}");
            }
        }
    }
    {
        let load_skill_tools = |store: &mut MemoryStore| -> Result<(), String> {
            let skill_caps = store
                .hub_list(Some("skill"), true)
                .map_err(|e| format!("hub list: {e}"))?;
            for cap in skill_caps {
                if should_expose_skill_tool(&cap) {
                    if let Err(e) = server.register_skill_tool(&cap) {
                        eprintln!(
                            "[startup] failed to register skill tool for '{}': {}",
                            cap.id, e
                        );
                    }
                }
            }
            Ok(())
        };
        if let Err(e) = server.with_global_store(load_skill_tools) {
            eprintln!("[startup] failed loading global skill tools: {e}");
        }
        if server.has_project_db() {
            if let Err(e) = server.with_project_store(load_skill_tools) {
                eprintln!("[startup] failed loading project skill tools: {e}");
            }
        }
    }

    if server.pipeline_enabled {
        eprintln!("Pipeline workers: ENABLED (external)");
    } else {
        eprintln!("Pipeline workers: DISABLED (set ENABLE_PIPELINE=true to enable)");
    }

    if daily_distill_scheduler_enabled(&server) {
        // Phase 1 daily batch distill. Default cadence is 24h; the legacy
        // per-capture `MemoryDistill` enqueue is gone, so this scheduler must
        // remain active even when external pipeline workers are disabled.
        let distill_interval_secs: u64 = std::env::var("DISTILL_INTERVAL_SECS")
            .ok()
            .and_then(|value| value.parse().ok())
            .unwrap_or(86_400);

        let distill_server = server.clone();
        let marker_path = daily_distill_marker_path(&app_home);
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_secs(60)).await;
            eprintln!(
                "Distill scheduler: ENABLED (daily batch, interval={}s)",
                distill_interval_secs
            );

            let run_once = |server: &crate::MemoryServer, marker: &std::path::Path| {
                let server = server.clone();
                let marker = marker.to_path_buf();
                async move {
                    match crate::foundry_runtime_ops::run_daily_batch_distill(&server).await {
                        Ok(report) => {
                            eprintln!(
                                "[distill] daily batch: dispatched={} distilled={} skipped={} fallback={} errors={}",
                                report.batches_dispatched,
                                report.groups_distilled,
                                report.groups_skipped,
                                report.fallback_used,
                                report.errors.len()
                            );
                            if let Some(parent) = marker.parent() {
                                if let Err(err) = tokio::fs::create_dir_all(parent).await {
                                    eprintln!(
                                        "[distill] failed to create marker directory {}: {err}",
                                        parent.display()
                                    );
                                    return;
                                }
                            }
                            // Marker carries the batch quality summary (not just a
                            // timestamp) so `tachi_status` can surface distill
                            // degradation and so hard errors reach the health score.
                            // read_distill_marker stays backward-compatible with the
                            // legacy bare-timestamp format.
                            let marker_body = serde_json::json!({
                                "ts": chrono::Utc::now().to_rfc3339(),
                                "groups_distilled": report.groups_distilled,
                                "groups_skipped": report.groups_skipped,
                                "fallback_used": report.fallback_used,
                                "errors": report.errors.len(),
                            })
                            .to_string();
                            if let Err(err) = tokio::fs::write(&marker, marker_body).await {
                                eprintln!(
                                    "[distill] failed to write marker {}: {err}",
                                    marker.display()
                                );
                            }
                        }
                        Err(err) => eprintln!("[distill] daily batch error: {err}"),
                    }
                }
            };

            // Catch-up: if the marker is missing or older than the cadence,
            // run immediately after the 60s warmup.
            let should_run_now = match tokio::fs::metadata(&marker_path)
                .await
                .and_then(|m| m.modified())
            {
                Ok(modified) => modified
                    .elapsed()
                    .map(|d| d.as_secs() >= distill_interval_secs)
                    .unwrap_or(true),
                Err(_) => true,
            };
            if should_run_now {
                run_once(&distill_server, &marker_path).await;
            }

            let mut interval = tokio::time::interval(Duration::from_secs(distill_interval_secs));
            // First tick fires immediately; consume it so we wait a full cadence.
            interval.tick().await;
            loop {
                interval.tick().await;
                run_once(&distill_server, &marker_path).await;
            }
        });
    } else {
        eprintln!("Distill scheduler: DISABLED (no project DB available)");
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
        server.global_vec_available, server.project_vec_available
    );
    eprintln!(
        "Tool surface: {}",
        server
            .active_tool_profile()
            .map(|profile| profile.as_str())
            .unwrap_or_else(|| crate::profiles::default_tool_profile().as_str())
    );

    if cli.daemon {
        // Mark this process so MCP write handlers execute locally instead of
        // re-forwarding to ourselves over HTTP.
        std::env::set_var("TACHI_DAEMON", "1");

        // HTTP daemon mode.
        // In daemon mode the cwd-derived repo-local project DB is RETAINED
        // (see the `cli.daemon && project_db_path.is_some()` block above): the
        // multi-DB FoundryScheduler gives every manifest DB equal coverage, so
        // we keep the auto-detected project and only emit a warning that the
        // daemon's bound project follows the launch cwd. Users can still pin a
        // single project explicitly via --project-db.

        // PR-4 singleton enforcement: acquire a daemon lock scoped to the
        // global DB before binding HTTP. Embedded runtimes can share the same
        // app_home without blocking each other when their global DBs differ.
        let lock_path = crate::daemon_lock::scoped_daemon_lock_path(&app_home, &global_db_path);
        let _daemon_lock = match crate::daemon_lock::DaemonLock::acquire(&lock_path) {
            Ok(g) => {
                eprintln!(
                    "[daemon] acquired singleton lock at {} (pid {})",
                    lock_path.display(),
                    std::process::id()
                );
                g
            }
            Err(e) => {
                eprintln!(
                    "[daemon] refusing to start: another tachi daemon already holds {} ({e})",
                    lock_path.display()
                );
                return Err(format!("tachi daemon singleton lock unavailable: {e}").into());
            }
        };

        // PR-4 multi-DB scheduler: periodically scan ~/.tachi/manifest.json
        // and run a per-DB safety-net poll against foundry_jobs. Re-injects
        // pending jobs into the existing foundry_tx mpsc channel for
        // routable scopes (own global/project + named projects), counts
        // orphans for unroutable manifest entries (dark DBs).
        let manifest_path = app_home.join("manifest.json");
        let scheduler = crate::foundry_scheduler::FoundryScheduler::start(
            manifest_path.clone(),
            server.foundry_tx_clone(),
            server.global_db_path_buf(),
            server.project_db_path_buf(),
        );
        eprintln!(
            "[daemon] foundry scheduler started (manifest={})",
            manifest_path.display()
        );
        // Hold scheduler for the whole daemon lifetime; Drop cancels
        // the manifest watcher + per-DB workers.
        let _scheduler = scheduler;

        let _vector_sweep = crate::vector_sweep::VectorSweepScheduler::start(
            manifest_path.clone(),
            global_db_path.clone(),
            project_db_path.clone(),
            server.llm.clone(),
        );
        eprintln!(
            "[daemon] vector sweep scheduled (manifest={})",
            manifest_path.display()
        );

        {
            let daily_server = server.clone();
            tokio::spawn(async move {
                loop {
                    let next_run = crate::daily_pipeline::next_daily_run_time();
                    tokio::time::sleep_until(next_run).await;
                    match crate::daily_pipeline::run_daily_pipeline(&daily_server).await {
                        Ok(report) => {
                            eprintln!("[daily-pipeline] completed: {}", report.summary())
                        }
                        Err(e) => eprintln!("[daily-pipeline] failed: {e}"),
                    }
                }
            });
            eprintln!("[daemon] daily pipeline scheduled for 04:00 Asia/Shanghai");
        }

        {
            let rem_server = server.clone();
            tokio::spawn(async move {
                loop {
                    let next_run = crate::daily_pipeline::next_weekly_rem_run_time();
                    tokio::time::sleep_until(next_run).await;
                    match crate::foundry_runtime_ops::wiki_evolver::run_weekly_wiki_evolution(&rem_server).await {
                        Ok(report) => eprintln!(
                            "[rem-wiki-evolver] completed: clusters={} drafts={} skipped={} errors={}",
                            report.clusters_found, report.drafts_written, report.skipped, report.errors
                        ),
                        Err(e) => eprintln!("[rem-wiki-evolver] failed: {e}"),
                    }
                }
            });
            eprintln!("[daemon] REM wiki evolver scheduled for Sunday 05:00 Asia/Shanghai");
        }

        use rmcp::transport::streamable_http_server::{
            session::local::LocalSessionManager, StreamableHttpServerConfig, StreamableHttpService,
        };
        use tokio_util::sync::CancellationToken;

        let ct = CancellationToken::new();
        let ct_shutdown = ct.clone();
        let requested_bind_addr = format!("127.0.0.1:{}", cli.port);
        let listener = tokio::net::TcpListener::bind(&requested_bind_addr).await?;
        let local_addr = listener.local_addr()?;
        let bind_addr = local_addr.to_string();
        let port = local_addr.port();

        // Idle reaper: a detached daemon has no parent whose death would signal
        // it to stop, so without this it lingers forever — one per global DB,
        // accumulating across every host restart. After
        // TACHI_DAEMON_IDLE_TIMEOUT_SECS with no MCP tool call (default 1800s;
        // 0 disables) it cancels its own serve token; the next stdio invocation
        // auto-respawns one on demand.
        if let Some(idle_timeout) = daemon_idle_timeout() {
            let clock = server.activity_clock();
            let ct_idle = ct.clone();
            let tick =
                std::time::Duration::from_secs(idle_timeout.as_secs().clamp(5, 60));
            tokio::spawn(async move {
                loop {
                    tokio::time::sleep(tick).await;
                    let last = clock.load(std::sync::atomic::Ordering::Relaxed);
                    let idle_ms = chrono::Utc::now().timestamp_millis() - last;
                    if idle_ms >= idle_timeout.as_millis() as i64 {
                        eprintln!(
                            "[idle-reaper] daemon idle {}s (limit {}s); shutting down — will auto-respawn on demand",
                            idle_ms / 1000,
                            idle_timeout.as_secs()
                        );
                        ct_idle.cancel();
                        break;
                    }
                }
            });
        }

        let health_payload = serde_json::json!({
            "status": "ok",
            "version": env!("CARGO_PKG_VERSION"),
            "transport": "http",
            "mcp": format!("http://{bind_addr}/mcp"),
        });

        let mut http_config = StreamableHttpServerConfig::default();
        http_config.stateful_mode = true;
        http_config.cancellation_token = ct.child_token();

        let service = StreamableHttpService::new(
            move || Ok(server.clone()),
            Arc::new(LocalSessionManager::default()),
            http_config,
        );

        let router = axum::Router::new()
            .route(
                "/health",
                axum::routing::get({
                    let health_payload = health_payload.clone();
                    move || {
                        let health_payload = health_payload.clone();
                        async move { axum::Json(health_payload) }
                    }
                }),
            )
            .nest_service("/mcp", service);
        eprintln!("Tachi daemon listening on http://{bind_addr}");

        // Write daemon discovery file so CLI invocations can forward writes
        // to the running daemon instead of contending for the DB write lock.
        let pid_path = crate::daemon_lock::scoped_daemon_pid_path(&app_home, &global_db_path);
        let pid_payload = serde_json::json!({
            "pid": std::process::id(),
            "port": port,
            "url": format!("http://{bind_addr}/mcp"),
            "global_db": global_db_path.display().to_string(),
            "project_db": project_db_path
                .as_ref()
                .map(|p| p.display().to_string()),
            "started_at": Utc::now().to_rfc3339(),
            "version": env!("CARGO_PKG_VERSION"),
        });
        if let Some(parent) = pid_path.parent() {
            let _ = tokio::fs::create_dir_all(parent).await;
        }
        let pid_body = serde_json::to_string_pretty(&pid_payload).unwrap_or_default();
        match tokio::task::spawn_blocking({
            let pid_path = pid_path.clone();
            move || crate::utils::write_owner_only_file(&pid_path, pid_body.as_bytes())
        })
        .await
        {
            Ok(Ok(())) => {}
            Ok(Err(e)) => {
                eprintln!(
                    "warning: failed to write daemon discovery file {}: {e}",
                    pid_path.display()
                );
            }
            Err(e) => {
                eprintln!(
                    "warning: failed to write daemon discovery file {}: {e}",
                    pid_path.display()
                );
            }
        }
        let pid_path_cleanup = pid_path.clone();

        tokio::select! {
            result = axum::serve(listener, router)
                .with_graceful_shutdown(async move { ct_shutdown.cancelled_owned().await }) => {
                if let Err(e) = result {
                    eprintln!("HTTP server error: {e}");
                }
            }
            _ = tokio::signal::ctrl_c() => {
                eprintln!("Received SIGINT, shutting down gracefully...");
                ct.cancel();
            }
            _ = sigterm() => {
                eprintln!("Received SIGTERM, shutting down gracefully...");
                ct.cancel();
            }
        }

        // Best-effort cleanup of daemon discovery file
        let _ = tokio::fs::remove_file(&pid_path_cleanup).await;
    } else {
        // stdio mode (default) — auto-spawn daemon if not running
        {
            let auto_daemon_disabled = std::env::var("TACHI_DISABLE_AUTO_DAEMON")
                .map(|value| {
                    let value = value.trim();
                    value == "1"
                        || value.eq_ignore_ascii_case("true")
                        || value.eq_ignore_ascii_case("yes")
                })
                .unwrap_or(false);

            // Version-skew replace: if a daemon is already running but STRICTLY
            // OLDER than this binary, a new-binary child cannot forward to it
            // (version mismatch ⇒ in-process fallback), so multiple OS processes
            // would write the same SQLite file directly and contend (5s
            // SQLITE_BUSY stalls). Replacing the stale daemon keeps a single
            // current-version writer. Safeguards: only when strictly older
            // (never a newer daemon mid-rollout); only the SAME global DB
            // (detect_* guarantees it); SIGTERM is graceful (cleans its pid
            // file); and flock arbitrates the respawn race if several children
            // detect the old daemon at once.
            if !auto_daemon_disabled {
                if let Some(info) =
                    crate::cli_client::detect_daemon_for_global_db(&app_home, &global_db_path).await
                {
                    if crate::cli_client::daemon_is_older_than_current(&info) {
                        if let Some(pid) = info.pid {
                            eprintln!(
                                "[auto-daemon] replacing stale daemon pid={pid} v{} (< v{})",
                                info.version.as_deref().unwrap_or("?"),
                                env!("CARGO_PKG_VERSION")
                            );
                            #[cfg(unix)]
                            {
                                unsafe { libc::kill(pid as libc::pid_t, libc::SIGTERM) };
                                // Wait (≤3s) for it to release its flock / exit
                                // so our respawn can bind cleanly.
                                for _ in 0..30 {
                                    if !crate::daemon_lock::process_alive(pid as i32) {
                                        break;
                                    }
                                    tokio::time::sleep(Duration::from_millis(100)).await;
                                }
                            }
                        }
                    }
                }
            }

            // Re-detect after the possible replacement (the dead daemon's port
            // is closed, so detect's TCP probe returns None even if a stale pid
            // file lingers).
            let daemon_running =
                crate::cli_client::detect_daemon_for_global_db(&app_home, &global_db_path)
                    .await
                    .is_some();
            if !daemon_running && !auto_daemon_disabled {
                match std::env::current_exe() {
                    Ok(exe) => {
                        let port_str = "0".to_string();
                        match std::process::Command::new(&exe)
                            .args(["--daemon", "--port", &port_str])
                            .stdin(std::process::Stdio::null())
                            .stdout(std::process::Stdio::null())
                            .stderr(std::process::Stdio::null())
                            .spawn()
                        {
                            Ok(mut child) => {
                                eprintln!(
                                    "[auto-daemon] spawned tachi daemon (pid={})",
                                    child.id()
                                );
                                // Reap child in background to avoid zombie processes on Unix
                                tokio::spawn(async move {
                                    let _ = child.wait();
                                });
                                // Wait for daemon to become ready by polling scoped discovery.
                                let ready_app_home = app_home.clone();
                                let ready_global_db = global_db_path.clone();
                                let ready = tokio::time::timeout(Duration::from_secs(5), async {
                                    for _ in 0..25 {
                                        tokio::time::sleep(Duration::from_millis(200)).await;
                                        if crate::cli_client::detect_daemon_for_global_db(
                                            &ready_app_home,
                                            &ready_global_db,
                                        )
                                        .await
                                        .is_some()
                                        {
                                            return true;
                                        }
                                    }
                                    false
                                })
                                .await
                                .unwrap_or(false);
                                if !ready {
                                    tracing::warn!(
                                        "[auto-daemon] daemon did not become ready within 5s"
                                    );
                                }
                            }
                            Err(e) => {
                                eprintln!("[auto-daemon] failed to spawn daemon: {e}");
                            }
                        }
                    }
                    Err(e) => {
                        eprintln!("[auto-daemon] cannot determine binary path: {e}");
                    }
                }
            } else if !daemon_running {
                eprintln!("[auto-daemon] disabled by TACHI_DISABLE_AUTO_DAEMON");
            }
        }

        let transport = (stdin(), stdout());
        let running = rmcp::service::serve_server(server, transport).await?;

        // Graceful shutdown: MCP quit (stdin EOF / client disconnect), SIGINT,
        // or parent-death. A well-behaved host closes stdin on disconnect so
        // `running.waiting()` resolves; the parent-death branch is the backstop
        // for hosts that leak the child (it gets reparented to init/launchd and
        // would otherwise linger forever).
        tokio::select! {
            quit_reason = running.waiting() => {
                eprintln!("Memory MCP Server stopped: {:?}", quit_reason);
            }
            _ = tokio::signal::ctrl_c() => {
                eprintln!("Received SIGINT, shutting down gracefully...");
            }
            _ = sigterm() => {
                eprintln!("Received SIGTERM, shutting down gracefully...");
            }
            _ = wait_for_parent_death() => {
                eprintln!("[parent-death] host process exited; shutting down orphaned stdio server");
            }
        }
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
