use memcore::HubCapability;
use rusqlite::OptionalExtension as _;
use serde_json::json;
use std::path::{Path, PathBuf};
use tachi_bootstrap::cli::{Cli, Commands, HubAction, McpAction};

use super::super::{open_cli_store, print_pretty_json};

#[path = "mcp_tasks.rs"]
mod mcp_tasks;
pub(crate) use mcp_tasks::run_mcp_task_command;

pub(crate) fn preflight_hub_action(action: &HubAction) -> Result<(), Box<dyn std::error::Error>> {
    match action {
        HubAction::Register { id, cap_type, .. } => {
            if crate::builtins::is_retired_builtin_capability_id(id) {
                return Err(std::io::Error::other(format!(
                    "Capability '{id}' is retired and cannot be registered."
                ))
                .into());
            }
            if !tachi_hub::is_supported_hub_capability_type(cap_type) {
                return Err(std::io::Error::other(
                    "Hub accepts only skill or plugin capabilities; MCP and virtual capabilities are retired. Configure MCP servers directly in the host.",
                )
                .into());
            }
            Ok(())
        }
        HubAction::Enable { id } => {
            if crate::builtins::is_retired_builtin_capability_id(id) {
                return Err(std::io::Error::other(format!(
                    "Capability '{id}' is retired and cannot be enabled."
                ))
                .into());
            }
            Ok(())
        }
        _ => Ok(()),
    }
}

fn resolve_hub_enable_preflight_db(cli: &Cli, home: &Path, app_home: &Path) -> Option<PathBuf> {
    let hub_db = memory_server_hub_cli::resolve_hub_db(None, app_home);
    if memcore::legacy_copy_marker_path(&hub_db).exists() {
        return None;
    }
    if let Ok(resolved) = std::fs::canonicalize(&hub_db) {
        if memcore::legacy_copy_marker_path(&resolved).exists() {
            return None;
        }
        return Some(resolved);
    }
    if std::fs::symlink_metadata(&hub_db).is_ok() {
        return None;
    }
    let default_global = app_home.join("global").join(memcore::MEMORY_DB_FILENAME);
    if cli.global_db.is_some()
        || std::env::var_os("MEMORY_DB_PATH").is_some()
        || hub_db != default_global
    {
        return None;
    }
    let defer_manifest = cli.daemon
        && cli.no_project_db
        && matches!(cli.command.as_ref(), None | Some(Commands::Serve));
    let manifest_path = app_home.join("manifest.json");
    if !defer_manifest && manifest_path.exists() {
        if let Some(entry) = crate::manifest::Manifest::load(&manifest_path)
            .ok()
            .and_then(|manifest| manifest.global().cloned())
        {
            if PathBuf::from(entry.path) != default_global {
                return None;
            }
        }
    }
    [
        app_home.join(memcore::LEGACY_MEMORY_DB_FILENAME),
        home.join(".sigil/global")
            .join(memcore::LEGACY_MEMORY_DB_FILENAME),
        home.join(".sigil").join(memcore::LEGACY_MEMORY_DB_FILENAME),
    ]
    .into_iter()
    .find(|path| path.exists())
    .and_then(|path| std::fs::canonicalize(path).ok())
}

fn make_readonly_shm_uri(path: &Path) -> String {
    let encoded = path
        .to_string_lossy()
        .replace('%', "%25")
        .replace('?', "%3f")
        .replace('#', "%23")
        .replace(' ', "%20");
    format!("file:{encoded}?mode=ro&readonly_shm=1")
}

fn query_hub_capability_type(
    conn: &rusqlite::Connection,
    id: &str,
) -> rusqlite::Result<Option<String>> {
    conn.query_row(
        "SELECT type FROM hub_capabilities WHERE id = ?1",
        [id],
        |row| row.get::<_, String>(0),
    )
    .optional()
}

