use crate::server_state::MemoryServer;
use memcore::MigrationAuthority;
use serde_json::json;
use std::path::PathBuf;

use super::super::print_pretty_json;

/// Pretty-print a tool's JSON-string output. Falls back to raw text if the
/// result is not valid JSON (defensive: handlers always return JSON today).
pub(super) fn print_cli_tool_result(body: &str) -> Result<(), Box<dyn std::error::Error>> {
    match serde_json::from_str::<serde_json::Value>(body) {
        Ok(v) => print_pretty_json(&v),
        Err(_) => {
            println!("{body}");
            Ok(())
        }
    }
}

fn cli_tool_allows_read_fallback(tool_name: &str) -> bool {
    matches!(
        tool_name,
        "search_memory"
            | "list_memories"
            | "get_memory"
            | "tachi_search"
            | "wiki_search"
            | "tachi_wiki_search"
            | "vault_status"
            | "vault_list"
    )
}

/// Dispatch a CLI tool invocation: try the running daemon first; on miss,
/// build a transient in-process MemoryServer and call the handler directly.
/// Either path returns the tool's JSON string body.
///
/// #1041 round-7: the in-process branch stamps `__tachi_project_explicit`
/// (via `session_identity::stamp_project_explicit_marker`) before invoking
/// `in_process`, since it never goes through `enforce_session_project` (no
/// daemon/session exists on this path at all). `project` present in the raw
/// CLI args means caller-explicit here, unconditionally — this dispatcher
/// never injects a default itself.
pub(super) async fn dispatch_cli_tool<F, Fut>(
    tool_name: &str,
    args: serde_json::Map<String, serde_json::Value>,
    global_db: &PathBuf,
    project_db: Option<&PathBuf>,
    app_home: &PathBuf,
    in_process: F,
) -> Result<String, Box<dyn std::error::Error>>
where
    F: FnOnce(MemoryServer, serde_json::Map<String, serde_json::Value>) -> Fut,
    Fut: std::future::Future<Output = Result<String, String>>,
{
    let schema_migration = MigrationAuthority::Deny;
    dispatch_cli_tool_with_migration_authority(
        tool_name,
        args,
        global_db,
        project_db,
        app_home,
        &schema_migration,
        in_process,
    )
    .await
}

