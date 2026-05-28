use super::*;

fn init_tracing(home: &std::path::Path) {
    use std::io::Write as _;
    use tracing_subscriber::{fmt, layer::SubscriberExt, util::SubscriberInitExt, EnvFilter};

    let env_filter = EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| EnvFilter::new("info,memory_server=info,rmcp=warn"));

    // Try the canonical location first, then /tmp, then bare stderr.
    let log_dir = home.join(".tachi").join("logs");
    let primary = log_dir.join("tachi.log");
    let fallback = std::path::PathBuf::from("/tmp/tachi.log");

    let opener = |path: &std::path::Path| -> Option<std::fs::File> {
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .ok()
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

#[tokio::main]
pub(super) async fn tokio_main(cli: Cli) -> Result<(), Box<dyn std::error::Error>> {
    // PR7 — install the tracing sink before doing anything else so early errors
    // (config load, manifest resolution, daemon bind) are captured.
    let home_for_logs = dirs::home_dir().unwrap_or_else(|| PathBuf::from("."));
    init_tracing(&home_for_logs);
    tracing::info!(
        version = env!("CARGO_PKG_VERSION"),
        "tachi memory-server starting"
    );

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

    let app_home = std::env::var("TACHI_HOME")
        .or_else(|_| std::env::var("SIGIL_HOME"))
        .map(|v| expand_user_path(&v))
        .unwrap_or_else(|_| home.join(".tachi"));
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
    let manifest_path = crate::manifest::Manifest::default_path(&home);
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
                        let daemon_alive =
                            crate::cli_client::detect_daemon(&app_home).await.is_some();
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
        batch_size,
        dry_run,
    } = &command
    {
        let target_path = if let Some(p) = db {
            expand_user_path(p.to_string_lossy().as_ref())
        } else {
            global_db_path.clone()
        };
        return super::backfill::run_backfill_vectors(
            &target_path,
            &global_db_path,
            *batch_size,
            *dry_run,
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
            return Err(format!(
                "project DB not found: {}",
                target_project.display()
            )
            .into());
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

    if let Commands::Doctor {
        json,
        scan_only,
        roots,
        jobs,
        probe_keys,
    } = &command
    {
        return super::manifest_cli::run_doctor_command(
            *json,
            *scan_only,
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
        return crate::status_ops::run_status(
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
        return crate::status_ops::run_daemon(action.clone(), &app_home).await;
    }

    if let Commands::Watcher { action } = &command {
        return crate::status_ops::run_watcher(action.clone(), &global_db_path, project_db_path.clone()).await;
    }

    if let Commands::Foundry { action } = &command {
        return crate::status_ops::run_foundry(action.clone(), &app_home, &global_db_path).await;
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
        filter,
        env_only,
        stdin_password,
        keychain,
        password_file,
    } = &command
    {
        return super::env_cmd::run_env_command(
            &global_db_path,
            filter.as_deref(),
            *env_only,
            *stdin_password,
            *keychain,
            password_file.as_deref(),
        )
        .await;
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
    {
        use base64::{engine::general_purpose::STANDARD as B64, Engine};
        let vault_unlocked = (|| -> Result<bool, Box<dyn std::error::Error>> {
            let config = server
                .with_global_store_read(|store| {
                    store.vault_get_config().map_err(|e| e.to_string())
                })?
                .ok_or("not initialized")?;

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
                return Err("keychain entry not found".into());
            }
            let password = String::from_utf8(output.stdout)?.trim().to_string();
            if password.is_empty() {
                return Err("empty keychain password".into());
            }

            let salt = B64.decode(&config.salt)?;
            let key = crate::vault_crypto::derive_key(&password, &salt)?;
            if !crate::vault_crypto::verify_password(&key, &config.verifier)? {
                return Err("keychain password doesn't match vault".into());
            }

            *crate::write_or_recover(&server.vault_key, "vault_key") = Some(key);
            *crate::write_or_recover(&server.vault_unlock_time, "vault_unlock_time") =
                Some(std::time::Instant::now());
            let loaded = server.refresh_llm_provider_secrets_from_vault()?;
            eprintln!("[vault] loaded {loaded} provider key(s) from unlocked vault");
            Ok(true)
        })();

        match vault_unlocked {
            Ok(true) => eprintln!("[vault] auto-unlocked from Keychain"),
            Err(e) => eprintln!("[vault] auto-unlock skipped: {e}"),
            _ => {}
        }
    }

    // Spawn idle connection cleanup task
    {
        let pool = server.pool.clone();
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(Duration::from_secs(60));
            loop {
                interval.tick().await;
                let mut conns = lock_or_recover(&pool.connections, "mcp_pool.connections");
                let now = Instant::now();
                let idle_ttl = pool.idle_ttl;
                let stale: Vec<String> = conns
                    .iter()
                    .filter(|(_, c)| now.duration_since(c.last_used) > idle_ttl)
                    .map(|(k, _)| k.clone())
                    .collect();
                for key in stale {
                    if let Some(conn) = conns.remove(&key) {
                        eprintln!("Idle cleanup: disconnecting '{}'", key);
                        drop(conn);
                    }
                }
            }
        });
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
                    if let Some(object) = gc.as_object_mut() {
                        object.insert("kanban_cards_pruned".into(), json!(kanban_deleted));
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
                        if let Some(object) = gc.as_object_mut() {
                            object.insert("kanban_cards_pruned".into(), json!(kanban_deleted));
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

    // ─── Clawdoctor: OpenClaw health monitor ─────────────────────────────────
    {
        let clawdoctor_enabled = cli
            .clawdoctor
            .or_else(|| parse_env_bool("CLAWDOCTOR_ENABLED"))
            .unwrap_or(false);

        if clawdoctor_enabled {
            let cd_url = cli
                .clawdoctor_url
                .clone()
                .or_else(|| std::env::var("CLAWDOCTOR_URL").ok())
                .unwrap_or_else(|| "http://127.0.0.1:18789".to_string());

            let cd_interval = cli
                .clawdoctor_interval_secs
                .or_else(|| parse_env_u64("CLAWDOCTOR_INTERVAL_SECS"))
                .unwrap_or(crate::clawdoctor::DEFAULT_CLAWDOCTOR_INTERVAL_SECS);

            let cd_threshold = cli
                .clawdoctor_fail_threshold
                .or_else(|| {
                    std::env::var("CLAWDOCTOR_FAIL_THRESHOLD")
                        .ok()
                        .and_then(|v| v.parse::<u32>().ok())
                })
                .unwrap_or(crate::clawdoctor::DEFAULT_CLAWDOCTOR_FAIL_THRESHOLD);

            eprintln!(
                "Clawdoctor enabled (url={}, interval={}s, threshold={})",
                cd_url, cd_interval, cd_threshold
            );

            let cd_server = server.clone();
            tokio::spawn(async move {
                crate::clawdoctor::run_clawdoctor(cd_server, cd_url, cd_interval, cd_threshold)
                    .await;
            });
        } else {
            eprintln!("Clawdoctor disabled (set CLAWDOCTOR_ENABLED=true to enable)");
        }
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
                            server.tool_discovery_lock()
                                .proxy_tools
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

        // Phase 1 daily batch distill. Default cadence is 24h; the legacy
        // 30-minute per-capture scheduler is retained as a fallback (see
        // `schedule_pending_distill_jobs`) but is no longer the primary path.
        let distill_interval_secs: u64 = std::env::var("DISTILL_INTERVAL_SECS")
            .ok()
            .and_then(|value| value.parse().ok())
            .unwrap_or(86_400);

        let distill_server = server.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_secs(60)).await;
            eprintln!(
                "Distill scheduler: ENABLED (daily batch, interval={}s)",
                distill_interval_secs
            );

            let marker_path = dirs::home_dir()
                .unwrap_or_else(|| std::path::PathBuf::from("."))
                .join(".tachi")
                .join("foundry-runs")
                .join(".last_distill_run");

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
                                let _ = std::fs::create_dir_all(parent);
                            }
                            let _ = std::fs::write(&marker, chrono::Utc::now().to_rfc3339());
                        }
                        Err(err) => eprintln!("[distill] daily batch error: {err}"),
                    }
                }
            };

            // Catch-up: if the marker is missing or older than the cadence,
            // run immediately after the 60s warmup.
            let should_run_now = match std::fs::metadata(&marker_path).and_then(|m| m.modified()) {
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
        eprintln!("Pipeline workers: DISABLED (set ENABLE_PIPELINE=true to enable)");
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

        // HTTP daemon mode
        // In daemon mode, project DB auto-detection is disabled above to avoid
        // mixed project context. Users can still opt into single-project mode
        // via explicit --project-db.

        // PR-4 singleton enforcement: acquire ~/.tachi/daemon.lock (flock +
        // PID file) before binding the HTTP port so a duplicate daemon
        // fails fast with a clear error instead of racing the first one
        // for DB writes. The guard is held for the whole daemon lifetime
        // and Drop releases the flock + unlinks the file.
        let lock_path = app_home.join("daemon.lock");
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

        use rmcp::transport::streamable_http_server::{
            session::local::LocalSessionManager, StreamableHttpServerConfig, StreamableHttpService,
        };
        use tokio_util::sync::CancellationToken;

        let ct = CancellationToken::new();
        let ct_shutdown = ct.clone();
        let port = cli.port;

        let service = StreamableHttpService::new(
            move || Ok(server.clone()),
            Arc::new(LocalSessionManager::default()),
            StreamableHttpServerConfig {
                stateful_mode: true,
                cancellation_token: ct.child_token(),
                ..Default::default()
            },
        );

        let router = axum::Router::new().nest_service("/mcp", service);
        let bind_addr = format!("127.0.0.1:{port}");
        let listener = tokio::net::TcpListener::bind(&bind_addr).await?;

        eprintln!("Tachi daemon listening on http://{bind_addr}");

        // Write daemon discovery file so CLI invocations can forward writes
        // to the running daemon instead of contending for the DB write lock.
        let pid_path = app_home.join("daemon.pid");
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
        if let Err(e) = tokio::fs::write(
            &pid_path,
            serde_json::to_string_pretty(&pid_payload).unwrap_or_default(),
        )
        .await
        {
            eprintln!(
                "warning: failed to write daemon discovery file {}: {e}",
                pid_path.display()
            );
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
        }

        // Best-effort cleanup of daemon discovery file
        let _ = tokio::fs::remove_file(&pid_path_cleanup).await;
    } else {
        // stdio mode (default) — auto-spawn daemon if not running
        {
            let daemon_running = crate::cli_client::detect_daemon(&app_home).await.is_some();
            if !daemon_running {
                match std::env::current_exe() {
                    Ok(exe) => {
                        let port_str = cli.port.to_string();
                        match std::process::Command::new(&exe)
                            .args(["--daemon", "--port", &port_str])
                            .stdin(std::process::Stdio::null())
                            .stdout(std::process::Stdio::null())
                            .stderr(std::process::Stdio::null())
                            .spawn()
                        {
                            Ok(child) => {
                                eprintln!(
                                    "[auto-daemon] spawned tachi daemon (pid={})",
                                    child.id()
                                );
                                // Give daemon time to acquire lock and bind port
                                tokio::time::sleep(Duration::from_millis(500)).await;
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
            }
        }

        let transport = (stdin(), stdout());
        let running = rmcp::service::serve_server(server, transport).await?;

        // Graceful shutdown: wait for either MCP quit or SIGINT/SIGTERM
        tokio::select! {
            quit_reason = running.waiting() => {
                eprintln!("Memory MCP Server stopped: {:?}", quit_reason);
            }
            _ = tokio::signal::ctrl_c() => {
                eprintln!("Received SIGINT, shutting down gracefully...");
            }
        }
    }

    Ok(())
}