fn probe_hub_capability_type_readonly(path: &Path, id: &str) -> Option<String> {
    // Resolve symlinks first so `-wal`/`-shm` sidecar discovery matches SQLite's
    // target-relative naming (`MEMORY_DB_PATH=/alias.db -> /real/target.db-wal`).
    let resolved = std::fs::canonicalize(path).ok()?;
    let path = resolved.as_path();
    let sidecar = |suffix: &str| -> PathBuf {
        let mut os = path.as_os_str().to_owned();
        os.push(suffix);
        PathBuf::from(os)
    };
    let wal_path = sidecar("-wal");
    let has_nonempty_wal = std::fs::metadata(&wal_path)
        .map(|metadata| metadata.len() > 0)
        .unwrap_or(false);
    if !has_nonempty_wal {
        let conn =
            memcore::db::open_immutable_readonly(&crate::path_utils::make_immutable_uri(path))
                .ok()?;
        return query_hub_capability_type(&conn, id).ok().flatten();
    }

    let shm_path = sidecar("-shm");
    let has_shm = std::fs::symlink_metadata(&shm_path)
        .map(|metadata| metadata.is_file() && metadata.len() > 0)
        .unwrap_or(false);
    if has_shm {
        memcore::db::register_sqlite_vec();
        if let Ok(conn) = rusqlite::Connection::open_with_flags(
            make_readonly_shm_uri(path),
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_URI,
        ) {
            let _ = conn.busy_timeout(std::time::Duration::from_millis(5_000));
            if let Ok(found) = query_hub_capability_type(&conn, id) {
                return found;
            }
        }
    }

    // Cold WAL without a usable -shm: stage main + WAL into a disposable temp
    // directory so SQLite never creates `<path>-shm` inside the user's directory.
    struct StagingDir(PathBuf);
    impl StagingDir {
        fn create() -> std::io::Result<Self> {
            let dir = std::env::temp_dir().join(format!(
                "tachi-hub-preflight-{}-{}",
                std::process::id(),
                uuid::Uuid::new_v4()
            ));
            let mut builder = std::fs::DirBuilder::new();
            #[cfg(unix)]
            {
                use std::os::unix::fs::DirBuilderExt as _;
                builder.mode(0o700);
            }
            builder.create(&dir)?;
            Ok(Self(dir))
        }
    }
    impl Drop for StagingDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    let staging = StagingDir::create().ok()?;
    let staged_db = staging.0.join("probe.db");
    let staged_wal = staging.0.join("probe.db-wal");
    std::fs::copy(path, &staged_db).ok()?;
    std::fs::copy(&wal_path, &staged_wal).ok()?;
    let result = {
        let conn = crate::physical_db_identity::open_read_only_connection(&staged_db).ok()?;
        query_hub_capability_type(&conn, id).ok().flatten()
    };
    drop(staging);
    result
}

pub(crate) fn preflight_hub_action_with_home(
    cli: &Cli,
    action: &HubAction,
    home: &Path,
    app_home: &Path,
) -> Result<(), Box<dyn std::error::Error>> {
    preflight_hub_action(action)?;
    if let HubAction::Enable { id } = action {
        let Some(candidate_db) = resolve_hub_enable_preflight_db(cli, home, app_home) else {
            return Ok(());
        };
        if let Some(cap_type) = probe_hub_capability_type_readonly(&candidate_db, id) {
            if !tachi_hub::is_supported_hub_capability_type(&cap_type) {
                return Err(std::io::Error::other(format!(
                    "Capability '{id}' (type '{cap_type}') is retired and cannot be enabled."
                ))
                .into());
            }
        }
    }
    Ok(())
}

fn collect_visible_hub_stats(
    hub_db: &Path,
) -> Result<memory_server_hub_cli::HubStatsSnapshot, Box<dyn std::error::Error>> {
    memory_server_hub_cli::collect_stats_filtered(
        hub_db,
        crate::builtins::is_visible_hub_capability,
    )
}

fn collect_visible_hub_list(
    hub_db: &Path,
    cap_type: Option<&str>,
    all: bool,
) -> Result<Vec<HubCapability>, Box<dyn std::error::Error>> {
    memory_server_hub_cli::collect_list_filtered_with(
        hub_db,
        cap_type,
        all,
        crate::builtins::is_visible_hub_capability,
    )
}

