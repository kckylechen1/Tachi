use super::*;
use rmcp::{
    model::*,
    service::{RequestContext, RoleServer},
    ServerHandler,
};
use serde_json::{json, Value};
use std::sync::{Arc, Mutex};

#[derive(Clone)]
struct ObservedServer {
    inner: crate::MemoryServer,
    calls: Arc<Mutex<Vec<(String, Value)>>>,
}

impl ServerHandler for ObservedServer {
    fn supported_protocol_versions(&self) -> std::borrow::Cow<'static, [ProtocolVersion]> {
        self.inner.supported_protocol_versions()
    }
    fn get_info(&self) -> ServerInfo {
        self.inner.get_info()
    }
    async fn initialize(
        &self,
        request: InitializeRequestParams,
        context: RequestContext<RoleServer>,
    ) -> Result<InitializeResult, rmcp::ErrorData> {
        self.inner.initialize(request, context).await
    }
    async fn call_tool(
        &self,
        request: CallToolRequestParams,
        context: RequestContext<RoleServer>,
    ) -> Result<CallToolResponse, rmcp::ErrorData> {
        let name = request.name.to_string();
        let response = self.inner.call_tool(request, context).await;
        let body = match &response {
            Ok(CallToolResponse::Complete(body)) => {
                serde_json::to_value(body).expect("response JSON")
            }
            Ok(_) => json!({"rpc_error":"non-complete response"}),
            Err(error) => json!({"rpc_error":error}),
        };
        self.calls
            .lock()
            .expect("call observations")
            .push((name, body));
        response
    }
}

#[test]
fn cli_daemon_scope_mismatch_preserves_global_and_explicit_project_intent() {
    assert_cli_daemon_scope_matrix(false, &[None, Some("cli-scope"), Some("other-scope")]);
}

#[test]
fn cli_daemon_scope_mismatch_preserves_domain_route_authority() {
    // A daemon statically bound to the routed destination has a separate
    // pre-existing affinity limitation; this checks global-only and matching
    // workspace daemons without claiming to change that policy.
    assert_cli_daemon_scope_matrix(true, &[None, Some("cli-scope")]);
}