/// Dispatch a CLI tool with an explicit authority for its in-process fallback.
/// A compatible daemon returns before this capability reaches any local DB open.
pub(super) async fn dispatch_cli_tool_with_migration_authority<F, Fut>(
    tool_name: &str,
    args: serde_json::Map<String, serde_json::Value>,
    global_db: &PathBuf,
    project_db: Option<&PathBuf>,
    app_home: &PathBuf,
    schema_migration: &MigrationAuthority,
    in_process: F,
) -> Result<String, Box<dyn std::error::Error>>
where
    F: FnOnce(MemoryServer, serde_json::Map<String, serde_json::Value>) -> Fut,
    Fut: std::future::Future<Output = Result<String, String>>,
{
    let read_fallback = cli_tool_allows_read_fallback(tool_name);
    // Compute the CLI's named project once. When a project DB is in play, the
    // daemon-forwarding branches must declare this binding via X-Tachi-Project
    // so the daemon-side C1 guard (`reject_unbound_cross_project_write`) sees a
    // bound session and accepts the explicit `project=` arg instead of rejecting
    // it as an unbound cross-tenant write. When `project_db` is None (global-only
    // CLI), this is None → no header → no explicit project= arg → C1 allows.
    let cli_named_project =
        project_db.and_then(|path| crate::path_utils::named_project_for_db_path(path));
    if let Some(info) = crate::cli_client::detect_daemon_for_global_db(app_home, global_db).await {
        if crate::cli_client::daemon_matches_requested_dbs(
            &info,
            global_db,
            project_db.map(|path| path.as_path()),
        ) {
            match crate::cli_client::call_daemon_tool(
                &info,
                tool_name,
                args.clone(),
                cli_named_project.as_deref(),
            )
            .await
            {
                Ok(body) => return Ok(body),
                Err(e) if read_fallback || e.allows_in_process_fallback() => {
                    eprintln!(
                        "[cli] daemon dispatch failed ({e}); falling back to in-process execution"
                    );
                }
                Err(e) => return Err(format!(
                    "daemon dispatch failed after dispatch; refusing in-process fallback to avoid duplicate writes: {e}"
                ).into()),
            }
        } else if crate::cli_client::daemon_global_db_matches(&info, global_db) {
            if let Some(named_project) = &cli_named_project {
                let mut daemon_args = args.clone();
                daemon_args
                    .entry("project".to_string())
                    .or_insert_with(|| json!(named_project.clone()));
                eprintln!(
                    "[cli] daemon project DB scope differs; forwarding via named project '{named_project}'"
                );
                match crate::cli_client::call_daemon_tool(
                    &info,
                    tool_name,
                    daemon_args,
                    cli_named_project.as_deref(),
                )
                .await
                {
                    Ok(body) => return Ok(body),
                    Err(e) if read_fallback || e.allows_in_process_fallback() => {
                        eprintln!(
                            "[cli] daemon named-project dispatch failed ({e}); falling back to in-process execution"
                        );
                    }
                    Err(e) => return Err(format!(
                        "daemon named-project dispatch failed after dispatch; refusing in-process fallback to avoid duplicate writes: {e}"
                    ).into()),
                }
            } else {
                eprintln!(
                    "[cli] daemon DB scope differs from requested CLI scope; executing in-process"
                );
            }
        } else {
            eprintln!(
                "[cli] foreign daemon global_db={:?}; executing in-process",
                info.global_db
            );
        }
    }
    // #1041 round-7 BUG (codex-17c966): this in-process path never goes
    // through a daemon, so `enforce_session_project` (the ONLY other place
    // that stamps `__tachi_project_explicit`) never runs here at all — there
    // is no session/transport to enforce against, just a transient
    // `MemoryServer` and the raw CLI args. Without this stamp, a caller who
    // typed an explicit `--project X` on the CLI arrives at
    // `write_affinity`'s S1 gate looking identical to a transport-injected
    // default (`project_explicit` defaults to `false` on deserialization),
    // so a domain-routed save can get silently rerouted away from the
    // project the caller pinned it to. `project` is present in `args` here
    // if and only if the CLI caller passed `--project`/`project=` explicitly
    // (this dispatcher never injects one itself) — so "present" IS
    // caller-explicit, unconditionally, in this in-process branch. Reuses
    // `session_identity`'s shared stamping helper rather than re-deriving
    // this signal a second way.
    let mut args = args;
    let project_explicit = args.contains_key("project");
    crate::session_identity::stamp_project_explicit_marker(&mut args, project_explicit);
    let server = crate::cli_client::build_in_process_server_with_migration_authority(
        global_db,
        project_db,
        schema_migration.clone(),
    )?;
    let body = in_process(server, args)
        .await
        .map_err(|e| -> Box<dyn std::error::Error> { e.into() })?;
    Ok(body)
}

#[cfg(test)]
mod tests {
    use super::cli_tool_allows_read_fallback;
    use crate::bootstrap::cli_tool::tool_dispatch::dispatch_cli_tool;
    use crate::test_support::EnvRestore;
    use std::path::PathBuf;
    use tokio_util::sync::CancellationToken;