pub(super) async fn run_hub_command(
    action: HubAction,
    app_home: &PathBuf,
) -> Result<(), Box<dyn std::error::Error>> {
    preflight_hub_action(&action)?;
    let hub_db = memory_server_hub_cli::resolve_hub_db(None, app_home);
    match action {
        HubAction::List {
            cap_type,
            all,
            json: true,
        } => {
            let caps = collect_visible_hub_list(&hub_db, cap_type.as_deref(), all)?;
            print_pretty_json(&serde_json::to_value(caps)?)
        }
        HubAction::List {
            cap_type,
            all,
            json: false,
        } => {
            memory_server_hub_cli::cmd_list_filtered(
                &hub_db,
                cap_type.as_deref(),
                all,
                crate::builtins::is_visible_hub_capability,
            )?;
            Ok(())
        }
        HubAction::Show { id } => {
            memory_server_hub_cli::run(&HubAction::Show { id }, &hub_db, app_home)
                .map_err(std::io::Error::other)?;
            Ok(())
        }
        HubAction::Bindings => {
            memory_server_hub_cli::run(&HubAction::Bindings, &hub_db, app_home)
                .map_err(std::io::Error::other)?;
            Ok(())
        }
        HubAction::Doctor { fix } => {
            memory_server_hub_cli::run(&HubAction::Doctor { fix }, &hub_db, app_home)
                .map_err(std::io::Error::other)?;
            Ok(())
        }
        HubAction::Register {
            id,
            cap_type,
            name,
            definition,
            description,
        } => {
            let store = open_cli_store(&hub_db)?;
            let cap = HubCapability {
                id: id.clone(),
                cap_type,
                name,
                version: 1,
                description: description.unwrap_or_default(),
                definition,
                enabled: true,
                review_status: "approved".to_string(),
                health_status: "healthy".to_string(),
                last_error: None,
                last_success_at: None,
                last_failure_at: None,
                fail_streak: 0,
                active_version: None,
                exposure_mode: "direct".to_string(),
                uses: 0,
                successes: 0,
                failures: 0,
                avg_rating: 0.0,
                last_used: None,
                created_at: String::new(),
                updated_at: String::new(),
            };
            store.hub_register(&cap)?;
            let saved = store.hub_get(&id)?.ok_or_else(|| {
                std::io::Error::other(format!(
                    "capability '{}' registered but failed to reload from DB",
                    id
                ))
            })?;
            print_pretty_json(&serde_json::to_value(saved)?)
        }
        HubAction::Enable { id } => {
            let store = open_cli_store(&hub_db)?;
            if let Some(existing) = store.hub_get(&id)? {
                if crate::builtins::is_retired_hub_capability(&existing) {
                    return Err(std::io::Error::other(format!(
                        "Capability '{id}' (type '{}') is retired and cannot be enabled.",
                        existing.cap_type
                    ))
                    .into());
                }
            }
            let updated = store.hub_set_enabled(&id, true)?;
            print_pretty_json(&json!({
                "updated": updated,
                "id": id,
                "enabled": true,
            }))
        }
        HubAction::Disable { id } => {
            let store = open_cli_store(&hub_db)?;
            let updated = store.hub_set_enabled(&id, false)?;
            print_pretty_json(&json!({
                "updated": updated,
                "id": id,
                "enabled": false,
            }))
        }
        HubAction::Stats { json: true } => {
            let stats = collect_visible_hub_stats(&hub_db)?;
            print_pretty_json(&json!({
                "total_capabilities": stats.capabilities,
                "by_type": stats.by_type,
                "total_uses": stats.total_uses,
                "total_successes": stats.total_successes,
                "success_rate": if stats.total_uses > 0 { stats.total_successes as f64 / stats.total_uses as f64 } else { 0.0 },
            }))
        }
        HubAction::Stats { json: false } => {
            memory_server_hub_cli::cmd_stats_filtered(
                &hub_db,
                crate::builtins::is_visible_hub_capability,
            )
            .map_err(|e| std::io::Error::other(e.to_string()))?;
            Ok(())
        }
    }
}

