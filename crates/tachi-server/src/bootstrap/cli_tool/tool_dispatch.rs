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

/// Derive the named project a CLI invocation should declare to a daemon as
/// `X-Tachi-Project` at `initialize` (a one-time session bind), so the
/// daemon-side C1 guard (`reject_unbound_cross_project_write`) sees a bound
/// session and accepts an explicit `project=` arg in the body instead of
/// rejecting it as an unbound cross-tenant write.
///
/// Two sources feed this, in priority order:
/// 1. `project_db` (the CLI's separate `--project-db` PATH flag) — unchanged,
///    pre-existing behavior for every tool.
/// 2. (tachi#1224) a CLI *write* tool's own `--project` NAME flag, when (1)
///    yielded nothing. `remember --project X` (and, identically,
///    `wiki-write --project X`) with no separate `--project-db` used to put
///    `project=X` in the body with no matching header at all — the
///    daemon-side C1 guard then rejected the unbound write. Neither tool
///    appears in `session_identity::explicit_project_can_cross_binding`'s
///    allowlist (both are writes, not one of the established read-only
///    cross-project cases), so an explicit `--project` on either always
///    requires a bound session — deriving the header from the same value
///    already riding in the body is exactly that bind, nothing more.
///    `cli_tool_named_project_write` (below) is the single list of which CLI
///    tool names this fallback applies to.
///
/// Every other CLI tool that also carries a `project` arg (`search_memory`,
/// `tachi_wiki_search`, `get_memory`, ...) keeps its pre-existing header
/// derivation (from `project_db` only) unchanged — those rely on
/// `explicit_project_can_cross_binding` to allow an explicit cross-project
/// *read* without rebinding the session, and falling back to
/// `args["project"]` for those too would change that behavior.
fn daemon_forward_named_project(
    tool_name: &str,
    project_db: Option<&std::path::Path>,
    args: &serde_json::Map<String, serde_json::Value>,
) -> Option<String> {
    project_db
        .and_then(crate::path_utils::named_project_for_db_path)
        .or_else(|| {
            if cli_tool_named_project_write(tool_name) {
                args.get("project")
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_string())
            } else {
                None
            }
        })
}