    /// Spawn a global-only HTTP MCP daemon (no project DB) on a random local
    /// port, mirroring the stdio-proxy test fixture pattern. Returns the bound
    /// URL, a cancellation token, and the task handle.
    async fn spawn_global_only_http_daemon(
        server: crate::MemoryServer,
        global_db_path: &PathBuf,
    ) -> (
        crate::cli_client::DaemonInfo,
        CancellationToken,
        tokio::task::JoinHandle<()>,
    ) {
        use rmcp::transport::streamable_http_server::{
            session::local::LocalSessionManager, StreamableHttpServerConfig, StreamableHttpService,
        };

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind daemon listener");
        let local_addr = listener.local_addr().expect("local addr");
        let ct = CancellationToken::new();
        let ct_shutdown = ct.clone();

        let mut http_config = StreamableHttpServerConfig::default();
        http_config.stateful_mode = true;
        http_config.cancellation_token = ct.child_token();

        let service = StreamableHttpService::new(
            move || Ok(server.clone_for_mcp_session()),
            std::sync::Arc::new(LocalSessionManager::default()),
            http_config,
        );
        let router = axum::Router::new().nest_service("/mcp", service);
        let handle = tokio::spawn(async move {
            let _ = axum::serve(listener, router)
                .with_graceful_shutdown(async move { ct_shutdown.cancelled_owned().await })
                .await;
        });

        (
            crate::cli_client::DaemonInfo {
                url: format!("http://{local_addr}/mcp"),
                global_db: Some(global_db_path.display().to_string()),
                project_db: None,
                version: Some(env!("CARGO_PKG_VERSION").to_string()),
                pid: Some(std::process::id() as i64),
            },
            ct,
            handle,
        )
    }