pub(super) async fn run_mcp_command(
    action: McpAction,
    _app_home: &PathBuf,
) -> Result<(), Box<dyn std::error::Error>> {
    match action {
        McpAction::Task(args) => run_mcp_task_command(&args).await,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `resolve_hub_db` honors `MEMORY_DB_PATH`, which sibling tests point at
    /// their own DB under the global test lock.
    /// Tuple fields drop in order: the variable is restored before the lock is released.
    fn hub_db_env_isolation() -> (
        crate::test_support::EnvRestore,
        std::sync::MutexGuard<'static, ()>,
    ) {
        let guard = crate::utils::global_test_lock()
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        (
            crate::test_support::EnvRestore::remove("MEMORY_DB_PATH"),
            guard,
        )
    }

    #[tokio::test]
    #[allow(clippy::await_holding_lock)]
    async fn cli_hub_register_rejects_retired_id_before_creating_store() {
        let _env = hub_db_env_isolation();
        let app_home = crate::utils::test_fixture_path(format!(
            "tachi-cli-retired-register-{}",
            uuid::Uuid::new_v4()
        ));
        let hub_db = memory_server_hub_cli::resolve_hub_db(None, &app_home);

        let err = run_hub_command(
            HubAction::Register {
                id: crate::builtins::RETIRED_TRAJECTORY_DISTILLER_ID.to_string(),
                cap_type: "skill".to_string(),
                name: "trajectory-distiller".to_string(),
                definition: json!({"content": "retired"}).to_string(),
                description: Some("retired trajectory writer".to_string()),
            },
            &app_home,
        )
        .await
        .expect_err("CLI must reject retired registration");

        assert!(err.to_string().contains("retired"));
        assert!(!hub_db.exists(), "rejection must not create the Hub store");
        let _ = std::fs::remove_dir_all(&app_home);
    }

    #[tokio::test]
    #[allow(clippy::await_holding_lock)]
    async fn cli_hub_register_rejects_retired_types_before_creating_store() {
        let _env = hub_db_env_isolation();
        let root = tempfile::tempdir().expect("fixture root");
        let app_home = root.path().join("home");
        let hub_db = memory_server_hub_cli::resolve_hub_db(None, &app_home);
        for cap_type in ["mcp", "virtual"] {
            let err = run_hub_command(
                HubAction::Register {
                    id: format!("{cap_type}:retired-proxy"),
                    cap_type: cap_type.to_string(),
                    name: "retired proxy".to_string(),
                    definition: "{}".to_string(),
                    description: None,
                },
                &app_home,
            )
            .await
            .expect_err("CLI must reject retired capability types");
            assert!(err.to_string().contains("retired"));
            assert!(!hub_db.exists(), "rejection must not create the Hub store");
        }
    }

    #[tokio::test]
    #[allow(clippy::await_holding_lock)]
    async fn cli_hub_enable_rejects_retired_id_without_mutating_row() {
        let _env = hub_db_env_isolation();
        let app_home = crate::utils::test_fixture_path(format!(
            "tachi-cli-retired-enable-{}",
            uuid::Uuid::new_v4()
        ));
        let hub_db = memory_server_hub_cli::resolve_hub_db(None, &app_home);
        std::fs::create_dir_all(hub_db.parent().expect("Hub DB parent")).unwrap();
        let store = memcore::MemoryStore::open(hub_db.to_string_lossy().as_ref()).unwrap();
        let legacy = HubCapability {
            id: crate::builtins::RETIRED_TRAJECTORY_DISTILLER_ID.to_string(),
            cap_type: "skill".to_string(),
            name: "trajectory-distiller".to_string(),
            version: 1,
            description: "retired trajectory writer".to_string(),
            definition: json!({"content": "retired"}).to_string(),
            enabled: false,
            review_status: "rejected".to_string(),
            health_status: "healthy".to_string(),
            last_error: None,
            last_success_at: None,
            last_failure_at: None,
            fail_streak: 0,
            active_version: None,
            exposure_mode: "direct".to_string(),
            uses: 0,
            successes: 0,
            failures: 0,
            avg_rating: 0.0,
            last_used: None,
            created_at: String::new(),
            updated_at: String::new(),
        };
        store.hub_register(&legacy).unwrap();
        let before = serde_json::to_string(
            &store
                .hub_get(crate::builtins::RETIRED_TRAJECTORY_DISTILLER_ID)
                .unwrap(),
        )
        .unwrap();

        let err = run_hub_command(
            HubAction::Enable {
                id: crate::builtins::RETIRED_TRAJECTORY_DISTILLER_ID.to_string(),
            },
            &app_home,
        )
        .await
        .expect_err("CLI must reject retired enablement");
        assert!(err.to_string().contains("retired"));
        let after = serde_json::to_string(
            &store
                .hub_get(crate::builtins::RETIRED_TRAJECTORY_DISTILLER_ID)
                .unwrap(),
        )
        .unwrap();
        assert_eq!(after, before, "rejection must not mutate the Hub row");

        drop(store);
        let _ = std::fs::remove_dir_all(&app_home);
    }

    #[test]
    fn cli_hub_stats_shared_json_and_text_source_excludes_retired_tombstones() {
        for scope in ["global", "project"] {
            let root = crate::utils::test_fixture_path(format!(
                "tachi-cli-retired-stats-{scope}-{}",
                uuid::Uuid::new_v4()
            ));
            let db_path = root.join(scope).join("memory.db");
            std::fs::create_dir_all(db_path.parent().expect("stats DB parent")).unwrap();
            let store = memcore::MemoryStore::open(db_path.to_string_lossy().as_ref()).unwrap();
            let visible = HubCapability {
                id: format!("skill:visible-{scope}"),
                cap_type: "skill".to_string(),
                name: format!("visible-{scope}"),
                version: 1,
                description: "visible stats fixture".to_string(),
                definition: json!({"content": "visible"}).to_string(),
                enabled: true,
                review_status: "approved".to_string(),
                health_status: "healthy".to_string(),
                last_error: None,
                last_success_at: None,
                last_failure_at: None,
                fail_streak: 0,
                active_version: None,
                exposure_mode: "direct".to_string(),
                uses: 7,
                successes: 5,
                failures: 2,
                avg_rating: 4.0,
                last_used: None,
                created_at: String::new(),
                updated_at: String::new(),
            };
            let mut retired = visible.clone();
            retired.id = crate::builtins::RETIRED_TRAJECTORY_DISTILLER_ID.to_string();
            retired.name = "trajectory-distiller".to_string();
            retired.uses = 1000;
            retired.successes = 999;
            store.hub_register(&visible).unwrap();
            store.hub_register(&retired).unwrap();
            drop(store);

            let stats = collect_visible_hub_stats(&db_path)
                .expect("shared JSON/text stats collector should succeed");
            assert_eq!(stats.capabilities, 1, "scope={scope}");
            assert_eq!(stats.enabled_capabilities, 1, "scope={scope}");
            assert_eq!(stats.by_type.get("skill"), Some(&1), "scope={scope}");
            assert_eq!(stats.total_uses, 7, "scope={scope}");
            assert_eq!(stats.total_successes, 5, "scope={scope}");

            let _ = std::fs::remove_dir_all(&root);
        }
    }

    #[test]
    fn cli_hub_list_all_shared_json_and_text_source_excludes_retired_tombstones() {
        for scope in ["global", "project"] {
            let root = crate::utils::test_fixture_path(format!(
                "tachi-cli-retired-list-{scope}-{}",
                uuid::Uuid::new_v4()
            ));
            let db_path = root.join(scope).join("memory.db");
            std::fs::create_dir_all(db_path.parent().expect("list DB parent")).unwrap();
            let store = memcore::MemoryStore::open(db_path.to_string_lossy().as_ref()).unwrap();
            let visible = HubCapability {
                id: format!("skill:visible-list-{scope}"),
                cap_type: "skill".to_string(),
                name: format!("visible-list-{scope}"),
                version: 1,
                description: "visible list fixture".to_string(),
                definition: json!({"content": "visible"}).to_string(),
                enabled: false,
                review_status: "pending".to_string(),
                health_status: "healthy".to_string(),
                last_error: None,
                last_success_at: None,
                last_failure_at: None,
                fail_streak: 0,
                active_version: None,
                exposure_mode: "direct".to_string(),
                uses: 0,
                successes: 0,
                failures: 0,
                avg_rating: 0.0,
                last_used: None,
                created_at: String::new(),
                updated_at: String::new(),
            };
            let mut retired = visible.clone();
            retired.id = crate::builtins::RETIRED_TRAJECTORY_DISTILLER_ID.to_string();
            retired.name = "trajectory-distiller".to_string();
            store.hub_register(&visible).unwrap();
            store.hub_register(&retired).unwrap();
            drop(store);

            let listed = collect_visible_hub_list(&db_path, Some("skill"), true)
                .expect("shared JSON/text list collector should succeed");
            assert_eq!(
                listed.len(),
                1,
                "--all must still filter tombstones; scope={scope}"
            );
            assert_eq!(listed[0].id, visible.id, "scope={scope}");

            let _ = std::fs::remove_dir_all(&root);
        }
    }

    #[test]
    fn preflight_hub_enable_rejects_legacy_db_historical_types_before_migration() {
        use clap::Parser as _;

        let _env = hub_db_env_isolation();
        let root = tempfile::tempdir().expect("fixture root");
        let home = root.path().join("user-home");
        let app_home = root.path().join("tachi-home");
        std::fs::create_dir_all(&home).unwrap();
        std::fs::create_dir_all(&app_home).unwrap();
        let legacy_db = app_home.join(memcore::LEGACY_MEMORY_DB_FILENAME);
        let store = memcore::MemoryStore::open(legacy_db.to_string_lossy().as_ref()).unwrap();
        for (id, cap_type) in [
            ("mcp:legacy-ready", "mcp"),
            ("virtual:legacy-vc", "virtual"),
        ] {
            store
                .hub_register(&HubCapability {
                    id: id.to_string(),
                    cap_type: cap_type.to_string(),
                    name: id.to_string(),
                    version: 1,
                    description: "legacy capability".to_string(),
                    definition: "{}".to_string(),
                    enabled: false,
                    review_status: "approved".to_string(),
                    health_status: "healthy".to_string(),
                    last_error: None,
                    last_success_at: None,
                    last_failure_at: None,
                    fail_streak: 0,
                    active_version: None,
                    exposure_mode: "direct".to_string(),
                    uses: 0,
                    successes: 0,
                    failures: 0,
                    avg_rating: 0.0,
                    last_used: None,
                    created_at: String::new(),
                    updated_at: String::new(),
                })
                .unwrap();
        }
        drop(store);

        let migrated_global = app_home.join("global").join(memcore::MEMORY_DB_FILENAME);
        for id in ["mcp:legacy-ready", "virtual:legacy-vc"] {
            let cli = Cli::parse_from(["tachi", "--no-project-db", "hub", "enable", id]);
            let Some(Commands::Hub { action }) = cli.command.as_ref() else {
                panic!("expected Hub command");
            };
            let err = preflight_hub_action_with_home(&cli, action, &home, &app_home)
                .expect_err("preflight must reject historical retired capability enablement");
            assert!(err.to_string().contains("retired"), "got: {err}");
            assert!(
                !migrated_global.exists(),
                "preflight rejection must not migrate or create global store"
            );
            assert!(
                !app_home.join("global").exists(),
                "preflight rejection must not create global directory"
            );
        }
    }

    #[test]
    fn preflight_hub_enable_reads_cold_wal_without_creating_shm_sidecar() {
        use clap::Parser as _;

        let _env = hub_db_env_isolation();
        let root = tempfile::tempdir().expect("fixture root");
        let staging_src = root.path().join("wal-source");
        std::fs::create_dir_all(&staging_src).unwrap();
        let src_db = staging_src.join("memory.db");
        let conn = rusqlite::Connection::open(&src_db).unwrap();
        conn.execute_batch(
            "PRAGMA journal_mode = WAL;
             PRAGMA wal_autocheckpoint = 0;
             CREATE TABLE hub_capabilities (id TEXT PRIMARY KEY, type TEXT NOT NULL);
             INSERT INTO hub_capabilities (id, type) VALUES ('mcp:wal-only', 'mcp');",
        )
        .unwrap();

        let home = root.path().join("user-home");
        let app_home = root.path().join("tachi-home");
        std::fs::create_dir_all(&home).unwrap();
        std::fs::create_dir_all(&app_home).unwrap();
        std::fs::copy(&src_db, app_home.join("memory.db")).unwrap();
        std::fs::copy(
            staging_src.join("memory.db-wal"),
            app_home.join("memory.db-wal"),
        )
        .unwrap();
        drop(conn);
        assert!(!app_home.join("memory.db-shm").exists());

        let cli = Cli::parse_from(["tachi", "--no-project-db", "hub", "enable", "mcp:wal-only"]);
        let Some(Commands::Hub { action }) = cli.command.as_ref() else {
            panic!("expected Hub command");
        };
        let err = preflight_hub_action_with_home(&cli, action, &home, &app_home)
            .expect_err("cold WAL retired row must be rejected");
        assert!(err.to_string().contains("retired"), "got: {err}");
        assert!(
            !app_home.join("memory.db-shm").exists(),
            "preflight must not create -shm sidecar in app_home"
        );
        assert!(!app_home.join("global").exists());
    }
}