fn assert_cli_daemon_scope_matrix(domain_routes: bool, daemon_projects: &[Option<&str>]) {
    let _lock = crate::utils::global_test_lock()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let home = tempfile::tempdir().expect("home");
    let app_home = home.path().to_path_buf();
    let global = app_home.join("global/memory.db");
    let project = app_home.join("projects/cli-scope/memory.db");
    let other = app_home.join("projects/other-scope/memory.db");
    let _home = EnvRestore::set_path("TACHI_HOME", &app_home);
    let _sigil = EnvRestore::remove("SIGIL_HOME");
    let _app = EnvRestore::remove("TACHI_APP_HOME");
    let _no_auto = EnvRestore::set("TACHI_DISABLE_AUTO_DAEMON", "1");
    let _daemon = EnvRestore::set("TACHI_DAEMON", "1");
    if domain_routes {
        std::fs::write(
            app_home.join("routing.json"),
            json!({"domain_routes": [{
                "project": "other-scope",
                "domains": ["cli_routing_regression"]
            }]})
            .to_string(),
        )
        .expect("routing fixture");
    }
    for path in [&project, &other] {
        std::fs::create_dir_all(path.parent().expect("project parent")).expect("project dir");
        drop(crate::MemoryServer::new(global.clone(), Some(path.clone())).expect("seed schema"));
    }
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("runtime")
        .block_on(async {
            for daemon_project in daemon_projects {
                let posture = daemon_project.unwrap_or("global-only");
                let daemon_db = daemon_project.map(|name| app_home.join("projects").join(name).join("memory.db"));
                let server = crate::MemoryServer::new(global.clone(), daemon_db).expect("daemon server");
                let (info, ct, task) = spawn_http_daemon(server, &global).await;
                let _shutdown = ct.clone().drop_guard();
                let port = url::Url::parse(&info.url).expect("daemon URL").port().expect("port");
                let receipt = crate::daemon_lock::scoped_daemon_pid_path(&app_home, &global);
                std::fs::create_dir_all(receipt.parent().expect("receipt parent")).expect("receipt dir");
                crate::utils::write_json_file_owner_only(
                    &receipt,
                    &json!({
                        "pid": std::process::id(),
                        "port": port,
                        "url": info.url,
                        "global_db": global,
                        "project_db": info.project_db,
                        "version": env!("CARGO_PKG_VERSION"),
                        "internal_proxy_token": info.internal_proxy_token,
                    }),
                )
                .expect("fixture receipt");

                let implicit_target = if domain_routes { &other } else { &project };
                for (scope, explicit_project, label, target) in [
                    ("project", None, "implicit-project", implicit_target),
                    ("global", None, "global", &global),
                    ("global", Some("cli-scope"), "explicit-project", &project),
                ] {
                    let path = format!("/notes/cli-scope-{posture}-{label}");
                    let mut args = json!({
                        "text": format!("CLI scope routing regression for {posture} {label}, with a resident daemon and workspace library."),
                        "path": path,
                        "scope": scope,
                        "force": true,
                    })
                    .as_object()
                    .expect("args")
                    .clone();
                    if let Some(name) = explicit_project {
                        args.insert("project".into(), json!(name));
                    }
                    if domain_routes {
                        args.insert("domain".into(), json!("cli_routing_regression"));
                    }
                    let body = dispatch_cli_tool(
                        "remember",
                        args,
                        &global,
                        Some(&project),
                        &app_home,
                        |_, _| async { Err("in-process fallback must not run".to_string()) },
                    )
                    .await
                    .unwrap_or_else(|error| panic!("{posture} {label} must complete on daemon: {error}"));
                    let result: Value = serde_json::from_str(&body).expect("save JSON");
                    assert!(result["id"].is_string(), "save receipt: {result}");
                    for db in [&global, &project, &other] {
                        let count: i64 = rusqlite::Connection::open_with_flags(
                            db,
                            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
                        )
                        .expect("read DB")
                        .query_row("SELECT COUNT(*) FROM memories WHERE path = ?1", [&path], |row| {
                            row.get(0)
                        })
                        .expect("row count");
                        assert_eq!(count, i64::from(db == target), "{posture} {label} in {}", db.display());
                    }
                }

                let rejected_path = format!("/notes/cli-scope-{posture}-rejected");
                let args = json!({
                    "text": "A conflicting explicit project must still be rejected before a write.",
                    "path": rejected_path,
                    "scope": "global",
                    "project": "other-scope",
                    "force": true,
                })
                .as_object()
                .expect("args")
                .clone();
                let error = dispatch_cli_tool(
                    "remember",
                    args,
                    &global,
                    Some(&project),
                    &app_home,
                    |_, _| async { Err("in-process fallback must not run".to_string()) },
                )
                .await
                .expect_err("cross-project write must stay rejected");
                assert!(error.to_string().contains("project binding mismatch"), "{posture}: {error}");
                for db in [&global, &project, &other] {
                    let count: i64 = rusqlite::Connection::open_with_flags(
                        db,
                        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
                    )
                    .expect("read DB")
                    .query_row(
                        "SELECT COUNT(*) FROM memories WHERE path = ?1",
                        [&rejected_path],
                        |row| row.get(0),
                    )
                    .expect("rejected row count");
                    assert_eq!(count, 0, "{posture} rejected write in {}", db.display());
                }
                ct.cancel();
                task.await.expect("daemon stopped");
            }
        });
}