    /// F7 regression: the CLI named-project forwarding path must declare its
    /// binding to the daemon (via X-Tachi-Project) so the daemon-side C1 guard
    /// (`reject_unbound_cross_project_write`) accepts the bound write instead of
    /// rejecting it as an unbound cross-tenant write.
    ///
    /// Scenario: a global-only daemon is running for global DB G. The CLI is
    /// invoked with the same global DB G but a DIFFERENT project DB P2. The CLI
    /// forwards the write via the named-project path (`dispatch_cli_tool` line
    /// 66-87), injecting `project=<name>` into args. On the v2 base, the wrapper
    /// `call_daemon_tool` passes `None` for the proxy project, so the daemon
    /// session is UNBOUND and C1 rejects the explicit `project=` arg → hard
    /// failure (no fallback). After F7, the header binds the session and the
    /// write lands in P2's DB.
    #[test]
    fn cli_named_project_forward_binds_daemon_session_for_write() {
        let _guard = crate::utils::global_test_lock()
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        // The in-process test daemon shares this process; mark it as the daemon
        // so its own tool handlers short-circuit `maybe_forward_*` (which would
        // otherwise detect the daemon's own PID file and loop). The CLI dispatch
        // path (`dispatch_cli_tool`) does not consult TACHI_DAEMON, so it still
        // detects and forwards to the daemon via the PID file.
        let _is_daemon = EnvRestore::set("TACHI_DAEMON", "1");

        let temp = tempfile::tempdir().expect("tempdir");
        let tachi_home = temp.path().join("home");
        let global = tachi_home.join("global/memory.db");
        let cli_project_name = "Sigil-cli-f7";
        let cli_project = tachi_home
            .join("projects")
            .join(cli_project_name)
            .join("memory.db");
        std::fs::create_dir_all(global.parent().expect("global parent")).expect("global parent");
        std::fs::create_dir_all(cli_project.parent().expect("project parent"))
            .expect("project parent");
        let _tachi_home = EnvRestore::set_path("TACHI_HOME", &tachi_home);
        let _sigil_home = EnvRestore::remove("SIGIL_HOME");
        let _app_home = EnvRestore::remove("TACHI_APP_HOME");
        let _disable_auto = EnvRestore::set("TACHI_DISABLE_AUTO_DAEMON", "1");

        // Seed the project DB schema so the daemon can open it for the named project.
        let _seed = crate::MemoryServer::new(
            tachi_home.join(format!("seed-{}.db", uuid::Uuid::new_v4())),
            Some(cli_project.clone()),
        )
        .expect("seed project db schema");

        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("test runtime");
        let saved_text = "cli named-project f7 bound write lands in project db";

        let (ct, daemon_task) = rt.block_on(async {
            // Daemon is global-only (project_db=None): the CLI's project DB
            // differs from the daemon's scope, exercising the named-project
            // forwarding branch in dispatch_cli_tool.
            let server = crate::MemoryServer::new(global.clone(), None).expect("daemon server");
            let (daemon_info, ct, daemon_task) =
                spawn_global_only_http_daemon(server, &global).await;

            // Write a scoped PID file so detect_daemon_for_global_db finds this
            // daemon for global DB G.
            let parsed_url: serde_json::Value = serde_json::from_str(
                &serde_json::json!({
                    "pid": std::process::id(),
                    "port": daemon_info
                        .url
                        .split(':')
                        .nth(2)
                        .and_then(|s| s.split('/').next())
                        .and_then(|s| s.parse::<u64>().ok())
                        .unwrap_or(0),
                    "url": daemon_info.url.clone(),
                    "global_db": global.display().to_string(),
                    "project_db": serde_json::Value::Null,
                    "version": env!("CARGO_PKG_VERSION"),
                })
                .to_string(),
            )
            .expect("pid json");
            let port = parsed_url["port"].as_u64().unwrap_or(0) as u16;
            let _ = port; // url is authoritative; port is for the TCP probe only
            let pid_path = crate::daemon_lock::scoped_daemon_pid_path(&tachi_home, &global);
            std::fs::create_dir_all(pid_path.parent().expect("pid parent")).expect("pid parent");
            std::fs::write(&pid_path, parsed_url.to_string()).expect("pid file");

            let mut args = serde_json::Map::new();
            args.insert("text".to_string(), serde_json::json!(saved_text));
            args.insert(
                "summary".to_string(),
                serde_json::json!("cli f7 named-project forward"),
            );
            args.insert(
                "path".to_string(),
                serde_json::json!("/tests/cli-named-project-f7"),
            );
            args.insert("category".to_string(), serde_json::json!("fact"));
            args.insert("scope".to_string(), serde_json::json!("project"));

            let result = dispatch_cli_tool(
                "save_memory",
                args,
                &global,
                Some(&cli_project),
                &tachi_home,
                |server, args| async move {
                    // In-process fallback. The daemon-forward path should handle
                    // this before we ever get here; reaching the fallback on the
                    // post-fix code would itself be a failure (duplicate write
                    // risk). On the pre-fix code this closure runs only if
                    // daemon detection itself missed.
                    let params: tachi_params::SaveMemoryParams =
                        serde_json::from_value(serde_json::Value::Object(args))
                            .map_err(|e| e.to_string())?;
                    crate::memory_search_ops::handle_save_memory(&server, params).await
                },
            )
            .await;

            let body = result.expect(
                "CLI named-project write should succeed (daemon session bound via X-Tachi-Project)",
            );
            let parsed: serde_json::Value = serde_json::from_str(&body)
                .unwrap_or_else(|e| panic!("save body JSON: {e}; {body}"));
            assert_eq!(
                parsed["ok"],
                serde_json::json!(true),
                "save_memory should report ok; full body: {body}"
            );
            (ct, daemon_task)
        });

        // The write must land in the CLI's named project DB, not the global DB.
        let count: i64 = rusqlite::Connection::open(&cli_project)
            .expect("open project db")
            .query_row(
                "SELECT COUNT(*) FROM memories WHERE text = ?1",
                [&saved_text],
                |row| row.get(0),
            )
            .expect("count in project db");
        assert_eq!(
            count, 1,
            "named-project write should land in the CLI project DB exactly once"
        );
        let global_count: i64 = rusqlite::Connection::open(&global)
            .expect("open global db")
            .query_row(
                "SELECT COUNT(*) FROM memories WHERE text = ?1",
                [&saved_text],
                |row| row.get(0),
            )
            .expect("count in global db");
        assert_eq!(
            global_count, 0,
            "named-project write must NOT land in the global DB"
        );

        ct.cancel();
        rt.block_on(daemon_task).expect("daemon task");
    }

    #[test]
    fn read_only_cli_wrappers_allow_daemon_fallback() {
        for tool_name in [
            "search_memory",
            "list_memories",
            "get_memory",
            "tachi_search",
            "wiki_search",
            "tachi_wiki_search",
            "vault_status",
            "vault_list",
        ] {
            assert!(
                cli_tool_allows_read_fallback(tool_name),
                "{tool_name} should be safe to run in-process when daemon forwarding misses"
            );
        }
        assert!(!cli_tool_allows_read_fallback("save_memory"));
        assert!(!cli_tool_allows_read_fallback("tachi_wiki_write"));
    }