/// CLI-facing tool names (as passed to `dispatch_cli_tool`, i.e. before
/// `cli_client::tool_map::remap_daemon_tool`'s later action-verb remap) whose
/// `--project` NAME flag must be forwarded as an `X-Tachi-Project` daemon
/// binding header when no separate `--project-db` PATH is set. Both are
/// mutating tools: `remember` (tachi#1224) and `tachi_wiki_write`
/// (tachi#1224 follow-up — `Commands::WikiWrite` hit the identical unbound
/// C1 rejection via the same `dispatch_cli_tool` path, since this fallback
/// was originally gated on `tool_name == "remember"` only). Read tools that
/// also carry a `project` arg (`search_memory`, `tachi_wiki_search`,
/// `get_memory`) are deliberately excluded — see `daemon_forward_named_project`
/// doc comment for why they must keep their unbound cross-project-read path.
fn cli_tool_named_project_write(tool_name: &str) -> bool {
    matches!(tool_name, "remember" | "tachi_wiki_write")
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
    // Compute the CLI's named project once (tachi#1224: see
    // `daemon_forward_named_project` for what feeds this and why). The
    // daemon-forwarding branches below declare this binding via
    // X-Tachi-Project so the daemon-side C1 guard
    // (`reject_unbound_cross_project_write`) sees a bound session and accepts
    // the explicit `project=` arg instead of rejecting it as an unbound
    // cross-tenant write.
    let cli_named_project =
        daemon_forward_named_project(tool_name, project_db.map(|path| path.as_path()), &args);
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
    use super::{cli_tool_allows_read_fallback, daemon_forward_named_project};
    use crate::bootstrap::cli_tool::tool_dispatch::dispatch_cli_tool;
    use crate::test_support::EnvRestore;
    use std::path::PathBuf;
    use tokio_util::sync::CancellationToken;

    /// Extract the first text content block's JSON payload from a
    /// `CallToolResult`. Mirrors `bootstrap/serve/stdio/tests.rs`'s
    /// `first_text`/`first_text_json` helpers of the same shape.
    fn first_text_json(result: &rmcp::model::CallToolResult) -> serde_json::Value {
        let text = result
            .content
            .iter()
            .find_map(|content| match &content.raw {
                rmcp::model::RawContent::Text(text) => Some(text.text.clone()),
                _ => None,
            })
            .expect("text tool result");
        serde_json::from_str(&text)
            .unwrap_or_else(|err| panic!("tool result json: {err}; text={text:?}"))
    }

    /// tachi#1224: `remember --project X` with no separate `--project-db`
    /// must translate into an `X-Tachi-Project: X` daemon-forward binding
    /// header — the exact discriminator the frozen spec calls for at the
    /// unit layer (no daemon/network needed; this is the pure computation
    /// that becomes `call_daemon_tool`'s `proxy_project` argument, which
    /// `call_daemon_tool_raw` turns 1:1 into the `X-Tachi-Project` header at
    /// `initialize`).
    #[test]
    fn remember_project_arg_forwards_as_named_project_header() {
        let mut args = serde_json::Map::new();
        args.insert("text".to_string(), serde_json::json!("hello"));
        args.insert("project".to_string(), serde_json::json!("X"));

        assert_eq!(
            daemon_forward_named_project("remember", None, &args),
            Some("X".to_string()),
            "remember --project X with no --project-db must still forward a \
             named-project binding header"
        );
    }

    /// Companion: `tachi remember TEXT` with no `--project` at all must not
    /// invent a header out of thin air.
    #[test]
    fn remember_without_project_arg_forwards_no_named_project_header() {
        let mut args = serde_json::Map::new();
        args.insert("text".to_string(), serde_json::json!("hello"));

        assert_eq!(
            daemon_forward_named_project("remember", None, &args),
            None,
            "remember with no --project must not forward any X-Tachi-Project \
             header"
        );
    }

    /// Scope guard: other CLI tools that also carry a `project` arg (e.g.
    /// `search_memory`) must NOT gain this args-fallback — several of them
    /// rely on `session_identity::explicit_project_can_cross_binding` to
    /// allow an explicit cross-project *read* without rebinding the session.
    /// Only `remember`'s header derivation changed by tachi#1224.
    #[test]
    fn non_remember_tool_with_project_arg_does_not_forward_header() {
        let mut args = serde_json::Map::new();
        args.insert("query".to_string(), serde_json::json!("hello"));
        args.insert("project".to_string(), serde_json::json!("X"));

        assert_eq!(
            daemon_forward_named_project("search_memory", None, &args),
            None,
            "search_memory must keep deriving its header from project_db \
             only, never from args[\"project\"] — that's the pre-existing, \
             unchanged cross-project-read path"
        );
    }

    /// tachi#1224 follow-up: `tachi wiki-write --project X` (CLI-facing tool
    /// name `tachi_wiki_write`, per `Commands::WikiWrite`'s dispatch call)
    /// with no separate `--project-db` hit the identical unbound C1
    /// rejection as `remember` did before the first tachi#1224 fix — this is
    /// the same discriminator, just for the second write-shaped CLI tool
    /// that carries a `project` NAME arg.
    #[test]
    fn wiki_write_project_arg_forwards_as_named_project_header() {
        let mut args = serde_json::Map::new();
        args.insert("title".to_string(), serde_json::json!("T"));
        args.insert("text".to_string(), serde_json::json!("body"));
        args.insert("project".to_string(), serde_json::json!("X"));

        assert_eq!(
            daemon_forward_named_project("tachi_wiki_write", None, &args),
            Some("X".to_string()),
            "wiki-write --project X with no --project-db must still forward \
             a named-project binding header"
        );
    }

    /// Companion: `tachi wiki-write TITLE TEXT` with no `--project` at all
    /// must not invent a header out of thin air.
    #[test]
    fn wiki_write_without_project_arg_forwards_no_named_project_header() {
        let mut args = serde_json::Map::new();
        args.insert("title".to_string(), serde_json::json!("T"));
        args.insert("text".to_string(), serde_json::json!("body"));

        assert_eq!(
            daemon_forward_named_project("tachi_wiki_write", None, &args),
            None,
            "wiki-write with no --project must not forward any \
             X-Tachi-Project header"
        );
    }

    /// Scope guard: `tachi_wiki_search` (the read sibling of
    /// `tachi_wiki_write`, both carrying a `project` arg) must NOT gain this
    /// args-fallback — it relies on
    /// `session_identity::explicit_project_can_cross_binding`'s
    /// `tachi_wiki_action_allows_cross_project_read` allowance ("search" is
    /// in that list) to allow an explicit cross-project *read* without
    /// rebinding the session.
    #[test]
    fn wiki_search_with_project_arg_does_not_forward_header() {
        let mut args = serde_json::Map::new();
        args.insert("query".to_string(), serde_json::json!("hello"));
        args.insert("project".to_string(), serde_json::json!("X"));

        assert_eq!(
            daemon_forward_named_project("tachi_wiki_search", None, &args),
            None,
            "tachi_wiki_search must keep deriving its header from \
             project_db only, never from args[\"project\"] — that's the \
             pre-existing, unchanged cross-project-read path"
        );
    }

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

    /// tachi#1224 regression: `tachi remember TEXT --project X` with NO
    /// separate `--project-db` — the common real CLI shape, where the CLI's
    /// own `project_db` argument is `None` and a single global-only daemon is
    /// already running. Unlike
    /// `cli_named_project_forward_binds_daemon_session_for_write` above
    /// (which exercises the SECOND/mismatch branch via a `project_db`
    /// difference), this exercises the FIRST/matching branch in
    /// `dispatch_cli_tool_with_migration_authority` — the branch whose
    /// `cli_named_project` used to be derived from `project_db` only (always
    /// `None` here) and therefore forwarded no `X-Tachi-Project` header at
    /// all, so the daemon-side C1 guard rejected the unbound `project=` arg
    /// this branch was still sending in the body. After the fix, the header
    /// is derived from `remember`'s own `args["project"]` and the write
    /// succeeds without ever reaching the in-process fallback closure.
    #[test]
    fn remember_named_project_forward_binds_daemon_session_without_project_db() {
        let _guard = crate::utils::global_test_lock()
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let _is_daemon = EnvRestore::set("TACHI_DAEMON", "1");

        let temp = tempfile::tempdir().expect("tempdir");
        let tachi_home = temp.path().join("home");
        let global = tachi_home.join("global/memory.db");
        let cli_project_name = "Sigil-cli-remember-1224";
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

        // Seed the project DB schema so the daemon can open it by name.
        let _seed = crate::MemoryServer::new(
            tachi_home.join(format!("seed-{}.db", uuid::Uuid::new_v4())),
            Some(cli_project.clone()),
        )
        .expect("seed project db schema");

        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("test runtime");
        let saved_text = "cli remember 1224 bound write lands in named project db";

        let (ct, daemon_task) = rt.block_on(async {
            // Daemon is global-only (project_db=None). The CLI request below
            // ALSO passes project_db=None (no --project-db flag), so
            // `daemon_matches_requested_dbs` matches and this exercises the
            // PRIMARY (matching) forwarding branch, not the mismatch branch.
            let server = crate::MemoryServer::new(global.clone(), None).expect("daemon server");
            let (daemon_info, ct, daemon_task) =
                spawn_global_only_http_daemon(server, &global).await;

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
            let pid_path = crate::daemon_lock::scoped_daemon_pid_path(&tachi_home, &global);
            std::fs::create_dir_all(pid_path.parent().expect("pid parent")).expect("pid parent");
            std::fs::write(&pid_path, parsed_url.to_string()).expect("pid file");

            let mut args = serde_json::Map::new();
            args.insert("text".to_string(), serde_json::json!(saved_text));
            args.insert(
                "summary".to_string(),
                serde_json::json!("cli remember 1224 named-project forward"),
            );
            args.insert(
                "path".to_string(),
                serde_json::json!("/tests/cli-remember-1224"),
            );
            args.insert("category".to_string(), serde_json::json!("fact"));
            args.insert("project".to_string(), serde_json::json!(cli_project_name));

            let result = dispatch_cli_tool(
                "remember",
                args,
                &global,
                None, // no --project-db: exactly the ticket's CLI shape
                &tachi_home,
                |server, args_map| async move {
                    // In-process fallback. The daemon-forward path should
                    // handle this before we ever get here; reaching the
                    // fallback on the post-fix code would itself be a
                    // failure (duplicate-write risk, and proof the header
                    // was never sent).
                    let params: crate::tool_params::RememberParams =
                        serde_json::from_value(serde_json::Value::Object(args_map))
                            .map_err(|e| e.to_string())?;
                    crate::memory_search_ops::handle_remember(&server, params).await
                },
            )
            .await;

            let body = result.expect(
                "remember --project X with no --project-db should succeed via the daemon-forward \
                 binding (X-Tachi-Project header derived from args[\"project\"])",
            );
            let parsed: serde_json::Value = serde_json::from_str(&body)
                .unwrap_or_else(|e| panic!("remember body JSON: {e}; {body}"));
            assert_eq!(
                parsed["ok"],
                serde_json::json!(true),
                "remember should report ok; full body: {body}"
            );
            (ct, daemon_task)
        });

        // The write must land in the named project DB, not the global DB.
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
            "remember --project X write should land in the named project DB exactly once"
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
            "remember --project X write must NOT land in the global DB"
        );

        ct.cancel();
        rt.block_on(daemon_task).expect("daemon task");
    }

    /// tachi#1224 follow-up regression: `tachi wiki-write TITLE TEXT --project
    /// X` with NO separate `--project-db` — the CLI shape review flagged as
    /// still broken (`cli_tool_named_project_write` originally gated the
    /// header-forward fallback on `tool_name == "remember"` only, so
    /// `dispatch_cli_tool("tachi_wiki_write", ...)` derived no header and
    /// the daemon-side C1 guard rejected the write). End-to-end mirror of
    /// `remember_named_project_forward_binds_daemon_session_without_project_db`
    /// above, for the second write-shaped CLI tool.
    #[test]
    fn wiki_write_named_project_forward_binds_daemon_session_without_project_db() {
        let _guard = crate::utils::global_test_lock()
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let _is_daemon = EnvRestore::set("TACHI_DAEMON", "1");

        let temp = tempfile::tempdir().expect("tempdir");
        let tachi_home = temp.path().join("home");
        let global = tachi_home.join("global/memory.db");
        let cli_project_name = "Sigil-cli-wiki-write-1224";
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

        // Seed the project DB schema so the daemon can open it by name.
        let _seed = crate::MemoryServer::new(
            tachi_home.join(format!("seed-{}.db", uuid::Uuid::new_v4())),
            Some(cli_project.clone()),
        )
        .expect("seed project db schema");

        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("test runtime");
        let saved_text = "cli wiki-write 1224 bound write lands in named project db, not global";

        let (ct, daemon_task) = rt.block_on(async {
            // Daemon is global-only (project_db=None). The CLI request below
            // ALSO passes project_db=None (no --project-db flag), so
            // `daemon_matches_requested_dbs` matches and this exercises the
            // PRIMARY (matching) forwarding branch, not the mismatch branch.
            let server = crate::MemoryServer::new(global.clone(), None).expect("daemon server");
            let (daemon_info, ct, daemon_task) =
                spawn_global_only_http_daemon(server, &global).await;

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
            let pid_path = crate::daemon_lock::scoped_daemon_pid_path(&tachi_home, &global);
            std::fs::create_dir_all(pid_path.parent().expect("pid parent")).expect("pid parent");
            std::fs::write(&pid_path, parsed_url.to_string()).expect("pid file");

            let mut args = serde_json::Map::new();
            args.insert(
                "title".to_string(),
                serde_json::json!("cli wiki-write 1224"),
            );
            args.insert("text".to_string(), serde_json::json!(saved_text));
            args.insert("project".to_string(), serde_json::json!(cli_project_name));

            let result = dispatch_cli_tool(
                "tachi_wiki_write",
                args,
                &global,
                None, // no --project-db: exactly the ticket's CLI shape
                &tachi_home,
                |server, args_map| async move {
                    // In-process fallback. The daemon-forward path should
                    // handle this before we ever get here; reaching the
                    // fallback on the post-fix code would itself be a
                    // failure (duplicate-write risk, and proof the header
                    // was never sent).
                    let params: crate::tool_params::WikiWriteParams =
                        serde_json::from_value(serde_json::Value::Object(args_map))
                            .map_err(|e| e.to_string())?;
                    crate::copilot_ops::handle_tachi_wiki_write(&server, params).await
                },
            )
            .await;

            let body = result.expect(
                "wiki-write --project X with no --project-db should succeed via the \
                 daemon-forward binding (X-Tachi-Project header derived from \
                 args[\"project\"])",
            );
            let parsed: serde_json::Value = serde_json::from_str(&body)
                .unwrap_or_else(|e| panic!("wiki_write body JSON: {e}; {body}"));
            assert!(
                parsed.get("id").is_some(),
                "wiki_write should report a saved id; full body: {body}"
            );
            (ct, daemon_task)
        });

        // The write must land in the named project DB, not the global DB.
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
            "wiki-write --project X write should land in the named project DB exactly once"
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
            "wiki-write --project X write must NOT land in the global DB"
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

    // --- tachi#1224 CONCERN follow-up: project-name boundary pins (non-ASCII,
    // spaces, nonexistent, empty). Test-only; no product code changes. Every
    // assertion below reflects behavior already present on this branch — these
    // are regression locks (baseline GREEN), not new-bug reproductions, EXCEPT
    // where a doc comment explicitly flags an escalated finding.

    /// CONCERN item 1 (CLI dispatch layer): `daemon_forward_named_project`
    /// performs zero validation or sanitization on the project name it reads
    /// out of `args["project"]` — a non-ASCII value passes through
    /// byte-for-byte. Any rejection or silent rewriting happens further
    /// downstream (transport.rs's header construction, or
    /// `sanitize_safe_path_name`'s resolution — see the live-daemon tests
    /// below), never at this layer.
    #[test]
    fn remember_project_arg_with_non_ascii_name_forwards_unvalidated() {
        let mut args = serde_json::Map::new();
        args.insert("text".to_string(), serde_json::json!("hello"));
        args.insert("project".to_string(), serde_json::json!("量化"));

        assert_eq!(
            daemon_forward_named_project("remember", None, &args),
            Some("量化".to_string()),
            "a non-ASCII --project value must forward unchanged, not be \
             rejected or rewritten, at the CLI dispatch layer"
        );
    }

    /// CONCERN item 2 (CLI dispatch layer): same unvalidated pass-through for
    /// a project name containing a plain space.
    #[test]
    fn remember_project_arg_with_embedded_space_forwards_unvalidated() {
        let mut args = serde_json::Map::new();
        args.insert("text".to_string(), serde_json::json!("hello"));
        args.insert("project".to_string(), serde_json::json!("my project"));

        assert_eq!(
            daemon_forward_named_project("remember", None, &args),
            Some("my project".to_string()),
            "a --project value containing a space must forward unchanged"
        );
    }

    /// CONCERN item 4 (CLI dispatch layer): an explicitly-empty
    /// `--project ""` is still `Some("")` at THIS layer —
    /// `args.get("project")` sees a present (if empty) string key, not an
    /// absent one. The empty-string-as-absent normalization happens later,
    /// server-side (`session_identity::normalize_identity_value`), already
    /// pinned by that module's own `normalize_identity_trims_and_rejects_empty`
    /// test — this test pins the DISTINCT, earlier fact that the CLI layer
    /// itself does not perform that collapse.
    #[test]
    fn remember_project_arg_with_empty_string_forwards_as_explicit_empty() {
        let mut args = serde_json::Map::new();
        args.insert("text".to_string(), serde_json::json!("hello"));
        args.insert("project".to_string(), serde_json::json!(""));

        assert_eq!(
            daemon_forward_named_project("remember", None, &args),
            Some(String::new()),
            "an explicit but empty --project value is still forwarded as \
             Some(\"\") at the CLI dispatch layer, not treated as absent"
        );
    }

    /// CONCERN item 3, full stack: a `--project` NAME that is legal ASCII but
    /// does not correspond to any registered project must fail the daemon's
    /// `initialize` binding check closed. `apply_http_session_identity`
    /// (server_handler.rs:284-314) DOES construct the expected
    /// `"invalid HTTP direct-connect project binding: {err}"` `ErrorData` and
    /// hand it to the rmcp streamable-HTTP server, which sends it as a
    /// JSON-RPC error frame and then tears the session down
    /// (`service/server.rs`'s `ServerInitializeError::InitializeFailed` path).
    ///
    /// Oz run 2026-07-17 (three-run deterministic red): the CLI-side
    /// `rmcp::service::client` handshake reader does NOT observe that error
    /// frame — it sees the stream end first and reports
    /// `ClientInitializeError::ConnectionClosed("initialize response")`,
    /// which `call_daemon_tool_raw` wraps as `"daemon handshake failed at
    /// {url}: connection closed: initialize response"`. This is still
    /// fail-closed (the call errors, no data crosses the boundary) — it is
    /// just a connection-teardown race in the streamable-HTTP transport
    /// (send-then-drop on the server side) rather than the client cleanly
    /// receiving the structured JSON-RPC error text. Pinning the actual shape
    /// here rather than the aspirational one: if that streamable-HTTP
    /// race is ever fixed upstream so the client reliably sees the
    /// `JsonRpcError` frame instead, this assertion should be tightened back
    /// to checking for `"invalid HTTP direct-connect project binding"` and
    /// `"not found"`.
    #[test]
    fn daemon_call_rejects_nonexistent_ascii_project_name_binding() {
        let _guard = crate::utils::global_test_lock()
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let temp = tempfile::tempdir().expect("tempdir");
        let tachi_home = temp.path().join("home");
        let global = tachi_home.join("global/memory.db");
        std::fs::create_dir_all(global.parent().expect("global parent")).expect("global parent");
        let _tachi_home = EnvRestore::set_path("TACHI_HOME", &tachi_home);
        let _sigil_home = EnvRestore::remove("SIGIL_HOME");
        let _app_home = EnvRestore::remove("TACHI_APP_HOME");

        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("test runtime");

        let (result, ct, daemon_task) = rt.block_on(async {
            let server = crate::MemoryServer::new(global.clone(), None).expect("daemon server");
            let (daemon_info, ct, daemon_task) =
                spawn_global_only_http_daemon(server, &global).await;
            let mut args = serde_json::Map::new();
            args.insert("action".to_string(), serde_json::json!("briefing"));
            let params = rmcp::model::CallToolRequestParams::new("tachi_memory".to_string())
                .with_arguments(args);
            let result = crate::cli_client::call_daemon_tool_raw(
                &daemon_info,
                params,
                Some("definitely-not-a-real-project-xyz123"),
            )
            .await;
            (result, ct, daemon_task)
        });

        let err = result.expect_err("binding to a nonexistent project name must fail closed");
        let message = err.to_string();
        assert!(
            message.contains("daemon handshake failed"),
            "unexpected error: {message}"
        );
        assert!(
            message.contains("connection closed"),
            "expected the streamable-HTTP client to observe a torn-down \
             connection during initialize (fail-closed via disconnect, not a \
             structured JSON-RPC error) — got: {message}"
        );

        ct.cancel();
        rt.block_on(daemon_task).expect("daemon task");
    }

    /// THIS PINS BUG #1228 — a live specimen, not a regression lock.
    ///
    /// Ground truth (verified directly against `http` 1.4.2, this crate's
    /// pinned version per Cargo.lock, in an isolated scratch crate — NOT
    /// this repo's build) is that `HeaderValue::from_str` accepts non-ASCII
    /// UTF-8 text; it only rejects embedded control characters
    /// (CR/LF/NUL/DEL). It does NOT reject Chinese, emoji, or any other
    /// valid non-ASCII text (see transport.rs's own test module for the
    /// header-construction-layer half of this pin). That much was already
    /// correctly documented here. What this test got wrong was what happens
    /// NEXT: the dispatch packet (and the prior version of this test)
    /// expected the non-ASCII name to reach `apply_http_session_identity`
    /// and fail closed as an unknown project, the same failure family as
    /// `daemon_call_rejects_nonexistent_ascii_project_name_binding` above.
    ///
    /// Oz run 2026-07-17 (three-run deterministic red) shows that is not
    /// what happens: `"量化"` is entirely non-ASCII, so
    /// `sanitize_safe_path_name` (crates/tachi-server/src/utils/text.rs)
    /// maps every character to `_`, trims the `_`/`.`/`-` boundary chars,
    /// and lands on an EMPTY string — which its own empty-collapse fallback
    /// then rewrites to the literal project name `"unnamed"`. `"unnamed"`
    /// resolves successfully (it is the sanitizer's own fallback identity,
    /// not a real caller-registered project), so
    /// `apply_http_session_identity` binds the session as `unnamed` instead
    /// of rejecting it, and the `tachi_memory` briefing call SUCCEEDS
    /// (`"status":"completed"`) instead of failing closed. This is
    /// tachi#1228: a non-ASCII (or any all-punctuation/all-non-alnum)
    /// `--project` name silently collapses onto a shared fallback project
    /// rather than being rejected as unknown or preserved verbatim — a
    /// binding-confusion hole, not the fail-closed behavior every other
    /// malformed-name case in this file exhibits.
    ///
    /// This test PINS the current (buggy) success so a future fix to #1228
    /// shows up as a red here, not a silent regression. When #1228 lands
    /// fail-closed behavior for the sanitize-collapse case, FLIP this
    /// assertion to expect an error containing "invalid HTTP direct-connect
    /// project binding" (or whatever the fixed rejection text becomes) —
    /// do not just delete this test.
    #[test]
    fn non_ascii_project_binding_currently_succeeds_via_sanitize_collapse_bug_1228() {
        let _guard = crate::utils::global_test_lock()
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let temp = tempfile::tempdir().expect("tempdir");
        let tachi_home = temp.path().join("home");
        let global = tachi_home.join("global/memory.db");
        std::fs::create_dir_all(global.parent().expect("global parent")).expect("global parent");
        let _tachi_home = EnvRestore::set_path("TACHI_HOME", &tachi_home);
        let _sigil_home = EnvRestore::remove("SIGIL_HOME");
        let _app_home = EnvRestore::remove("TACHI_APP_HOME");

        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("test runtime");

        let (result, ct, daemon_task) = rt.block_on(async {
            let server = crate::MemoryServer::new(global.clone(), None).expect("daemon server");
            let (daemon_info, ct, daemon_task) =
                spawn_global_only_http_daemon(server, &global).await;
            let mut args = serde_json::Map::new();
            args.insert("action".to_string(), serde_json::json!("briefing"));
            let params = rmcp::model::CallToolRequestParams::new("tachi_memory".to_string())
                .with_arguments(args);
            let result =
                crate::cli_client::call_daemon_tool_raw(&daemon_info, params, Some("量化")).await;
            (result, ct, daemon_task)
        });

        let call_result = result.unwrap_or_else(|err| {
            panic!(
                "THIS PINS BUG #1228 as a live success, not a failure — if \
                 the daemon now rejects this call, #1228's sanitize collapse \
                 has apparently already changed shape; do not silently \
                 delete this test, re-diagnose and update its pin. Got \
                 error instead of success: {err}"
            )
        });
        let status = first_text_json(&call_result)["status"].clone();
        assert_eq!(
            status,
            serde_json::json!("completed"),
            "BUG #1228 live specimen: a non-ASCII --project value \
             (\"量化\") collapses via sanitize_safe_path_name to the empty \
             string, which falls back to the shared \"unnamed\" project, so \
             the briefing call succeeds instead of failing closed as an \
             unknown project. full result={call_result:?}"
        );

        ct.cancel();
        rt.block_on(daemon_task).expect("daemon task");
    }

    /// CONCERN item 2, full stack: a project name containing a plain space
    /// (`sanitize_safe_path_name` maps the space to `_`, giving the
    /// non-empty name `my_project` — distinct from the non-ASCII case above,
    /// which collapses to empty and hits the `unnamed` fallback instead) is
    /// accepted by header construction, then fails once it reaches the
    /// daemon's binding resolution because no project named `my_project` is
    /// registered.
    ///
    /// Same streamable-HTTP connection-teardown race as the ASCII-nonexistent
    /// test above (Oz run 2026-07-17, deterministic): the client observes
    /// `"connection closed: initialize response"` rather than the structured
    /// `"invalid HTTP direct-connect project binding"` JSON-RPC error text,
    /// even though the server-side rejection did fire. Still fail-closed
    /// (no data crosses); pinning the actual disconnect shape here. If the
    /// streamable-HTTP race is fixed upstream, tighten this back to asserting
    /// the structured error text.
    #[test]
    fn daemon_call_accepts_project_name_with_space_then_fails_as_unknown_project() {
        let _guard = crate::utils::global_test_lock()
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let temp = tempfile::tempdir().expect("tempdir");
        let tachi_home = temp.path().join("home");
        let global = tachi_home.join("global/memory.db");
        std::fs::create_dir_all(global.parent().expect("global parent")).expect("global parent");
        let _tachi_home = EnvRestore::set_path("TACHI_HOME", &tachi_home);
        let _sigil_home = EnvRestore::remove("SIGIL_HOME");
        let _app_home = EnvRestore::remove("TACHI_APP_HOME");

        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("test runtime");

        let (result, ct, daemon_task) = rt.block_on(async {
            let server = crate::MemoryServer::new(global.clone(), None).expect("daemon server");
            let (daemon_info, ct, daemon_task) =
                spawn_global_only_http_daemon(server, &global).await;
            let mut args = serde_json::Map::new();
            args.insert("action".to_string(), serde_json::json!("briefing"));
            let params = rmcp::model::CallToolRequestParams::new("tachi_memory".to_string())
                .with_arguments(args);
            let result =
                crate::cli_client::call_daemon_tool_raw(&daemon_info, params, Some("my project"))
                    .await;
            (result, ct, daemon_task)
        });

        let err = result.expect_err("an unregistered project name must fail closed");
        let message = err.to_string();
        assert!(
            !message.contains("invalid proxy project header value"),
            "a project name containing a plain space must not be rejected at \
             header construction; got: {message}"
        );
        assert!(
            message.contains("daemon handshake failed"),
            "unexpected error: {message}"
        );
        assert!(
            message.contains("connection closed"),
            "expected the streamable-HTTP client to observe a torn-down \
             connection during initialize (fail-closed via disconnect, not a \
             structured JSON-RPC error) — got: {message}"
        );

        ct.cancel();
        rt.block_on(daemon_task).expect("daemon task");
    }

    /// CONCERN item 4, full stack: an empty `--project ""` header value is
    /// accepted by header construction (see transport.rs's own test) AND then
    /// normalized away to "absent" server-side
    /// (`session_identity::normalize_identity_value("")` is `None` — already
    /// pinned by that module's own `normalize_identity_trims_and_rejects_empty`
    /// test). The net effect, proven live here, is that the session binds as
    /// UNBOUND rather than failing closed: the call succeeds (no "invalid
    /// HTTP direct-connect project binding" error) — an empty `--project` is
    /// treated as though it were never passed, not rejected.
    #[test]
    fn daemon_call_with_empty_project_name_is_treated_as_unbound_not_rejected() {
        let _guard = crate::utils::global_test_lock()
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let temp = tempfile::tempdir().expect("tempdir");
        let tachi_home = temp.path().join("home");
        let global = tachi_home.join("global/memory.db");
        std::fs::create_dir_all(global.parent().expect("global parent")).expect("global parent");
        let _tachi_home = EnvRestore::set_path("TACHI_HOME", &tachi_home);
        let _sigil_home = EnvRestore::remove("SIGIL_HOME");
        let _app_home = EnvRestore::remove("TACHI_APP_HOME");

        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("test runtime");

        let (result, ct, daemon_task) = rt.block_on(async {
            let server = crate::MemoryServer::new(global.clone(), None).expect("daemon server");
            let (daemon_info, ct, daemon_task) =
                spawn_global_only_http_daemon(server, &global).await;
            let mut args = serde_json::Map::new();
            args.insert("action".to_string(), serde_json::json!("briefing"));
            let params = rmcp::model::CallToolRequestParams::new("tachi_memory".to_string())
                .with_arguments(args);
            let result =
                crate::cli_client::call_daemon_tool_raw(&daemon_info, params, Some("")).await;
            (result, ct, daemon_task)
        });

        assert!(
            result.is_ok(),
            "an empty --project value must not fail the daemon binding check \
             (it is normalized away to \"absent\", not rejected); got: {:?}",
            result.err().map(|e| e.to_string())
        );

        ct.cancel();
        rt.block_on(daemon_task).expect("daemon task");
    }

    /// CONCERN escalation (flagged in the delivery report, not fixed here):
    /// `resolve_named_project_db_path` — the exact function
    /// `apply_http_session_identity` calls at server_handler.rs:284-314 to
    /// resolve a named-project binding — feeds its input through
    /// `sanitize_safe_path_name` (`crate::utils::sanitize_safe_path_name`),
    /// which maps every non-ASCII-alphanumeric character to `_` and then
    /// trims leading/trailing `_`/`.`/`-`. A project name made ENTIRELY of
    /// non-ASCII characters (e.g. "量化", two CJK characters) sanitizes to
    /// two underscores, which then trim away to nothing, falling through to
    /// the `"unnamed"` fallback (`utils/text.rs`'s
    /// `sanitize_safe_path_name`). This is a SILENT identity collision: a
    /// caller declaring `--project 量化` resolves to the EXACT SAME database
    /// as a caller who legitimately declared `--project unnamed`, with no
    /// error, warning, or any signal that the name was rewritten. This test
    /// pins the collision as a reproducible fact (not an endorsement) — it is
    /// the concrete evidence behind the "candidate silent cross-project
    /// misroute" escalation called out in this contract's delivery report,
    /// per the dispatch packet's STOP-on-silent-misrouting clause. Test-only;
    /// zero product code changed by this contract.
    #[test]
    fn non_ascii_only_project_name_collides_with_the_literal_project_named_unnamed() {
        let _guard = crate::utils::global_test_lock()
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let temp = tempfile::tempdir().expect("tempdir");
        let tachi_home = temp.path().join("home");
        let _tachi_home = EnvRestore::set_path("TACHI_HOME", &tachi_home);
        let _sigil_home = EnvRestore::remove("SIGIL_HOME");
        let _app_home = EnvRestore::remove("TACHI_APP_HOME");

        let unnamed_db = tachi_home.join("projects/unnamed/memory.db");
        std::fs::create_dir_all(unnamed_db.parent().expect("parent")).expect("parent dir");
        std::fs::write(&unnamed_db, b"unnamed project db placeholder")
            .expect("write placeholder db");

        let resolved = crate::MemoryServer::resolve_named_project_db_path("量化").expect(
            "sanitize_safe_path_name collapses an all-non-ASCII project name to \
             \"unnamed\" with no rejection — this resolves successfully, which \
             is exactly the silent-collision behavior this test pins",
        );

        assert_eq!(
            resolved, unnamed_db,
            "a caller declaring --project 量化 must not silently resolve to the \
             SAME database path as a caller who declared --project unnamed — \
             this is a candidate silent cross-project misroute (see this \
             test's doc comment); escalated, not fixed, by this test-only change"
        );
    }
}