#[test]
fn operator_cli_wiki_and_list_commands_reach_authorized_daemon_without_fallback() {
    let _lock = crate::utils::global_test_lock()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let home = tempfile::tempdir().expect("home");
    let app_home = home.path().to_path_buf();
    let global = app_home.join("global/memory.db");
    let _home = EnvRestore::set_path("TACHI_HOME", &app_home);
    let _sigil = EnvRestore::remove("SIGIL_HOME");
    let _app = EnvRestore::remove("TACHI_APP_HOME");
    let _no_auto = EnvRestore::set("TACHI_DISABLE_AUTO_DAEMON", "1");
    let _daemon = EnvRestore::set("TACHI_DAEMON", "1");
    tokio::runtime::Builder::new_current_thread().enable_all().build().expect("runtime").block_on(async {
        use rmcp::transport::streamable_http_server::{
            session::local::LocalSessionManager, StreamableHttpServerConfig, StreamableHttpService,
        };
        let wiki = app_home.join("projects/wiki/memory.db");
        std::fs::create_dir_all(wiki.parent().expect("wiki parent")).expect("wiki directory");
        drop(crate::MemoryServer::new(global.clone(), Some(wiki)).expect("seed wiki schema"));
        let server = crate::MemoryServer::new(global.clone(), None).expect("server");
        server.set_tool_profile(Some(tachi_hub::ToolProfile::operate()));
        let token = uuid::Uuid::new_v4().simple().to_string();
        server.set_daemon_proxy_token(token.clone());
        let calls = Arc::new(Mutex::new(Vec::new()));
        let recorded = calls.clone();
        let config = StreamableHttpServerConfig::default()
            .with_legacy_session_mode(true)
            .with_stateless_protocol_metadata_required(true);
        let factory_server = server.clone();
        let service = StreamableHttpService::new(
            move || {
                Ok(ObservedServer {
                    inner: factory_server.clone_for_mcp_session(),
                    calls: recorded.clone(),
                })
            },
            Arc::new(LocalSessionManager::default()),
            config,
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind");
        let address = listener.local_addr().expect("address");
        let ct = CancellationToken::new();
        let stop = ct.clone();
        let task = tokio::spawn(async move {
            axum::serve(listener, axum::Router::new().nest_service("/mcp", service))
                .with_graceful_shutdown(stop.cancelled_owned())
                .await
                .expect("serve");
        });
        let receipt = crate::daemon_lock::scoped_daemon_pid_path(&app_home, &global);
        std::fs::create_dir_all(receipt.parent().expect("receipt parent")).expect("receipt dir");
        crate::utils::write_json_file_owner_only(&receipt, &json!({"pid":std::process::id(), "port":address.port(), "url":format!("http://{address}/mcp"), "global_db":global, "project_db":null, "version":env!("CARGO_PKG_VERSION"), "internal_proxy_token":token})).expect("fixture receipt");
        // An explicit daemon-wide Ops profile is not inherited by ordinary HTTP.
        // Even possession of its capability leaves generic CLI transport narrow.
        let daemon = crate::cli_client::DaemonInfo {
            url: format!("http://{address}/mcp"),
            global_db: Some(global.display().to_string()),
            project_db: None,
            version: Some(env!("CARGO_PKG_VERSION").into()),
            pid: Some(std::process::id() as i64),
            internal_proxy_token: Some(token.clone()),
        };
        for name in ["tachi_wiki", "list_memories"] {
            assert!(
                tachi_hub::tool_visible(name, server.active_tool_profile(), None),
                "the explicitly Ops local daemon owns this operator route"
            );
            let error =
                crate::cli_client::call_daemon_tool(&daemon, name, serde_json::Map::new(), None)
                    .await
                    .expect_err("ordinary generic transport must stay narrow");
            assert!(
                error.to_string().contains("tool not found"),
                "ordinary request denied at visibility boundary: {error}"
            );
        }
        use tachi_bootstrap::cli::Commands;
        let commands = [
            (
                "wiki write",
                "tachi_wiki",
                Commands::WikiWrite {
                    title: "operator regression".into(),
                    text: "operator CLI body".into(),
                    path: Some("/tests/operator-cli".into()),
                    topic: None,
                    summary: None,
                    keywords: vec![],
                    entities: vec![],
                    importance: None,
                    scope: Some("global".into()),
                    project: None,
                    domain: None,
                    force: true,
                },
            ),
            (
                "wiki search",
                "tachi_wiki",
                Commands::WikiSearch {
                    query: "operator regression".into(),
                    category: None,
                    top_k: 5,
                    project: None,
                },
            ),
            (
                "list",
                "list_memories",
                Commands::List {
                    path_prefix: "/tests/operator-cli".into(),
                    limit: 10,
                    include_archived: false,
                },
            ),
        ];
        let mut violations = Vec::new();
        for (label, expected_tool, command) in commands {
            calls.lock().expect("calls").clear();
            let result = crate::bootstrap::cli_tool::run_cli_command(
                command,
                &global,
                None,
                &app_home,
                &memcore::MigrationAuthority::Deny,
            )
            .await;
            if let Err(error) = result {
                violations.push(format!("{label} command failed: {error}"));
            }
            let observed = calls.lock().expect("calls");
            if observed.len() != 1
                || observed[0].0 != expected_tool
                || observed[0].1["isError"] == true
                || observed[0].1.get("rpc_error").is_some()
            {
                violations.push(format!("{label} did not complete on daemon (read fallback cannot count as success): {observed:?}"));
            }
        }
        ct.cancel();
        task.await.expect("daemon stopped");
        assert!(
            violations.is_empty(),
            "operator CLI daemon boundary failed:\n{}",
            violations.join("\n")
        );
    });
}