    /// #1041 round-7 BUG regression (codex-17c966): with NO daemon reachable
    /// at all (fresh `app_home`, no pid file — the exact scenario
    /// `dispatch_cli_tool` falls through to its unconditional in-process
    /// branch for), a caller-supplied `project=` on the CLI must still be
    /// stamped `__tachi_project_explicit: true` before the handler sees it.
    /// Before the fix, this in-process path never ran
    /// `session_identity::enforce_session_project` (or anything equivalent)
    /// at all, so the marker was simply absent from the args map and
    /// `RememberParams::project_explicit` deserialized to its serde default
    /// (`false`) — indistinguishable, at `write_affinity`'s S1 gate, from a
    /// transport-injected default. A CLI `remember --project X` whose
    /// content's domain routed to an already-mounted project Y would then
    /// get silently rerouted into Y instead of landing in the caller's own
    /// X.
    #[tokio::test]
    async fn in_process_fallback_stamps_project_explicit_true_for_caller_supplied_project() {
        let temp = tempfile::tempdir().expect("tempdir");
        let app_home = temp.path().join("home");
        std::fs::create_dir_all(&app_home).expect("app_home dir");
        let global_db = temp.path().join("global.db");

        let mut args = serde_json::Map::new();
        args.insert(
            "text".to_string(),
            serde_json::json!("cli explicit project regression"),
        );
        args.insert("project".to_string(), serde_json::json!("some-project"));

        let body = dispatch_cli_tool(
            "remember",
            args,
            &global_db,
            None,
            &app_home,
            |_server, args_map| {
                Box::pin(async move {
                    let marker = args_map
                        .get(crate::session_identity::PROJECT_EXPLICIT_MARKER)
                        .cloned();
                    Ok(serde_json::json!({ "marker": marker }).to_string())
                })
            },
        )
        .await
        .expect("dispatch should reach the in-process fallback and succeed");

        let parsed: serde_json::Value = serde_json::from_str(&body).expect("json body");
        assert_eq!(
            parsed["marker"],
            serde_json::json!(true),
            "in-process fallback must stamp __tachi_project_explicit=true when \
             the CLI caller supplied an explicit project=, exactly like \
             enforce_session_project's Authoritative branch does for a bound \
             session — otherwise write_affinity's S1 gate treats a \
             caller-pinned project as a transport default and can silently \
             reroute the write; body={body}"
        );
    }

    /// Companion to the above: when the CLI caller omits `project=` entirely,
    /// the marker must stay `false` — presence of `project` in the raw args
    /// is the ONLY signal, matching `enforce_session_project`'s own
    /// unconditional-false-on-inject branch.
    #[tokio::test]
    async fn in_process_fallback_stamps_project_explicit_false_when_project_omitted() {
        let temp = tempfile::tempdir().expect("tempdir");
        let app_home = temp.path().join("home");
        std::fs::create_dir_all(&app_home).expect("app_home dir");
        let global_db = temp.path().join("global.db");

        let mut args = serde_json::Map::new();
        args.insert(
            "text".to_string(),
            serde_json::json!("cli default project regression"),
        );

        let body = dispatch_cli_tool(
            "remember",
            args,
            &global_db,
            None,
            &app_home,
            |_server, args_map| {
                Box::pin(async move {
                    let marker = args_map
                        .get(crate::session_identity::PROJECT_EXPLICIT_MARKER)
                        .cloned();
                    Ok(serde_json::json!({ "marker": marker }).to_string())
                })
            },
        )
        .await
        .expect("dispatch should reach the in-process fallback and succeed");

        let parsed: serde_json::Value = serde_json::from_str(&body).expect("json body");
        assert_eq!(
            parsed["marker"],
            serde_json::json!(false),
            "when the CLI caller omits project=, the marker must stay false — \
             only presence of project= in the raw args means caller-explicit; \
             body={body}"
        );
    }
}
