mod rmcp_compat;

use super::*;
use crate::test_support::EnvRestore;
use tokio_util::sync::CancellationToken;

fn daemon(global: Option<&Path>, project: Option<&Path>) -> crate::cli_client::DaemonInfo {
    crate::cli_client::DaemonInfo {
        url: "http://127.0.0.1:6919/mcp".to_string(),
        global_db: global.map(|path| path.display().to_string()),
        project_db: project.map(|path| path.display().to_string()),
        version: Some(env!("CARGO_PKG_VERSION").to_string()),
        pid: Some(std::process::id() as i64),
        internal_proxy_token: None,
    }
}

// #1096 leaf-2a: this file needs a caller-supplied (and sometimes reused
// across several calls) `home` path, unlike `crate::test_support`'s
// `with_tachi_home` which always mints its own fresh tempdir — so it keeps
// its own distinct signature, but now delegates env save/restore to the
// shared `EnvRestore` RAII guard instead of hand-rolled pre/post statements
// (which, notably, were not panic-safe: a panic inside `f` used to skip the
// restore calls below entirely and leak the override into later tests).
fn with_tachi_home<T>(home: &Path, f: impl FnOnce() -> T) -> T {
    let _guard = crate::utils::global_test_lock()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let _tachi_home = EnvRestore::set_path("TACHI_HOME", home);
    let _sigil_home = EnvRestore::remove("SIGIL_HOME");
    let _app_home = EnvRestore::remove("TACHI_APP_HOME");
    f()
}

fn test_runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("test runtime")
}

async fn spawn_test_http_daemon(
    server: crate::MemoryServer,
    global_db_path: &Path,
) -> (
    crate::cli_client::DaemonInfo,
    CancellationToken,
    tokio::task::JoinHandle<()>,
) {
    spawn_test_http_daemon_with_client_observer(server, global_db_path, None).await
}

type ClientHeaderObservations = std::sync::Arc<std::sync::Mutex<Vec<Option<String>>>>;

async fn spawn_test_http_daemon_with_client_observer(
    server: crate::MemoryServer,
    global_db_path: &Path,
    client_observer: Option<ClientHeaderObservations>,
) -> (
    crate::cli_client::DaemonInfo,
    CancellationToken,
    tokio::task::JoinHandle<()>,
) {
    spawn_test_http_daemon_with_observers(server, global_db_path, client_observer, None).await
}

type ToolResponseObservations =
    std::sync::Arc<std::sync::Mutex<Vec<(String, Option<String>, String)>>>;

async fn spawn_test_http_daemon_with_observers(
    server: crate::MemoryServer,
    global_db_path: &Path,
    client_observer: Option<ClientHeaderObservations>,
    response_observer: Option<ToolResponseObservations>,
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
    let internal_proxy_token = uuid::Uuid::new_v4().simple().to_string();
    server.set_daemon_proxy_token(internal_proxy_token.clone());

    let mut http_config = StreamableHttpServerConfig::default();
    http_config.legacy_session_mode = true;
    http_config.stateless_protocol_metadata_required = true;
    http_config.cancellation_token = ct.child_token();

    let service = StreamableHttpService::new(
        move || Ok(server.clone_for_mcp_session()),
        std::sync::Arc::new(LocalSessionManager::default()),
        http_config,
    );
    let router =
        axum::Router::new()
            .nest_service("/mcp", service)
            .layer(axum::middleware::from_fn(
                move |request: axum::extract::Request, next: axum::middleware::Next| {
                    let observer = client_observer.clone();
                    let response_observer = response_observer.clone();
                    async move {
                        if request.method() == axum::http::Method::POST {
                            if let Some(observer) = observer {
                                observer.lock().expect("client header observations").push(
                                    request
                                        .headers()
                                        .get(crate::session_identity::HEADER_CLIENT)
                                        .map(|value| {
                                            value.to_str().expect("client header UTF-8").to_string()
                                        }),
                                );
                            }
                        }
                        if let Some(observer) = response_observer {
                            let profile = request
                                .headers()
                                .get(crate::session_identity::HEADER_PROFILE)
                                .map(|value| value.to_str().unwrap().to_string());
                            let (parts, body) = request.into_parts();
                            let bytes = axum::body::to_bytes(body, 1024 * 1024)
                                .await
                                .expect("bounded request");
                            let tool = serde_json::from_slice::<serde_json::Value>(&bytes)
                                .ok()
                                .and_then(|value| {
                                    value["params"]["name"].as_str().map(str::to_owned)
                                });
                            let request = axum::extract::Request::from_parts(
                                parts,
                                axum::body::Body::from(bytes),
                            );
                            let response = next.run(request).await;
                            if let Some(tool) = tool {
                                let (parts, body) = response.into_parts();
                                let bytes = axum::body::to_bytes(body, 1024 * 1024)
                                    .await
                                    .expect("bounded response");
                                observer.lock().unwrap().push((
                                    tool,
                                    profile,
                                    String::from_utf8(bytes.to_vec()).unwrap(),
                                ));
                                return axum::response::Response::from_parts(
                                    parts,
                                    axum::body::Body::from(bytes),
                                );
                            }
                            response
                        } else {
                            next.run(request).await
                        }
                    }
                },
            ));
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
            internal_proxy_token: Some(internal_proxy_token),
        },
        ct,
        handle,
    )
}

async fn call_tool_via_stdio_proxy(
    proxy: StdioProxyServer,
    tool_name: &str,
    arguments: serde_json::Map<String, serde_json::Value>,
) -> Result<rmcp::model::CallToolResult, rmcp::ErrorData> {
    let mut params = rmcp::model::CallToolRequestParams::new(tool_name.to_string());
    if !arguments.is_empty() {
        params = params.with_arguments(arguments);
    }
    let request =
        rmcp::model::ClientRequest::CallToolRequest(rmcp::model::CallToolRequest::new(params));
    let (transport, mut receiver) =
        rmcp::transport::OneshotTransport::<rmcp::service::RoleServer>::new(
            rmcp::model::ClientJsonRpcMessage::request(request, rmcp::model::RequestId::Number(1)),
        );
    let service = rmcp::service::serve_directly(proxy, transport, None);

    // Headroom note (issue #987): this is a safety-net deadline on an
    // in-process oneshot transport, not a real network wait — it normally
    // resolves in milliseconds. 5s was observed tripping under wide-suite
    // `cargo test` parallelism (CPU/scheduler contention from hundreds of
    // concurrently-running unrelated tests), not from any real slowness in
    // the proxy itself. 30s gives ample headroom without slowing down a
    // healthy run (the await still returns as soon as the response arrives).
    let message = tokio::time::timeout(std::time::Duration::from_secs(30), receiver.recv())
        .await
        .expect("proxied tool call timed out")
        .expect("proxied tool call should yield one response");

    let quit_reason = service.waiting().await.expect("wait for proxy service");
    assert!(
        matches!(quit_reason, rmcp::service::QuitReason::Closed),
        "proxy oneshot service should close cleanly after one tool call"
    );

    match message {
        rmcp::model::ServerJsonRpcMessage::Response(response) => match response.result {
            rmcp::model::ServerResult::CallToolResult(result) => Ok(result),
            other => panic!("expected CallToolResult, got {other:?}"),
        },
        rmcp::model::ServerJsonRpcMessage::Error(error) => Err(error.error),
        other => panic!("expected tool response or error, got {other:?}"),
    }
}

async fn list_tools_via_stdio_proxy(
    proxy: StdioProxyServer,
) -> Result<rmcp::model::ListToolsResult, rmcp::ErrorData> {
    let request =
        rmcp::model::ClientRequest::ListToolsRequest(rmcp::model::ListToolsRequest::default());
    let (transport, mut receiver) =
        rmcp::transport::OneshotTransport::<rmcp::service::RoleServer>::new(
            rmcp::model::ClientJsonRpcMessage::request(request, rmcp::model::RequestId::Number(1)),
        );
    let service = rmcp::service::serve_directly(proxy, transport, None);
    let message = tokio::time::timeout(std::time::Duration::from_secs(30), receiver.recv())
        .await
        .expect("proxied list timed out")
        .expect("proxied list should yield one response");
    let quit_reason = service.waiting().await.expect("wait for proxy service");
    assert!(matches!(quit_reason, rmcp::service::QuitReason::Closed));
    match message {
        rmcp::model::ServerJsonRpcMessage::Response(response) => match response.result {
            rmcp::model::ServerResult::ListToolsResult(result) => Ok(result),
            other => panic!("expected ListToolsResult, got {other:?}"),
        },
        rmcp::model::ServerJsonRpcMessage::Error(error) => Err(error.error),
        other => panic!("expected list response or error, got {other:?}"),
    }
}

fn memory_text_count(db_path: &Path, text: &str) -> i64 {
    rusqlite::Connection::open(db_path)
        .expect("open db")
        .query_row(
            "SELECT COUNT(*) FROM memories WHERE text = ?1",
            [text],
            |row| row.get(0),
        )
        .expect("count memories")
}

fn memory_access_count(db_path: &Path, id: &str) -> i64 {
    rusqlite::Connection::open(db_path)
        .expect("open db")
        .query_row(
            "SELECT access_count FROM memories WHERE id = ?1",
            [id],
            |row| row.get(0),
        )
        .expect("read access count")
}

fn memory_id_count(db_path: &Path, id: &str) -> i64 {
    rusqlite::Connection::open(db_path)
        .expect("open db")
        .query_row("SELECT COUNT(*) FROM memories WHERE id = ?1", [id], |row| {
            row.get(0)
        })
        .expect("count memory id")
}

fn memory_archived_value(db_path: &Path, id: &str) -> i64 {
    rusqlite::Connection::open(db_path)
        .expect("open db")
        .query_row("SELECT archived FROM memories WHERE id = ?1", [id], |row| {
            row.get(0)
        })
        .expect("archived value")
}

fn first_text(result: &rmcp::model::CallToolResult) -> String {
    result
        .content
        .iter()
        .find_map(|content| match content {
            rmcp::model::ContentBlock::Text(text) => Some(text.text.clone()),
            _ => None,
        })
        .expect("text tool result")
}

fn first_text_json(result: &rmcp::model::CallToolResult) -> serde_json::Value {
    let text = first_text(result);
    serde_json::from_str(&text)
        .unwrap_or_else(|err| panic!("tool result json: {err}; text={text:?}"))
}

fn assert_search_section_rows_are_objects(parsed: &serde_json::Value) {
    let sections = parsed["sections"].as_array().unwrap_or_else(|| {
        panic!("search JSON should contain sections array: {parsed:#}");
    });
    for section in sections {
        let section_name = section["name"].as_str().unwrap_or("<unnamed>");
        let rows = section["rows"].as_array().unwrap_or_else(|| {
            panic!("section {section_name} rows should be an array: {section:#}");
        });
        for row in rows {
            assert!(
                row.is_object(),
                "section {section_name} contains non-object row: row={row:#} parsed={parsed:#}"
            );
        }
    }
}

fn assert_tool_ok(result: &rmcp::model::CallToolResult) {
    assert!(
        !result.is_error.unwrap_or(false),
        "tool call should not be an MCP error: {result:?}"
    );
}

fn seed_project_db(tachi_home: &Path, project_db_path: &Path) {
    let _seed = crate::MemoryServer::new(
        tachi_home.join(format!("seed-{}.db", uuid::Uuid::new_v4())),
        Some(project_db_path.to_path_buf()),
    )
    .expect("seed project db schema");
}

fn seed_identity_db(tachi_home: &Path, project_name: &str) {
    let db_path = tachi_home
        .join("projects")
        .join(project_name)
        .join("memory.db");
    std::fs::create_dir_all(db_path.parent().expect("identity db parent"))
        .expect("identity db parent");
    std::fs::write(db_path, project_name.as_bytes()).expect("identity db fixture");
}

#[test]
fn stdio_proxy_profile_is_forwarded_once_and_denies_attachment_before_handler() {
    let _guard = crate::utils::global_test_lock()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let temp = tempfile::tempdir().expect("tempdir");
    let tachi_home = temp.path().join("home");
    let global = tachi_home.join("global/memory.db");
    std::fs::create_dir_all(global.parent().expect("global parent")).expect("global parent");

    let rt = test_runtime();
    let (ct, daemon_task) = rt.block_on(async {
        let server = crate::MemoryServer::new(global.clone(), None).expect("daemon server");
        server.set_tool_profile(Some(tachi_hub::ToolProfile::coordinate()));
        let (daemon, ct, daemon_task) = spawn_test_http_daemon(server, &global).await;
        let delegate_proxy = StdioProxyServer {
            adapter_started_at: chrono::Utc::now(),
            tool_profile: Some(tachi_hub::ToolProfile::delegate()),
            resolved_agent_identity: Default::default(),
            rate_limit_session: ProxyRateLimitSession::mint(),
            daemon: std::sync::Arc::new(std::sync::RwLock::new(daemon.clone())),
            app_home: tachi_home.clone(),
            global_db_path: global.clone(),
            project_db_path: None,
            client_project: None,
        };
        let listed = list_tools_via_stdio_proxy(delegate_proxy.clone()).await;
        assert!(
            listed.is_ok(),
            "profile-bound tools/list should succeed: {listed:?}"
        );
        assert!(
            !listed
                .as_ref()
                .unwrap()
                .tools
                .iter()
                .any(|tool| tool.name.as_ref() == "tachi_agent_eval"),
            "delegate profile must remain bound during tools/list"
        );

        let delegate_get = call_tool_via_stdio_proxy(
            delegate_proxy.clone(),
            "tachi_agent_eval",
            serde_json::Map::from_iter([(
                "action".to_string(),
                serde_json::json!("get_attachment"),
            )]),
        )
        .await
        .expect("delegate profile refusal should be an MCP error result");
        assert!(delegate_get.is_error.unwrap_or(false));
        assert!(
            first_text(&delegate_get).contains("tool not found"),
            "delegate must not reach the attachment handler: {delegate_get:?}"
        );

        // Changing launch configuration after construction is not a profile
        // redeclaration. The connection keeps the parsed delegate profile;
        // it must not widen when a later call observes a broader environment.
        let _profile_env = EnvRestore::set("TACHI_PROFILE", "admin");
        let listed_after_redeclare = list_tools_via_stdio_proxy(delegate_proxy.clone()).await;
        assert!(listed_after_redeclare.is_ok());
        assert!(
            !listed_after_redeclare
                .as_ref()
                .unwrap()
                .tools
                .iter()
                .any(|tool| tool.name.as_ref() == "tachi_agent_eval"),
            "mid-session profile widening must be ignored"
        );

        for action in ["attach_session", "get_attachment"] {
            let proxy = StdioProxyServer {
                adapter_started_at: chrono::Utc::now(),
                tool_profile: Some(tachi_hub::ToolProfile::observe()),
                resolved_agent_identity: Default::default(),
                rate_limit_session: ProxyRateLimitSession::mint(),
                daemon: std::sync::Arc::new(std::sync::RwLock::new(daemon.clone())),
                app_home: tachi_home.clone(),
                global_db_path: global.clone(),
                project_db_path: None,
                client_project: None,
            };
            let result = call_tool_via_stdio_proxy(
                proxy,
                "tachi_agent_eval",
                serde_json::Map::from_iter([("action".to_string(), serde_json::json!(action))]),
            )
            .await
            .expect("profile-denied call should return an MCP error result");
            assert!(result.is_error.unwrap_or(false));
            assert!(
                first_text(&result).contains("tool not found"),
                "hidden route must be refused before proxy dispatch: {result:?}"
            );
        }

        let expected_five = vec![
            "tachi_a2a".to_string(),
            "tachi_gh".to_string(),
            "tachi_memory".to_string(),
            "tachi_staff".to_string(),
            "tachi_task".to_string(),
        ];
        let expected_standard = vec![
            "tachi_a2a".to_string(),
            "tachi_agent_eval".to_string(),
            "tachi_gh".to_string(),
            "tachi_memory".to_string(),
            "tachi_staff".to_string(),
            "tachi_task".to_string(),
        ];
        for raw_profile in [
            "standard",
            "lead",
            "delegate",
            "worker",
            "observe",
            "read",
            "reader",
            "remember",
            "write",
            "writer",
            "agent",
            "coordinate",
            "observe+remember",
            "remember+observe",
            "reader+writer",
            "coordinate+observe",
            "observe+coordinate",
            "delegate+operate",
            "standard+delegate",
            "delegate+standard",
        ] {
            let profile = tachi_hub::parse_tool_profile(raw_profile)
                .unwrap_or_else(|| panic!("profile {raw_profile} should parse"));
            let proxy = StdioProxyServer {
                adapter_started_at: chrono::Utc::now(),
                tool_profile: Some(profile),
                resolved_agent_identity: Default::default(),
                rate_limit_session: ProxyRateLimitSession::mint(),
                daemon: std::sync::Arc::new(std::sync::RwLock::new(daemon.clone())),
                app_home: tachi_home.clone(),
                global_db_path: global.clone(),
                project_db_path: None,
                client_project: None,
            };
            let mut names = list_tools_via_stdio_proxy(proxy.clone())
                .await
                .unwrap_or_else(|error| panic!("{raw_profile} tools/list failed: {error}"))
                .tools
                .into_iter()
                .map(|tool| tool.name.into_owned())
                .collect::<Vec<_>>();
            names.sort();
            let is_standard = profile.as_str() == "standard";
            assert_eq!(
                &names,
                if is_standard {
                    &expected_standard
                } else {
                    &expected_five
                },
                "stdio profile {raw_profile}"
            );

            if is_standard {
                for action in ["attach_session", "aggregate_live", "future_action"] {
                    let denied = call_tool_via_stdio_proxy(
                        proxy.clone(),
                        "tachi_agent_eval",
                        serde_json::Map::from_iter([(
                            "action".to_string(),
                            serde_json::json!(action),
                        )]),
                    )
                    .await
                    .expect("standard eval action refusal should be an MCP result");
                    assert_eq!(denied.is_error, Some(true), "{raw_profile}:{action}");
                    assert!(
                        first_text(&denied).contains("not allowed"),
                        "{raw_profile}:{action}: {denied:?}"
                    );
                }
            }

            for hidden in ["runtime_info", "tachi_briefing"] {
                let result =
                    call_tool_via_stdio_proxy(proxy.clone(), hidden, serde_json::Map::new())
                        .await
                        .unwrap_or_else(|error| {
                            panic!("{raw_profile} hidden call {hidden} failed: {error}")
                        });
                assert_eq!(result.is_error, Some(true), "{raw_profile}:{hidden}");
                assert!(
                    first_text(&result).contains("tool not found"),
                    "{raw_profile}:{hidden}: {result:?}"
                );
            }
        }

        let default_proxy = StdioProxyServer {
            adapter_started_at: chrono::Utc::now(),
            tool_profile: None,
            resolved_agent_identity: Default::default(),
            rate_limit_session: ProxyRateLimitSession::mint(),
            daemon: std::sync::Arc::new(std::sync::RwLock::new(daemon)),
            app_home: tachi_home,
            global_db_path: global,
            project_db_path: None,
            client_project: None,
        };
        let denied_runtime = call_tool_via_stdio_proxy(
            default_proxy.clone(),
            "runtime_info",
            serde_json::Map::new(),
        )
        .await
        .expect("default proxy diagnostic refusal should be an MCP result");
        assert_eq!(denied_runtime.is_error, Some(true));
        assert!(
            first_text(&denied_runtime).contains("tool not found"),
            "missing proxy profile must not call diagnostics: {denied_runtime:?}"
        );

        let mut default_names = list_tools_via_stdio_proxy(default_proxy)
            .await
            .expect("default proxy tools/list")
            .tools
            .into_iter()
            .map(|tool| tool.name.into_owned())
            .collect::<Vec<_>>();
        default_names.sort();
        assert_eq!(
            default_names, expected_standard,
            "missing proxy profile must not inherit the broader daemon profile"
        );
        (ct, daemon_task)
    });
    rt.block_on(async {
        ct.cancel();
        daemon_task.await.expect("daemon task");
    });
}

#[test]
fn stdio_process_selected_ops_proxy_lists_and_calls_through_production_session_clone() {
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

    let rt = test_runtime();
    let (ct, daemon_task) = rt.block_on(async {
        let server = crate::MemoryServer::new(global.clone(), None).expect("daemon server");
        let (daemon, ct, daemon_task) = spawn_test_http_daemon(server, &global).await;
        let proxy = StdioProxyServer {
            adapter_started_at: chrono::Utc::now(),
            tool_profile: Some(tachi_hub::ToolProfile::operate()),
            resolved_agent_identity: Default::default(),
            rate_limit_session: ProxyRateLimitSession::mint(),
            daemon: std::sync::Arc::new(std::sync::RwLock::new(daemon)),
            app_home: tachi_home,
            global_db_path: global,
            project_db_path: None,
            client_project: None,
        };

        let listed = list_tools_via_stdio_proxy(proxy.clone())
            .await
            .expect("process-selected Ops tools/list must traverse HTTP proxy");
        let names = listed
            .tools
            .iter()
            .map(|tool| tool.name.as_ref())
            .collect::<std::collections::BTreeSet<_>>();
        assert_eq!(names.len(), 39, "Ops must retain its frozen broad surface");
        assert!(names.contains("tachi_status"), "Ops list: {names:?}");
        assert!(names.contains("runtime_info"), "Ops list: {names:?}");

        let status = call_tool_via_stdio_proxy(proxy, "tachi_status", serde_json::Map::new())
            .await
            .expect("process-selected Ops call must traverse HTTP proxy");
        assert_ne!(status.is_error, Some(true), "Ops status call: {status:?}");
        (ct, daemon_task)
    });
    ct.cancel();
    rt.block_on(daemon_task).expect("daemon task");
}

#[cfg(unix)]
#[test]
fn vault_cli_operator_actions_select_ops_through_real_daemon_session() {
    let _guard = crate::utils::global_test_lock()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let temp = tempfile::tempdir().expect("tempdir");
    let tachi_home = temp.path().join("home");
    let global = tachi_home.join("global/memory.db");
    let password_file = temp.path().join("vault-password");
    let password = "correct horse battery staple";
    std::fs::create_dir_all(global.parent().expect("global parent")).expect("global parent");
    crate::utils::write_owner_only_file_atomic(&password_file, password.as_bytes())
        .expect("write owner-only password fixture");
    let key = crate::bootstrap::vault_cli::vault_init_with_password(&global, password.to_string())
        .expect("initialize vault fixture");
    crate::bootstrap::vault_cli::vault_upsert_secret_with_key(
        &global,
        &key,
        "FIXTURE_API_KEY",
        memcore::SECRET_TYPE_API_KEY,
        "synthetic operator CLI fixture",
        "synthetic-value-never-a-real-credential".into(),
    )
    .expect("seed synthetic API key");
    let _tachi_home = EnvRestore::set_path("TACHI_HOME", &tachi_home);
    let _sigil_home = EnvRestore::remove("SIGIL_HOME");
    let _app_home = EnvRestore::remove("TACHI_APP_HOME");

    let rt = test_runtime();
    let (ct, daemon_task) = rt.block_on(async {
        let server = crate::MemoryServer::new(global.clone(), None).expect("daemon server");
        let (daemon, ct, daemon_task) = spawn_test_http_daemon(server, &global).await;
        let port = daemon
            .url
            .strip_prefix("http://127.0.0.1:")
            .and_then(|rest| rest.split('/').next())
            .and_then(|value| value.parse::<u16>().ok())
            .expect("test daemon URL port");
        let pid_path = crate::daemon_lock::scoped_daemon_pid_path(&tachi_home, &global);
        crate::utils::write_json_file_owner_only(
            &pid_path,
            &serde_json::json!({
                "pid": std::process::id(),
                "port": port,
                "url": daemon.url,
                "global_db": global.display().to_string(),
                "version": env!("CARGO_PKG_VERSION"),
                "internal_proxy_token": daemon.internal_proxy_token,
            }),
        )
        .expect("write daemon discovery receipt");

        for action in [
            tachi_bootstrap::cli::VaultAction::Status,
            tachi_bootstrap::cli::VaultAction::Lock,
            tachi_bootstrap::cli::VaultAction::Unlock {
                stdin_password: false,
                keychain: false,
                password_file: Some(password_file),
                insecure_password_file: false,
            },
        ] {
            crate::bootstrap::vault_cli::run_vault_command(&global, &tachi_home, action)
                .await
                .expect("explicit Vault CLI action must select authorized Ops profile");
        }
        let connection = rusqlite::Connection::open(&global).expect("fixture DB");
        let access_count = || {
            connection
                .query_row(
                    "SELECT access_count FROM vault_entries WHERE name='FIXTURE_API_KEY'",
                    [],
                    |row| row.get::<_, i64>(0),
                )
                .expect("lease access count")
        };
        // Unlock may warm provider caches; measure the lease independently of it.
        let before_lease = access_count();
        let lease_args =
            serde_json::Map::from_iter([("name".into(), serde_json::json!("FIXTURE_API_KEY"))]);
        for profile in [None, Some(tachi_hub::ToolProfile::operate())] {
            let denied = crate::cli_client::call_daemon_tool_with_profile(
                &daemon,
                "vault_lease_api_key",
                lease_args.clone(),
                None,
                profile,
            )
            .await
            .expect_err("capability alone or Ops must not select Admin");
            assert!(denied.to_string().contains("tool not found"), "{denied}");
        }
        let (_, _, denied) = http_mcp_initialize(
            &daemon.url,
            http_headers(&[(crate::session_identity::HEADER_PROFILE, "admin")]),
            None,
        )
        .await;
        assert_eq!(
            denied["error"]["code"], -32602,
            "Admin requires daemon capability: {denied}"
        );
        crate::bootstrap::vault_cli::run_vault_command(
            &global,
            &tachi_home,
            tachi_bootstrap::cli::VaultAction::List {
                stdin_password: false,
                keychain: false,
                password_file: None,
                insecure_password_file: false,
            },
        )
        .await
        .expect("metadata list remains available through existing read fallback");
        assert_eq!(
            access_count(),
            before_lease,
            "denied requests and metadata-only List must not read the synthetic key"
        );
        crate::bootstrap::vault_cli::run_vault_command(
            &global,
            &tachi_home,
            tachi_bootstrap::cli::VaultAction::Lease {
                name: "FIXTURE_API_KEY".into(),
                env_name: None,
                stdin_password: false,
                keychain: false,
                password_file: None,
                insecure_password_file: false,
                json: true,
            },
        )
        .await
        .expect("explicit operator lease must reach authorized daemon");
        assert_eq!(
            access_count(),
            before_lease + 1,
            "exactly the explicitly authorized lease must read the synthetic key once"
        );
        (ct, daemon_task)
    });
    ct.cancel();
    rt.block_on(daemon_task).expect("daemon task");
}

#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn vault_cli_list_uses_authorized_daemon_health_without_metadata_fallback() {
    let _guard = crate::utils::global_test_lock()
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    let temp = tempfile::tempdir().expect("temp");
    let home = temp.path().join("home");
    let global = home.join("global/memory.db");
    std::fs::create_dir_all(global.parent().unwrap()).unwrap();
    let _home = EnvRestore::set_path("TACHI_HOME", &home);
    let _sigil = EnvRestore::remove("SIGIL_HOME");
    let _app = EnvRestore::remove("TACHI_APP_HOME");
    let _alias = EnvRestore::set("EXTRACT_API_KEY", "vault:DEEPSEEK_API_KEY");
    let server = crate::MemoryServer::new(global.clone(), None).expect("server");
    crate::vault_ops::handle_vault_init(
        &server,
        crate::vault_ops::VaultInitParams {
            password: "synthetic-health-board-password".into(),
        },
    )
    .await
    .unwrap();
    crate::vault_ops::handle_vault_set(
        &server,
        crate::vault_ops::VaultSetParams {
            name: "DEEPSEEK_API_KEY".into(),
            value: "synthetic-health-board-secret".into(),
            secret_type: "api_key".into(),
            description: "health fixture".into(),
            agent_id: None,
            allowed_agents: None,
            enable_rotation: false,
            rotation_strategy: None,
            rebind: false,
        },
    )
    .await
    .unwrap();
    crate::provider_config::materialize_for_server(&server).expect("materialize");
    let generation = server.llm.provider_health_board_snapshot().2.unwrap();
    let health = memcore::vault::health::record_key_outcome_for_generation(
        None,
        "DEEPSEEK_API_KEY",
        "DEEPSEEK_API_KEY",
        memcore::vault::health::TypedOutcome::Exhausted,
        memcore::vault::health::EvidenceKind::Probed,
        None,
        chrono::Utc::now(),
        Some(generation),
    )
    .health;
    server
        .with_global_store(|store| {
            store
                .vault_upsert_key_health(&health)
                .map_err(|error| error.to_string())
        })
        .unwrap();
    let observations = ToolResponseObservations::default();
    let (daemon, cancel, task) =
        spawn_test_http_daemon_with_observers(server, &global, None, Some(observations.clone()))
            .await;
    let port = reqwest::Url::parse(&daemon.url).unwrap().port().unwrap();
    let pid_path = crate::daemon_lock::scoped_daemon_pid_path(&home, &global);
    crate::utils::write_json_file_owner_only(&pid_path, &serde_json::json!({
        "pid": std::process::id(), "port": port, "url": daemon.url, "global_db": global,
        "version": env!("CARGO_PKG_VERSION"), "internal_proxy_token": daemon.internal_proxy_token,
    })).unwrap();
    for profile in [None, Some(tachi_hub::ToolProfile::operate())] {
        let denied = crate::cli_client::call_daemon_tool_with_profile(
            &daemon,
            "vault_list",
            serde_json::Map::new(),
            None,
            profile,
        )
        .await
        .expect_err("only explicit Admin may list");
        assert!(denied.to_string().contains("tool not found"));
    }
    observations.lock().unwrap().clear();
    let result = crate::bootstrap::vault_cli::run_vault_command(
        &global,
        &home,
        tachi_bootstrap::cli::VaultAction::List {
            stdin_password: false,
            keychain: false,
            password_file: None,
            insecure_password_file: false,
        },
    )
    .await;
    // A matching daemon that rejects dispatch must stay an error. Falling
    // back here would turn its denied observation into a successful local list.
    crate::utils::write_json_file_owner_only(&pid_path, &serde_json::json!({
        "pid": std::process::id(), "port": port, "url": daemon.url, "global_db": global,
        "version": env!("CARGO_PKG_VERSION"), "internal_proxy_token": uuid::Uuid::new_v4().simple().to_string(),
    })).unwrap();
    let denied = crate::bootstrap::vault_cli::run_vault_command(
        &global,
        &home,
        tachi_bootstrap::cli::VaultAction::List {
            stdin_password: false,
            keychain: false,
            password_file: None,
            insecure_password_file: false,
        },
    )
    .await;
    cancel.cancel();
    task.await.expect("daemon terminal");
    result.expect("actual operator List");
    assert!(
        denied.is_err(),
        "matching daemon denial must not become metadata fallback success"
    );
    let rows = observations.lock().unwrap();
    assert_eq!(rows.len(), 1, "one List call");
    assert_eq!(rows[0].0, "vault_list");
    assert_eq!(
        rows[0].1.as_deref(),
        Some("admin"),
        "List must reach health handler, response={}",
        rows[0].2
    );
    assert!(
        rows[0].2.contains("402"),
        "real daemon response must carry observed health"
    );
    assert!(!rows[0].2.contains("synthetic-health-board-secret"));
}

fn write_repo_project_manifest(tachi_home: &Path, db_paths: &[&Path]) {
    let entries = db_paths
        .iter()
        .map(|db_path| {
            serde_json::json!({
                "path": db_path.to_string_lossy(),
                "role": "project",
                "owner": "project:test",
                "schema_kind": "tachi",
                "vec_enabled": false,
                "allow_write": true,
                "last_doctor_at": "1970-01-01T00:00:00Z",
                "last_classification": "healthy",
                "scope_hint": "test"
            })
        })
        .collect::<Vec<_>>();
    std::fs::write(
        tachi_home.join("manifest.json"),
        serde_json::to_vec_pretty(&serde_json::json!({
            "schema_version": 1,
            "generated_at": "1970-01-01T00:00:00Z",
            "_comment": "endpoint alias test",
            "dbs": entries
        }))
        .expect("serialize manifest"),
    )
    .expect("write manifest");
}

#[test]
fn ensure_stdio_proxy_accepts_global_only_daemon_for_project_client() {
    let _guard = crate::utils::global_test_lock()
        .lock()
        .unwrap_or_else(|e| e.into_inner());

    let temp = tempfile::tempdir().expect("tempdir");
    let tachi_home = temp.path().join("home");
    let global = tachi_home.join("global/memory.db");
    let project = tachi_home.join("projects/Sigil-test/memory.db");
    std::fs::create_dir_all(global.parent().expect("global parent")).expect("global parent");
    std::fs::create_dir_all(project.parent().expect("project parent")).expect("project parent");
    std::fs::write(&project, b"").expect("project db placeholder");
    let _tachi_home = EnvRestore::set_path("TACHI_HOME", &tachi_home);
    let _sigil_home = EnvRestore::remove("SIGIL_HOME");
    let _app_home = EnvRestore::remove("TACHI_APP_HOME");
    let _disable_proxy = EnvRestore::remove("TACHI_DISABLE_STDIO_PROXY");
    let _disable_auto = EnvRestore::set("TACHI_DISABLE_AUTO_DAEMON", "1");

    let rt = test_runtime();
    let listener = rt
        .block_on(tokio::net::TcpListener::bind("127.0.0.1:0"))
        .expect("listener");
    let port = listener.local_addr().expect("local addr").port();
    let pid_path = crate::daemon_lock::scoped_daemon_pid_path(&tachi_home, &global);
    std::fs::write(
        &pid_path,
        serde_json::json!({
            "pid": std::process::id(),
            "port": port,
            "url": format!("http://127.0.0.1:{port}/mcp"),
            "global_db": global.display().to_string(),
            "project_db": null,
            "version": env!("CARGO_PKG_VERSION"),
        })
        .to_string(),
    )
    .expect("pid file");

    let info = rt
        .block_on(ensure_stdio_proxy_daemon(
            &tachi_home,
            &global,
            Some(&project),
            Some("Sigil-test"),
        ))
        .expect("global-only daemon should accept bound project client");
    assert_eq!(info.project_db, None);
    assert_eq!(info.url, format!("http://127.0.0.1:{port}/mcp"));

    drop(listener);
}

#[test]
fn stdio_proxy_call_writes_bound_project_via_global_only_daemon() {
    let _guard = crate::utils::global_test_lock()
        .lock()
        .unwrap_or_else(|e| e.into_inner());

    let temp = tempfile::tempdir().expect("tempdir");
    let tachi_home = temp.path().join("home");
    let global = tachi_home.join("global/memory.db");
    let project_name = "Sigil-proxy-e2e";
    let project = tachi_home
        .join("projects")
        .join(project_name)
        .join("memory.db");
    std::fs::create_dir_all(global.parent().expect("global parent")).expect("global parent");
    let _tachi_home = EnvRestore::set_path("TACHI_HOME", &tachi_home);
    let _sigil_home = EnvRestore::remove("SIGIL_HOME");
    let _app_home = EnvRestore::remove("TACHI_APP_HOME");
    seed_project_db(&tachi_home, &project);

    let rt = test_runtime();
    let (ct, daemon_task) = rt.block_on(async {
        let server = crate::MemoryServer::new(global.clone(), None).expect("daemon server");
        let (daemon, ct, daemon_task) = spawn_test_http_daemon(server, &global).await;
        let proxy = StdioProxyServer {
            adapter_started_at: chrono::Utc::now(),
            tool_profile: None,
            daemon: std::sync::Arc::new(std::sync::RwLock::new(daemon)),
            app_home: tachi_home.clone(),
            global_db_path: global.clone(),
            project_db_path: Some(project.clone()),
            client_project: Some(project_name.to_string()),
            resolved_agent_identity: Default::default(),
            rate_limit_session: ProxyRateLimitSession::mint(),
        };

        let saved_text = "stdio proxy e2e writes only the bound project db";
        let result = call_tool_via_stdio_proxy(
            proxy,
            "tachi_memory",
            serde_json::Map::from_iter([
                ("action".to_string(), serde_json::json!("save")),
                ("text".to_string(), serde_json::json!(saved_text)),
                ("summary".to_string(), serde_json::json!("stdio proxy e2e")),
                (
                    "path".to_string(),
                    serde_json::json!("/tests/stdio-proxy-e2e"),
                ),
                ("category".to_string(), serde_json::json!("fact")),
                ("scope".to_string(), serde_json::json!("project")),
                ("force".to_string(), serde_json::json!(true)),
            ]),
        )
        .await
        .expect("proxied save_memory should succeed");

        assert_tool_ok(&result);
        (ct, daemon_task)
    });

    let saved_text = "stdio proxy e2e writes only the bound project db";
    assert_eq!(memory_text_count(&project, saved_text), 1);
    assert_eq!(memory_text_count(&global, saved_text), 0);

    ct.cancel();
    rt.block_on(daemon_task).expect("daemon task");
}

#[test]
fn stdio_proxy_same_db_alias_write_normalizes_to_bound_identity() {
    let _guard = crate::utils::global_test_lock()
        .lock()
        .unwrap_or_else(|e| e.into_inner());

    let temp = tempfile::tempdir().expect("tempdir");
    let tachi_home = temp.path().join("home");
    let global = tachi_home.join("global/memory.db");
    let repo = temp.path().join("repos/Sigil");
    let project = repo.join(".tachi/memory.db");
    std::fs::create_dir_all(global.parent().expect("global parent")).expect("global parent");
    let _tachi_home = EnvRestore::set_path("TACHI_HOME", &tachi_home);
    let _sigil_home = EnvRestore::remove("SIGIL_HOME");
    let _app_home = EnvRestore::remove("TACHI_APP_HOME");
    seed_project_db(&tachi_home, &project);
    write_repo_project_manifest(&tachi_home, &[&project]);
    let bound_name = crate::path_utils::plan_c_dir_name_from_root(&repo).expect("hashed name");
    let requested_alias =
        crate::path_utils::plan_c_legacy_dir_name_from_root(&repo).expect("legacy alias");

    let mapped = prepare_proxy_tool_call(
        rmcp::model::CallToolRequestParams::new("tachi_memory").with_arguments(
            serde_json::Map::from_iter([
                ("action".to_string(), serde_json::json!("save")),
                ("project".to_string(), serde_json::json!(&requested_alias)),
            ]),
        ),
        Some(bound_name.as_str()),
    )
    .expect("same-DB legacy alias should pass the stdio gate");
    assert_eq!(
        mapped.arguments.expect("normalized args")["project"],
        serde_json::json!(&bound_name),
        "effective identity must remain the hashed session binding"
    );

    let saved_text = "stdio same-DB alias writes once to the bound repo DB";
    let rt = test_runtime();
    let (ct, daemon_task) = rt.block_on(async {
        let server = crate::MemoryServer::new(global.clone(), None).expect("daemon server");
        let (daemon, ct, daemon_task) = spawn_test_http_daemon(server, &global).await;
        let proxy = StdioProxyServer {
            adapter_started_at: chrono::Utc::now(),
            tool_profile: None,
            daemon: std::sync::Arc::new(std::sync::RwLock::new(daemon)),
            app_home: tachi_home.clone(),
            global_db_path: global.clone(),
            project_db_path: Some(project.clone()),
            client_project: Some(bound_name.clone()),
            resolved_agent_identity: Default::default(),
            rate_limit_session: ProxyRateLimitSession::mint(),
        };
        let result = call_tool_via_stdio_proxy(
            proxy,
            "tachi_memory",
            serde_json::Map::from_iter([
                ("action".to_string(), serde_json::json!("save")),
                ("project".to_string(), serde_json::json!(&requested_alias)),
                ("text".to_string(), serde_json::json!(saved_text)),
                ("summary".to_string(), serde_json::json!("same DB alias")),
                (
                    "path".to_string(),
                    serde_json::json!("/tests/stdio-same-db-alias"),
                ),
                ("category".to_string(), serde_json::json!("fact")),
                ("scope".to_string(), serde_json::json!("project")),
                ("force".to_string(), serde_json::json!(true)),
            ]),
        )
        .await
        .expect("same-DB alias write through stdio");
        assert_tool_ok(&result);
        (ct, daemon_task)
    });

    assert_eq!(memory_text_count(&project, saved_text), 1);
    assert_eq!(memory_text_count(&global, saved_text), 0);
    assert!(
        !tachi_home.join("projects").join(&bound_name).exists(),
        "validation must not create the hashed Plan C alias"
    );
    assert!(
        !tachi_home.join("projects").join(&requested_alias).exists(),
        "validation must not create the legacy Plan C alias"
    );

    ct.cancel();
    rt.block_on(daemon_task).expect("daemon task");
}

#[test]
fn stdio_proxy_call_rejects_cross_project_override_before_daemon_write() {
    let _guard = crate::utils::global_test_lock()
        .lock()
        .unwrap_or_else(|e| e.into_inner());

    let temp = tempfile::tempdir().expect("tempdir");
    let tachi_home = temp.path().join("home");
    let global = tachi_home.join("global/memory.db");
    let bound_project_name = "Sigil-proxy-e2e";
    let other_project_name = "Quant-proxy-e2e";
    let bound_project = tachi_home
        .join("projects")
        .join(bound_project_name)
        .join("memory.db");
    let other_project = tachi_home
        .join("projects")
        .join(other_project_name)
        .join("memory.db");
    std::fs::create_dir_all(global.parent().expect("global parent")).expect("global parent");
    let _tachi_home = EnvRestore::set_path("TACHI_HOME", &tachi_home);
    let _sigil_home = EnvRestore::remove("SIGIL_HOME");
    let _app_home = EnvRestore::remove("TACHI_APP_HOME");
    seed_project_db(&tachi_home, &bound_project);
    seed_project_db(&tachi_home, &other_project);

    let rt = test_runtime();
    let (ct, daemon_task) = rt.block_on(async {
        let server = crate::MemoryServer::new(global.clone(), None).expect("daemon server");
        let (daemon, ct, daemon_task) = spawn_test_http_daemon(server, &global).await;
        let proxy = StdioProxyServer {
            adapter_started_at: chrono::Utc::now(),
            tool_profile: None,
            daemon: std::sync::Arc::new(std::sync::RwLock::new(daemon)),
            app_home: tachi_home.clone(),
            global_db_path: global.clone(),
            project_db_path: Some(bound_project.clone()),
            client_project: Some(bound_project_name.to_string()),
            resolved_agent_identity: Default::default(),
            rate_limit_session: ProxyRateLimitSession::mint(),
        };

        let rejected_text = "stdio proxy e2e rejects cross-project override";
        let err = call_tool_via_stdio_proxy(
            proxy,
            "tachi_memory",
            serde_json::Map::from_iter([
                ("action".to_string(), serde_json::json!("save")),
                ("project".to_string(), serde_json::json!(other_project_name)),
                ("text".to_string(), serde_json::json!(rejected_text)),
                (
                    "summary".to_string(),
                    serde_json::json!("stdio proxy e2e reject"),
                ),
                (
                    "path".to_string(),
                    serde_json::json!("/tests/stdio-proxy-e2e-reject"),
                ),
                ("category".to_string(), serde_json::json!("fact")),
                ("scope".to_string(), serde_json::json!("project")),
                ("force".to_string(), serde_json::json!(true)),
            ]),
        )
        .await
        .expect_err("cross-project override should be rejected before dispatch");

        assert!(
            err.message.contains("stdio proxy project binding mismatch"),
            "unexpected error: {err:?}"
        );
        (ct, daemon_task)
    });

    let rejected_text = "stdio proxy e2e rejects cross-project override";
    assert_eq!(memory_text_count(&bound_project, rejected_text), 0);
    assert_eq!(memory_text_count(&other_project, rejected_text), 0);
    assert_eq!(memory_text_count(&global, rejected_text), 0);

    ct.cancel();
    rt.block_on(daemon_task).expect("daemon task");
}

#[test]
fn stdio_proxy_tachi_search_returns_global_and_bound_project_rows() {
    let _guard = crate::utils::global_test_lock()
        .lock()
        .unwrap_or_else(|e| e.into_inner());

    let temp = tempfile::tempdir().expect("tempdir");
    let tachi_home = temp.path().join("home");
    let global = tachi_home.join("global/memory.db");
    let project_name = "Sigil-proxy-search-e2e";
    let project = tachi_home
        .join("projects")
        .join(project_name)
        .join("memory.db");
    std::fs::create_dir_all(global.parent().expect("global parent")).expect("global parent");
    let _tachi_home = EnvRestore::set_path("TACHI_HOME", &tachi_home);
    let _sigil_home = EnvRestore::remove("SIGIL_HOME");
    let _app_home = EnvRestore::remove("TACHI_APP_HOME");
    seed_project_db(&tachi_home, &project);

    let rt = test_runtime();
    let (ct, daemon_task) = rt.block_on(async {
        let server = crate::MemoryServer::new(global.clone(), None).expect("daemon server");
        let (daemon, ct, daemon_task) = spawn_test_http_daemon(server, &global).await;
        let proxy = StdioProxyServer {
            adapter_started_at: chrono::Utc::now(),
            tool_profile: None,
            daemon: std::sync::Arc::new(std::sync::RwLock::new(daemon)),
            app_home: tachi_home.clone(),
            global_db_path: global.clone(),
            project_db_path: Some(project.clone()),
            client_project: Some(project_name.to_string()),
            resolved_agent_identity: Default::default(),
            rate_limit_session: ProxyRateLimitSession::mint(),
        };

        for (id, scope, summary) in [
            (
                "global-proxy-search-e2e",
                "global",
                "global PROXYREADMERGE row",
            ),
            (
                "project-proxy-search-e2e",
                "project",
                "project PROXYREADMERGE row",
            ),
        ] {
            let result = call_tool_via_stdio_proxy(
                proxy.clone(),
                "tachi_memory",
                serde_json::Map::from_iter([
                    ("action".to_string(), serde_json::json!("save")),
                    ("id".to_string(), serde_json::json!(id)),
                    (
                        "text".to_string(),
                        serde_json::json!(format!("{summary} lossless text")),
                    ),
                    ("summary".to_string(), serde_json::json!(summary)),
                    (
                        "path".to_string(),
                        serde_json::json!("/tests/stdio-proxy-search-e2e"),
                    ),
                    ("category".to_string(), serde_json::json!("fact")),
                    ("scope".to_string(), serde_json::json!(scope)),
                    ("force".to_string(), serde_json::json!(true)),
                ]),
            )
            .await
            .unwrap_or_else(|err| panic!("seed {id}: {err}"));
            assert_tool_ok(&result);
        }

        let result = call_tool_via_stdio_proxy(
            proxy,
            "tachi_memory",
            serde_json::Map::from_iter([
                ("action".to_string(), serde_json::json!("search")),
                ("query".to_string(), serde_json::json!("PROXYREADMERGE")),
                ("scope".to_string(), serde_json::json!("memory")),
                ("top_k".to_string(), serde_json::json!(10)),
                ("format".to_string(), serde_json::json!("json")),
            ]),
        )
        .await
        .expect("proxied tachi_memory search should succeed");
        assert_tool_ok(&result);
        let text = first_text(&result);
        assert!(
            text.contains("global-proxy-search-e2e"),
            "proxied search lost global row: {text}"
        );
        assert!(
            text.contains("project-proxy-search-e2e"),
            "proxied search lost bound project row: {text}"
        );

        (ct, daemon_task)
    });

    ct.cancel();
    rt.block_on(daemon_task).expect("daemon task");
}

#[test]
fn stdio_proxy_allows_explicit_cross_project_read() {
    let _guard = crate::utils::global_test_lock()
        .lock()
        .unwrap_or_else(|e| e.into_inner());

    let temp = tempfile::tempdir().expect("tempdir");
    let tachi_home = temp.path().join("home");
    let global = tachi_home.join("global/memory.db");
    let bound_project_name = "Sigil-proxy-cross-read-e2e";
    let other_project_name = "Quant-proxy-cross-read-e2e";
    let bound_project = tachi_home
        .join("projects")
        .join(bound_project_name)
        .join("memory.db");
    let other_project = tachi_home
        .join("projects")
        .join(other_project_name)
        .join("memory.db");
    std::fs::create_dir_all(global.parent().expect("global parent")).expect("global parent");
    let _tachi_home = EnvRestore::set_path("TACHI_HOME", &tachi_home);
    let _sigil_home = EnvRestore::remove("SIGIL_HOME");
    let _app_home = EnvRestore::remove("TACHI_APP_HOME");
    seed_project_db(&tachi_home, &bound_project);
    seed_project_db(&tachi_home, &other_project);

    let rt = test_runtime();
    let (ct, daemon_task) = rt.block_on(async {
        let server = crate::MemoryServer::new(global.clone(), None).expect("daemon server");
        let (daemon, ct, daemon_task) = spawn_test_http_daemon(server, &global).await;
        let daemon = std::sync::Arc::new(std::sync::RwLock::new(daemon));
        let bound_proxy = StdioProxyServer {
            adapter_started_at: chrono::Utc::now(),
            tool_profile: None,
            daemon: daemon.clone(),
            app_home: tachi_home.clone(),
            global_db_path: global.clone(),
            project_db_path: Some(bound_project.clone()),
            client_project: Some(bound_project_name.to_string()),
            resolved_agent_identity: Default::default(),
            rate_limit_session: ProxyRateLimitSession::mint(),
        };
        let other_proxy = StdioProxyServer {
            adapter_started_at: chrono::Utc::now(),
            tool_profile: None,
            daemon,
            app_home: tachi_home.clone(),
            global_db_path: global.clone(),
            project_db_path: Some(other_project.clone()),
            client_project: Some(other_project_name.to_string()),
            resolved_agent_identity: Default::default(),
            rate_limit_session: ProxyRateLimitSession::mint(),
        };

        let result = call_tool_via_stdio_proxy(
            other_proxy,
            "tachi_memory",
            serde_json::Map::from_iter([
                ("action".to_string(), serde_json::json!("save")),
                (
                    "id".to_string(),
                    serde_json::json!("other-proxy-cross-read-e2e"),
                ),
                (
                    "text".to_string(),
                    serde_json::json!("other PROXYCROSSREAD row"),
                ),
                (
                    "summary".to_string(),
                    serde_json::json!("other PROXYCROSSREAD row"),
                ),
                (
                    "path".to_string(),
                    serde_json::json!("/tests/stdio-proxy-cross-read-e2e"),
                ),
                ("category".to_string(), serde_json::json!("fact")),
                ("scope".to_string(), serde_json::json!("project")),
                ("force".to_string(), serde_json::json!(true)),
            ]),
        )
        .await
        .expect("seed other project through proxy");
        assert_tool_ok(&result);

        let result = call_tool_via_stdio_proxy(
            bound_proxy,
            "tachi_memory",
            serde_json::Map::from_iter([
                ("action".to_string(), serde_json::json!("search")),
                ("project".to_string(), serde_json::json!(other_project_name)),
                ("query".to_string(), serde_json::json!("PROXYCROSSREAD")),
                ("scope".to_string(), serde_json::json!("memory")),
                ("top_k".to_string(), serde_json::json!(10)),
                ("format".to_string(), serde_json::json!("json")),
            ]),
        )
        .await
        .expect("explicit cross-project read should be forwarded");
        assert_tool_ok(&result);
        let parsed = first_text_json(&result);
        assert_search_section_rows_are_objects(&parsed);
        let text = parsed.to_string();
        assert!(
            text.contains("other-proxy-cross-read-e2e"),
            "proxied explicit project read lost other project row: {parsed:#}"
        );

        (ct, daemon_task)
    });

    ct.cancel();
    rt.block_on(daemon_task).expect("daemon task");
}

#[test]
fn stdio_proxy_tachi_memory_search_rows_stay_objects_under_parallel_forwarding() {
    let _guard = crate::utils::global_test_lock()
        .lock()
        .unwrap_or_else(|e| e.into_inner());

    let temp = tempfile::tempdir().expect("tempdir");
    let tachi_home = temp.path().join("home");
    let global = tachi_home.join("global/memory.db");
    let project_name = "Sigil-proxy-row-shape-e2e";
    let project = tachi_home
        .join("projects")
        .join(project_name)
        .join("memory.db");
    std::fs::create_dir_all(global.parent().expect("global parent")).expect("global parent");
    let _tachi_home = EnvRestore::set_path("TACHI_HOME", &tachi_home);
    let _sigil_home = EnvRestore::remove("SIGIL_HOME");
    let _app_home = EnvRestore::remove("TACHI_APP_HOME");
    // This row-shape/forwarding contract must not inherit an ambient provider
    // key and fan out 32 provider-health writes against the fixture DB.
    let _embedding_disabled = EnvRestore::set("TACHI_SEARCH_DISABLE_QUERY_EMBEDDING", "1");
    seed_project_db(&tachi_home, &project);

    let rt = test_runtime();
    let (ct, daemon_task) = rt.block_on(async {
        let server = crate::MemoryServer::new(global.clone(), None).expect("daemon server");
        let (daemon, ct, daemon_task) = spawn_test_http_daemon(server, &global).await;
        let proxy = StdioProxyServer {
            adapter_started_at: chrono::Utc::now(),
            tool_profile: Some(tachi_hub::ToolProfile::operate()),
            daemon: std::sync::Arc::new(std::sync::RwLock::new(daemon)),
            app_home: tachi_home.clone(),
            global_db_path: global.clone(),
            project_db_path: Some(project.clone()),
            client_project: Some(project_name.to_string()),
            resolved_agent_identity: Default::default(),
            rate_limit_session: ProxyRateLimitSession::mint(),
        };

        for (id, scope, summary) in [
            (
                "global-proxy-row-shape-e2e",
                "global",
                "global PROXYROWSHAPE row",
            ),
            (
                "project-proxy-row-shape-e2e",
                "project",
                "project PROXYROWSHAPE row",
            ),
        ] {
            let result = call_tool_via_stdio_proxy(
                proxy.clone(),
                "tachi_memory",
                serde_json::Map::from_iter([
                    ("action".to_string(), serde_json::json!("save")),
                    ("id".to_string(), serde_json::json!(id)),
                    (
                        "text".to_string(),
                        serde_json::json!(format!("{summary} lossless text")),
                    ),
                    ("summary".to_string(), serde_json::json!(summary)),
                    (
                        "path".to_string(),
                        serde_json::json!("/tests/stdio-proxy-row-shape-e2e"),
                    ),
                    ("category".to_string(), serde_json::json!("fact")),
                    ("scope".to_string(), serde_json::json!(scope)),
                    ("force".to_string(), serde_json::json!(true)),
                ]),
            )
            .await
            .unwrap_or_else(|err| panic!("seed {id}: {err}"));
            assert_tool_ok(&result);
        }

        let search_args = |i: usize| {
            serde_json::Map::from_iter([
                ("action".to_string(), serde_json::json!("search")),
                ("query".to_string(), serde_json::json!("PROXYROWSHAPE")),
                ("scope".to_string(), serde_json::json!("memory")),
                ("top_k".to_string(), serde_json::json!(10 + i)),
                ("format".to_string(), serde_json::json!("json")),
            ])
        };
        let calls = (0..32)
            .map(|i| call_tool_via_stdio_proxy(proxy.clone(), "tachi_memory", search_args(i)));
        let results = futures::future::join_all(calls).await;

        for result in results {
            let result = result.expect("proxied tachi_memory search should succeed");
            assert_tool_ok(&result);
            let parsed = first_text_json(&result);
            assert_search_section_rows_are_objects(&parsed);
            let text = parsed.to_string();
            assert!(
                text.contains("global-proxy-row-shape-e2e"),
                "proxied search lost global row: {parsed:#}"
            );
            assert!(
                text.contains("project-proxy-row-shape-e2e"),
                "proxied search lost bound project row: {parsed:#}"
            );
        }

        let runtime = call_tool_via_stdio_proxy(proxy, "runtime_info", serde_json::Map::new())
            .await
            .expect("runtime_info should succeed");
        assert_tool_ok(&runtime);

        (ct, daemon_task)
    });

    ct.cancel();
    rt.block_on(daemon_task).expect("daemon task");
    assert_eq!(
        memory_access_count(&global, "global-proxy-row-shape-e2e"),
        32,
        "the isolated forwarding test must retain per-search access recording"
    );
}

// tachi#1222: `runtime_info` must re-derive the daemon identity block on
// every call instead of echoing whatever `DaemonInfo` the adapter happened to
// cache at startup. This test seeds the `StdioProxyServer`'s cached RwLock
// with a deliberately WRONG identity (pid 9999, an unreachable port), then
// writes the real scoped pid/lock file the adapter is supposed to re-read.
// If `runtime_info` reflected the cache, it would report the fake identity;
// if it re-derives fresh (as `tachi#1222` requires), it reports whatever the
// pid file says right now -- and reflects it again after the pid file is
// mutated to point at a second listener, without ever touching the cache.
#[test]
fn stdio_proxy_runtime_info_reflects_pid_file_changes_not_cached_snapshot() {
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

    let rt = test_runtime();
    rt.block_on(async {
        let listener_a = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind listener a");
        let port_a = listener_a.local_addr().expect("addr a").port();
        let listener_b = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind listener b");
        let port_b = listener_b.local_addr().expect("addr b").port();

        let pid_path = crate::daemon_lock::scoped_daemon_pid_path(&tachi_home, &global);
        std::fs::write(
            &pid_path,
            serde_json::json!({
                "pid": 1111,
                "port": port_a,
                "url": format!("http://127.0.0.1:{port_a}/mcp"),
                "global_db": global.display().to_string(),
                "project_db": null,
                "version": env!("CARGO_PKG_VERSION"),
            })
            .to_string(),
        )
        .expect("write pid file (first identity)");

        // Cached snapshot is deliberately a different identity that does not
        // even resolve (127.0.0.1:1 has nothing listening) -- if
        // `runtime_info` ever fell back to this, the reachability probe below
        // would fail, proving the bug this test guards against.
        let stale_cached = crate::cli_client::DaemonInfo {
            url: "http://127.0.0.1:1/mcp".to_string(),
            global_db: Some(global.display().to_string()),
            project_db: None,
            version: Some("0.0.0-stale-cache".to_string()),
            pid: Some(9999),
            internal_proxy_token: None,
        };
        let proxy = StdioProxyServer {
            adapter_started_at: chrono::Utc::now(),
            tool_profile: Some(tachi_hub::ToolProfile::operate()),
            daemon: std::sync::Arc::new(std::sync::RwLock::new(stale_cached)),
            app_home: tachi_home.clone(),
            global_db_path: global.clone(),
            project_db_path: None,
            client_project: None,
            resolved_agent_identity: Default::default(),
            rate_limit_session: ProxyRateLimitSession::mint(),
        };

        let first = call_tool_via_stdio_proxy(proxy.clone(), "runtime_info", serde_json::Map::new())
            .await
            .expect("runtime_info should succeed (first identity)");
        assert_tool_ok(&first);
        let first_body = first_text_json(&first);
        assert_eq!(
            first_body["daemon"]["reachable"], true,
            "runtime_info should report the pid-file daemon as reachable: {first_body:#}"
        );
        assert_eq!(
            first_body["daemon"]["pid"], 1111,
            "runtime_info should report the freshly-read pid, not the cached 9999: {first_body:#}"
        );
        assert_eq!(
            first_body["transport"]["target"],
            format!("http://127.0.0.1:{port_a}/mcp"),
            "runtime_info should target the freshly-read port, not the cached stale URL: {first_body:#}"
        );
        assert!(
            first_body.get("adapter_started_at").and_then(|v| v.as_str()).is_some(),
            "runtime_info should carry adapter_started_at: {first_body:#}"
        );
        assert!(
            first_body
                .get("daemon_identity_as_of")
                .and_then(|v| v.as_str())
                .is_some(),
            "runtime_info should carry daemon_identity_as_of: {first_body:#}"
        );

        // Mutate the pid file in place to a second, distinct identity, without
        // ever touching the proxy's cached RwLock.
        std::fs::write(
            &pid_path,
            serde_json::json!({
                "pid": 2222,
                "port": port_b,
                "url": format!("http://127.0.0.1:{port_b}/mcp"),
                "global_db": global.display().to_string(),
                "project_db": null,
                "version": env!("CARGO_PKG_VERSION"),
            })
            .to_string(),
        )
        .expect("write pid file (second identity)");

        let second = call_tool_via_stdio_proxy(proxy, "runtime_info", serde_json::Map::new())
            .await
            .expect("runtime_info should succeed (second identity)");
        assert_tool_ok(&second);
        let second_body = first_text_json(&second);
        assert_eq!(
            second_body["daemon"]["pid"], 2222,
            "runtime_info should reflect the mutated pid file's new pid: {second_body:#}"
        );
        assert_eq!(
            second_body["transport"]["target"],
            format!("http://127.0.0.1:{port_b}/mcp"),
            "runtime_info should reflect the mutated pid file's new port: {second_body:#}"
        );

        drop(listener_a);
        drop(listener_b);
    });
}

// tachi#1222: when no daemon answers for this global DB (pid file missing,
// or pointing at a dead port), `runtime_info` must say so explicitly rather
// than serving the last-known-good cached identity as if it were current.
#[test]
fn stdio_proxy_runtime_info_reports_unreachable_when_daemon_absent() {
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
    // No pid file is written at all: `detect_daemon_for_global_db` has
    // nothing to read (neither the scoped nor legacy path exists).

    let rt = test_runtime();
    rt.block_on(async {
        let cached_but_dead = crate::cli_client::DaemonInfo {
            url: "http://127.0.0.1:1/mcp".to_string(),
            global_db: Some(global.display().to_string()),
            project_db: None,
            version: Some(env!("CARGO_PKG_VERSION").to_string()),
            pid: Some(4242),
            internal_proxy_token: None,
        };
        let proxy = StdioProxyServer {
            adapter_started_at: chrono::Utc::now(),
            tool_profile: Some(tachi_hub::ToolProfile::operate()),
            daemon: std::sync::Arc::new(std::sync::RwLock::new(cached_but_dead)),
            app_home: tachi_home.clone(),
            global_db_path: global.clone(),
            project_db_path: None,
            client_project: None,
            resolved_agent_identity: Default::default(),
            rate_limit_session: ProxyRateLimitSession::mint(),
        };

        let result = call_tool_via_stdio_proxy(proxy, "runtime_info", serde_json::Map::new())
            .await
            .expect("runtime_info should still succeed as a tool call when the daemon is absent");
        // Explicit-unreachable must not be surfaced as an MCP tool error --
        // it is a truthful data field, not a failed call.
        assert_tool_ok(&result);
        let body = first_text_json(&result);
        assert_eq!(
            body["daemon"]["reachable"], false,
            "runtime_info should mark the daemon unreachable rather than echo the stale cache: {body:#}"
        );
        assert!(
            body["daemon"]["pid"].is_null(),
            "unreachable daemon block should not leak the stale cached pid: {body:#}"
        );
        assert!(
            body["transport"]["target"].is_null(),
            "unreachable daemon block should not leak the stale cached target URL: {body:#}"
        );
        assert!(
            body.get("adapter_started_at").and_then(|v| v.as_str()).is_some(),
            "runtime_info should still carry adapter_started_at when unreachable: {body:#}"
        );
        assert!(
            body.get("daemon_identity_as_of")
                .and_then(|v| v.as_str())
                .is_some(),
            "runtime_info should still carry daemon_identity_as_of when unreachable: {body:#}"
        );
    });
}

#[test]
fn stdio_proxy_archives_global_row_with_bound_project() {
    let _guard = crate::utils::global_test_lock()
        .lock()
        .unwrap_or_else(|e| e.into_inner());

    let temp = tempfile::tempdir().expect("tempdir");
    let tachi_home = temp.path().join("home");
    let global = tachi_home.join("global/memory.db");
    let project_name = "Sigil-proxy-archive-e2e";
    let project = tachi_home
        .join("projects")
        .join(project_name)
        .join("memory.db");
    std::fs::create_dir_all(global.parent().expect("global parent")).expect("global parent");
    let _tachi_home = EnvRestore::set_path("TACHI_HOME", &tachi_home);
    let _sigil_home = EnvRestore::remove("SIGIL_HOME");
    let _app_home = EnvRestore::remove("TACHI_APP_HOME");
    seed_project_db(&tachi_home, &project);

    let rt = test_runtime();
    let (ct, daemon_task) = rt.block_on(async {
        let server = crate::MemoryServer::new(global.clone(), None).expect("daemon server");
        server.set_tool_profile(Some(tachi_hub::ToolProfile::admin()));
        let (daemon, ct, daemon_task) = spawn_test_http_daemon(server, &global).await;
        let proxy = StdioProxyServer {
            adapter_started_at: chrono::Utc::now(),
            tool_profile: Some(tachi_hub::ToolProfile::admin()),
            daemon: std::sync::Arc::new(std::sync::RwLock::new(daemon)),
            app_home: tachi_home.clone(),
            global_db_path: global.clone(),
            project_db_path: Some(project.clone()),
            client_project: Some(project_name.to_string()),
            resolved_agent_identity: Default::default(),
            rate_limit_session: ProxyRateLimitSession::mint(),
        };

        let result = call_tool_via_stdio_proxy(
            proxy.clone(),
            "tachi_memory",
            serde_json::Map::from_iter([
                ("action".to_string(), serde_json::json!("save")),
                (
                    "id".to_string(),
                    serde_json::json!("global-proxy-archive-e2e"),
                ),
                (
                    "text".to_string(),
                    serde_json::json!("global archive fallback row PROXYMUTATEGLOBAL"),
                ),
                (
                    "summary".to_string(),
                    serde_json::json!("global archive fallback row"),
                ),
                (
                    "path".to_string(),
                    serde_json::json!("/tests/stdio-proxy-mutate-global"),
                ),
                ("category".to_string(), serde_json::json!("fact")),
                ("scope".to_string(), serde_json::json!("global")),
                ("force".to_string(), serde_json::json!(true)),
            ]),
        )
        .await
        .expect("seed archive row");
        assert_tool_ok(&result);

        let archived = call_tool_via_stdio_proxy(
            proxy,
            "archive_memory",
            serde_json::Map::from_iter([(
                "id".to_string(),
                serde_json::json!("global-proxy-archive-e2e"),
            )]),
        )
        .await
        .expect("proxied archive should succeed");
        assert_tool_ok(&archived);
        let archived = first_text_json(&archived);
        assert_eq!(archived["archived"], serde_json::json!(true));
        assert_eq!(archived["db"], serde_json::json!("global"));

        (ct, daemon_task)
    });

    assert_eq!(
        memory_archived_value(&global, "global-proxy-archive-e2e"),
        1
    );
    assert_eq!(memory_id_count(&project, "global-proxy-archive-e2e"), 0);

    ct.cancel();
    rt.block_on(daemon_task).expect("daemon task");
}

#[test]
fn auto_daemon_args_preserve_global_only_scope() {
    let args = auto_daemon_command_args(Path::new("/tmp/agent.db"), None, "0");
    let rendered: Vec<String> = args
        .iter()
        .map(|arg| arg.to_string_lossy().into_owned())
        .collect();

    assert_eq!(
        rendered,
        vec![
            "--daemon",
            "--port",
            "0",
            "--global-db",
            "/tmp/agent.db",
            "--no-project-db"
        ]
    );
}

#[test]
fn auto_daemon_args_preserve_project_scope() {
    let args = auto_daemon_command_args(
        Path::new("/tmp/global.db"),
        Some(Path::new("/tmp/project.db")),
        "1234",
    );
    let rendered: Vec<String> = args
        .iter()
        .map(|arg| arg.to_string_lossy().into_owned())
        .collect();

    assert_eq!(
        rendered,
        vec![
            "--daemon",
            "--port",
            "1234",
            "--global-db",
            "/tmp/global.db",
            "--project-db",
            "/tmp/project.db"
        ]
    );
}

#[test]
fn stdio_proxy_accepts_matching_project_daemon() {
    let global = Path::new("/tmp/tachi/global/memory.db");
    let project = Path::new("/tmp/tachi/sigil/memory.db");
    let info = daemon(Some(global), Some(project));

    assert!(proxy_can_preserve_project_context(
        &info,
        Path::new("/tmp/tachi"),
        global,
        Some(project),
        None
    ));
}

#[test]
fn stdio_proxy_rejects_matching_project_daemon_with_wrong_named_binding() {
    let temp = tempfile::tempdir().expect("tempdir");
    let tachi_home = temp.path().join("home");
    let global = tachi_home.join("global/memory.db");
    let project = tachi_home.join("projects/Sigil-test/memory.db");
    let other = tachi_home.join("projects/Quant-test/memory.db");
    for path in [&global, &project, &other] {
        std::fs::create_dir_all(path.parent().expect("parent")).expect("parent");
        std::fs::write(path, b"").expect("db placeholder");
    }
    let info = daemon(Some(&global), Some(&project));

    with_tachi_home(&tachi_home, || {
        assert!(!proxy_can_preserve_project_context(
            &info,
            &tachi_home,
            &global,
            Some(&project),
            Some("Quant-test")
        ));
    });
}

#[test]
fn stdio_proxy_rejects_named_project_when_daemon_project_differs() {
    let global = Path::new("/tmp/tachi/global/memory.db");
    let daemon_project = Path::new("/tmp/tachi/quant/memory.db");
    let requested_project = Path::new("/tmp/tachi/sigil/memory.db");
    let info = daemon(Some(global), Some(daemon_project));

    assert!(!proxy_can_preserve_project_context(
        &info,
        Path::new("/tmp/tachi"),
        global,
        Some(requested_project),
        Some("Sigil-test")
    ));
}

#[test]
fn stdio_proxy_rejects_project_daemon_for_no_project_client() {
    let global = Path::new("/tmp/tachi/global/memory.db");
    let project = Path::new("/tmp/tachi/quant/memory.db");
    let info = daemon(Some(global), Some(project));

    assert!(!proxy_can_preserve_project_context(
        &info,
        Path::new("/tmp/tachi"),
        global,
        None,
        None
    ));
}

#[test]
fn stdio_proxy_accepts_global_only_daemon_for_no_project_client() {
    let global = Path::new("/tmp/tachi/global/memory.db");
    let info = daemon(Some(global), None);

    assert!(proxy_can_preserve_project_context(
        &info,
        Path::new("/tmp/tachi"),
        global,
        None,
        None
    ));
}

#[test]
fn stdio_proxy_accepts_global_only_daemon_for_bound_named_project_client() {
    let temp = tempfile::tempdir().expect("tempdir");
    let tachi_home = temp.path().join("home");
    let global = tachi_home.join("global/memory.db");
    let project = tachi_home.join("projects/Sigil-test/memory.db");
    std::fs::create_dir_all(global.parent().expect("global parent")).expect("global parent");
    std::fs::create_dir_all(project.parent().expect("project parent")).expect("project parent");
    std::fs::write(&project, b"").expect("project db placeholder");
    let info = daemon(Some(&global), None);

    with_tachi_home(&tachi_home, || {
        assert!(proxy_can_preserve_project_context(
            &info,
            &tachi_home,
            &global,
            Some(&project),
            Some("Sigil-test")
        ));
    });
}

#[test]
fn stdio_proxy_rejects_unverified_named_project_binding() {
    let temp = tempfile::tempdir().expect("tempdir");
    let tachi_home = temp.path().join("home");
    let global = tachi_home.join("global/memory.db");
    let project = temp.path().join("repo/.tachi/memory.db");
    std::fs::create_dir_all(global.parent().expect("global parent")).expect("global parent");
    std::fs::create_dir_all(project.parent().expect("project parent")).expect("project parent");
    std::fs::write(&project, b"").expect("project db placeholder");
    let info = daemon(Some(&global), None);

    with_tachi_home(&tachi_home, || {
        assert!(!proxy_can_preserve_project_context(
            &info,
            &tachi_home,
            &global,
            Some(&project),
            Some("Sigil-test")
        ));
    });
}

#[test]
fn stdio_proxy_env_gate_accepts_common_truthy_values() {
    assert!(env_truthy("1"));
    assert!(env_truthy("true"));
    assert!(env_truthy(" yes "));
    assert!(!env_truthy("0"));
    assert!(!env_truthy("false"));
    assert!(!env_truthy(""));
}

#[test]
fn proxy_no_longer_rewrites_retired_briefing_route() {
    let request = rmcp::model::CallToolRequestParams::new("tachi_briefing");

    let unchanged =
        prepare_proxy_tool_call(request, Some("Sigil-abc123")).expect("preflight succeeds");

    assert_eq!(unchanged.name.as_ref(), "tachi_briefing");
    let arguments = unchanged.arguments.unwrap_or_default();
    for rewritten in ["action", "format", "compact", "project", "query"] {
        assert!(!arguments.contains_key(rewritten), "{rewritten}");
    }
}

// #1041 B1: the stdio proxy's `prepare_proxy_tool_call` is a Preflight hop
// only — it validates/rejects an explicit `project=` but never injects a
// default for an absent one (see `EnforcementRole::Preflight`). Injecting
// the bound project for an omitted argument is the daemon's own
// `call_tool` (Authoritative) job, exercised end-to-end (through a real
// spawned daemon, asserting the write landed on the right db) by
// `stdio_proxy_call_writes_bound_project_via_global_only_daemon` above.
#[test]
fn preflight_maps_tachi_memory_project_actions_without_injecting() {
    let save = rmcp::model::CallToolRequestParams::new("tachi_memory").with_arguments(
        serde_json::Map::from_iter([("action".to_string(), serde_json::json!("save"))]),
    );
    let save = prepare_proxy_tool_call(save, Some("Sigil-abc123")).expect("save mapped");
    assert!(
        !save.arguments.expect("save args").contains_key("project"),
        "preflight must not inject a default project for save"
    );

    let search = rmcp::model::CallToolRequestParams::new("tachi_memory").with_arguments(
        serde_json::Map::from_iter([("action".to_string(), serde_json::json!("search"))]),
    );
    let search = prepare_proxy_tool_call(search, Some("Sigil-abc123")).expect("search mapped");
    assert!(
        !search
            .arguments
            .expect("search args")
            .contains_key("project"),
        "preflight must not inject a default project for search"
    );

    let action = "consolidate";
    let request = rmcp::model::CallToolRequestParams::new("tachi_memory").with_arguments(
        serde_json::Map::from_iter([("action".to_string(), serde_json::json!(action))]),
    );
    let mapped = prepare_proxy_tool_call(request, Some("Sigil-abc123")).expect("action mapped");
    assert!(
        !mapped.arguments.expect("args").contains_key("project"),
        "{action} preflight must not inject a default project"
    );
}

#[test]
fn preflight_maps_raw_project_tools_without_injecting() {
    for tool in [
        "search_memory",
        "find_similar_memory",
        "get_memory",
        "list_memories",
        "archive_memory",
        "ingest_event",
        "tachi_search",
    ] {
        let mapped = prepare_proxy_tool_call(
            rmcp::model::CallToolRequestParams::new(tool),
            Some("Sigil-abc123"),
        )
        .unwrap_or_else(|err| panic!("{tool} should map: {err}"));
        assert!(
            !mapped.arguments.expect("args").contains_key("project"),
            "{tool} preflight must not inject a default project"
        );
    }
}

#[test]
fn proxy_does_not_override_explicit_global_scope() {
    let request = rmcp::model::CallToolRequestParams::new("tachi_save").with_arguments(
        serde_json::Map::from_iter([("scope".to_string(), serde_json::json!("global"))]),
    );

    let mapped = prepare_proxy_tool_call(request, Some("Sigil-abc123")).expect("mapped");

    assert!(
        !mapped.arguments.expect("args").contains_key("project"),
        "explicit global writes must remain global"
    );
}

#[test]
fn proxy_rejects_explicit_cross_project_write_override() {
    let request = rmcp::model::CallToolRequestParams::new("tachi_save").with_arguments(
        serde_json::Map::from_iter([("project".to_string(), serde_json::json!("Quant-test"))]),
    );

    let err = prepare_proxy_tool_call(request, Some("Sigil-test"))
        .expect_err("cross-project override should be rejected");

    assert!(
        err.to_string().contains("project binding mismatch"),
        "unexpected error: {err}"
    );

    let request = rmcp::model::CallToolRequestParams::new("tachi_wiki").with_arguments(
        serde_json::Map::from_iter([
            ("action".to_string(), serde_json::json!("write")),
            ("project".to_string(), serde_json::json!("Quant-test")),
        ]),
    );

    let err = prepare_proxy_tool_call(request, Some("Sigil-test"))
        .expect_err("cross-project wiki write should be rejected");

    assert!(
        err.to_string().contains("project binding mismatch"),
        "unexpected error: {err}"
    );

    for (tool, action) in [
        ("tachi_memory", Some("save")),
        ("tachi_memory", Some("extract_facts")),
        ("tachi_memory", Some("checkpoint")),
        ("save_memory", None),
        ("tachi_event", Some("emit")),
        // #1426: the tuning writes kept their cross-project refusal when they
        // moved to tachi_tune — route_apply writes route-policy rule rows and
        // recall_apply writes config.env.
        ("tachi_tune", Some("route_apply")),
        ("tachi_tune", Some("recall_apply")),
    ] {
        let mut args =
            serde_json::Map::from_iter([("project".to_string(), serde_json::json!("Quant-test"))]);
        if let Some(action) = action {
            args.insert("action".to_string(), serde_json::json!(action));
        }
        let request = rmcp::model::CallToolRequestParams::new(tool).with_arguments(args);

        let err = prepare_proxy_tool_call(request, Some("Sigil-test"))
            .expect_err("{tool}/{action:?} cross-project write must be rejected");

        assert!(
            err.to_string().contains("project binding mismatch"),
            "{tool}/{action:?} unexpected error: {err}"
        );
    }
}

#[test]
fn proxy_allows_explicit_cross_project_read_override() {
    let temp = tempfile::tempdir().expect("tempdir");
    with_tachi_home(temp.path(), || {
        seed_identity_db(temp.path(), "Sigil-test");
        seed_identity_db(temp.path(), "Quant-test");
        for (tool, action) in [
            ("tachi_memory", Some("search")),
            ("tachi_memory", Some("get")),
            ("tachi_memory", Some("briefing")),
            ("tachi_memory", Some("consolidate")),
            // #1426: recall_simulate kept its read-only cross-project
            // standing when it moved to the admin-only tachi_tune surface.
            ("tachi_tune", Some("recall_simulate")),
            ("tachi_search", None),
            ("search_memory", None),
            ("find_similar_memory", None),
            ("get_memory", None),
            ("list_memories", None),
            ("tachi_wiki", Some("search")),
            ("tachi_wiki", Some("browse")),
            ("tachi_wiki", Some("read")),
            ("tachi_event", Some("query")),
            ("tachi_event", Some("metrics")),
        ] {
            let mut args = serde_json::Map::from_iter([(
                "project".to_string(),
                serde_json::json!("Quant-test"),
            )]);
            if let Some(action) = action {
                args.insert("action".to_string(), serde_json::json!(action));
            }
            let request = rmcp::model::CallToolRequestParams::new(tool).with_arguments(args);

            let mapped = prepare_proxy_tool_call(request, Some("Sigil-test"))
                .unwrap_or_else(|err| panic!("{tool}/{action:?} should be forwarded: {err}"));

            assert_eq!(
                mapped.arguments.expect("args")["project"],
                serde_json::json!("Quant-test"),
                "{tool}/{action:?} should preserve explicit project"
            );
        }
    });
}

#[test]
fn proxy_allows_explicit_cross_project_direct_read_override() {
    let temp = tempfile::tempdir().expect("tempdir");
    with_tachi_home(temp.path(), || {
        seed_identity_db(temp.path(), "Sigil-test");
        seed_identity_db(temp.path(), "Quant-test");
        // #757 removed memory_graph/get_edges from MCP; remaining cross-project
        // direct reads include list_memories / get_memory / tachi_search.
        for tool in ["list_memories", "get_memory", "tachi_search"] {
            let request = rmcp::model::CallToolRequestParams::new(tool).with_arguments(
                serde_json::Map::from_iter([(
                    "project".to_string(),
                    serde_json::json!("Quant-test"),
                )]),
            );

            let mapped = prepare_proxy_tool_call(request, Some("Sigil-test"))
                .unwrap_or_else(|err| panic!("{tool} direct read should be forwarded: {err}"));

            assert_eq!(
                mapped.arguments.expect("args")["project"],
                serde_json::json!("Quant-test"),
                "{tool} should preserve explicit project"
            );
        }
    });
}

#[test]
fn proxy_rejects_cross_project_override_even_with_global_scope() {
    let request = rmcp::model::CallToolRequestParams::new("tachi_save").with_arguments(
        serde_json::Map::from_iter([
            ("scope".to_string(), serde_json::json!("global")),
            ("project".to_string(), serde_json::json!("Quant-test")),
        ]),
    );

    let err = prepare_proxy_tool_call(request, Some("Sigil-test"))
        .expect_err("cross-project override should be rejected");

    assert!(
        err.to_string().contains("project binding mismatch"),
        "unexpected error: {err}"
    );
}

fn parse_http_mcp_payload(body: &str, expected_id: i64) -> serde_json::Value {
    let data_lines = body
        .lines()
        .filter_map(|line| line.strip_prefix("data:").map(str::trim))
        .filter(|line| !line.is_empty())
        .collect::<Vec<_>>();
    let payload = data_lines
        .iter()
        .rev()
        .find_map(|line| serde_json::from_str::<serde_json::Value>(line).ok())
        .unwrap_or_else(|| {
            serde_json::from_str(body)
                .unwrap_or_else(|err| panic!("HTTP MCP body is not JSON/SSE JSON: {err}; {body}"))
        });
    assert_eq!(payload["jsonrpc"], serde_json::json!("2.0"), "{payload:#}");
    assert_eq!(payload["id"], serde_json::json!(expected_id), "{payload:#}");
    payload
}

async fn http_mcp_initialize(
    url: &str,
    headers: reqwest::header::HeaderMap,
    meta: Option<serde_json::Value>,
) -> (
    reqwest::Client,
    reqwest::header::HeaderMap,
    serde_json::Value,
) {
    let client = reqwest::Client::new();
    let mut params = serde_json::json!({
        "protocolVersion": "2024-11-05",
        "capabilities": {},
        "clientInfo": {"name": "tachi-test", "version": env!("CARGO_PKG_VERSION")},
    });
    if let Some(meta) = meta {
        params
            .as_object_mut()
            .expect("initialize params object")
            .insert("_meta".to_string(), meta);
    }
    let response = client
        .post(url)
        .headers(headers.clone())
        .json(&serde_json::json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": params,
        }))
        .send()
        .await
        .expect("send HTTP MCP initialize");
    let init_headers = response.headers().clone();
    let body = response.text().await.expect("initialize body");
    let init = parse_http_mcp_payload(&body, 1);
    let mut session_headers = headers;
    if let Some(session_id) = init_headers
        .get("mcp-session-id")
        .and_then(|value| value.to_str().ok())
    {
        session_headers.insert(
            reqwest::header::HeaderName::from_static("mcp-session-id"),
            reqwest::header::HeaderValue::from_str(session_id)
                .expect("valid mcp-session-id header"),
        );
    }
    (client, session_headers, init)
}

async fn http_mcp_initialized(
    client: &reqwest::Client,
    url: &str,
    headers: reqwest::header::HeaderMap,
) {
    client
        .post(url)
        .headers(headers)
        .json(&serde_json::json!({
            "jsonrpc": "2.0",
            "method": "notifications/initialized",
            "params": {},
        }))
        .send()
        .await
        .expect("send HTTP MCP initialized")
        .error_for_status()
        .expect("initialized status");
}

async fn http_mcp_call_tool(
    client: &reqwest::Client,
    url: &str,
    headers: reqwest::header::HeaderMap,
    id: i64,
    tool_name: &str,
    arguments: serde_json::Map<String, serde_json::Value>,
) -> serde_json::Value {
    let response = client
        .post(url)
        .headers(headers)
        .json(&serde_json::json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": "tools/call",
            "params": {
                "name": tool_name,
                "arguments": arguments,
            },
        }))
        .send()
        .await
        .expect("send HTTP MCP tool call");
    let body = response.text().await.expect("tool call body");
    parse_http_mcp_payload(&body, id)
}

async fn http_mcp_list_tools(
    client: &reqwest::Client,
    url: &str,
    headers: reqwest::header::HeaderMap,
    id: i64,
) -> serde_json::Value {
    let response = client
        .post(url)
        .headers(headers)
        .json(&serde_json::json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": "tools/list",
            "params": {},
        }))
        .send()
        .await
        .expect("send HTTP MCP tools/list");
    let body = response.text().await.expect("tools/list body");
    parse_http_mcp_payload(&body, id)
}

fn http_tool_text(response: &serde_json::Value) -> String {
    let content = response["result"]["content"]
        .as_array()
        .unwrap_or_else(|| panic!("tool result should contain content: {response:#}"));
    content
        .iter()
        .find_map(|entry| entry.get("text").and_then(|value| value.as_str()))
        .unwrap_or_else(|| panic!("tool result should contain text content: {response:#}"))
        .to_string()
}

fn http_headers(pairs: &[(&str, &str)]) -> reqwest::header::HeaderMap {
    let mut headers = reqwest::header::HeaderMap::new();
    headers.insert(
        reqwest::header::CONTENT_TYPE,
        reqwest::header::HeaderValue::from_static("application/json"),
    );
    headers.insert(
        reqwest::header::ACCEPT,
        reqwest::header::HeaderValue::from_static("application/json, text/event-stream"),
    );
    for (name, value) in pairs {
        headers.insert(
            reqwest::header::HeaderName::from_bytes(name.as_bytes()).expect("test header name"),
            reqwest::header::HeaderValue::from_str(value).expect("test header value"),
        );
    }
    headers
}

#[test]
fn http_direct_connect_header_identity_binds_profile_and_project() {
    let _guard = crate::utils::global_test_lock()
        .lock()
        .unwrap_or_else(|e| e.into_inner());

    let temp = tempfile::tempdir().expect("tempdir");
    let tachi_home = temp.path().join("home");
    let global = tachi_home.join("global/memory.db");
    let project_name = "Sigil-http-direct-e2e";
    let project = tachi_home
        .join("projects")
        .join(project_name)
        .join("memory.db");
    std::fs::create_dir_all(global.parent().expect("global parent")).expect("global parent");
    let _tachi_home = EnvRestore::set_path("TACHI_HOME", &tachi_home);
    let _sigil_home = EnvRestore::remove("SIGIL_HOME");
    let _app_home = EnvRestore::remove("TACHI_APP_HOME");
    seed_project_db(&tachi_home, &project);

    let rt = test_runtime();
    let (ct, daemon_task) = rt.block_on(async {
        let server = crate::MemoryServer::new(global.clone(), None).expect("daemon server");
        let (daemon, ct, daemon_task) = spawn_test_http_daemon(server, &global).await;
        let headers = http_headers(&[
            (crate::session_identity::HEADER_PROFILE, "standard"),
            (crate::session_identity::HEADER_CLIENT, "codex-http-test"),
            (crate::session_identity::HEADER_PROJECT, project_name),
        ]);
        let (client, session_headers, init) = http_mcp_initialize(&daemon.url, headers, None).await;
        assert!(init.get("error").is_none(), "initialize failed: {init:#}");
        http_mcp_initialized(&client, &daemon.url, session_headers.clone()).await;

        let listed = http_mcp_list_tools(&client, &daemon.url, session_headers.clone(), 2).await;
        let names = listed["result"]["tools"]
            .as_array()
            .expect("tools/list tools")
            .iter()
            .filter_map(|tool| tool["name"].as_str())
            .collect::<std::collections::BTreeSet<_>>();
        assert_eq!(
            names,
            std::collections::BTreeSet::from([
                "tachi_a2a",
                "tachi_agent_eval",
                "tachi_gh",
                "tachi_memory",
                "tachi_staff",
                "tachi_task",
            ])
        );

        for (id, hidden) in [(3, "runtime_info"), (4, "tachi_briefing")] {
            let denied = http_mcp_call_tool(
                &client,
                &daemon.url,
                session_headers.clone(),
                id,
                hidden,
                serde_json::Map::new(),
            )
            .await;
            assert!(
                http_tool_text(&denied).contains("tool not found"),
                "standard HTTP direct call should hide {hidden}: {denied:#}"
            );
        }

        for (id, action) in [
            (20, "attach_session"),
            (21, "aggregate_live"),
            (22, "future_action"),
        ] {
            let denied = http_mcp_call_tool(
                &client,
                &daemon.url,
                session_headers.clone(),
                id,
                "tachi_agent_eval",
                serde_json::Map::from_iter([("action".to_string(), serde_json::json!(action))]),
            )
            .await;
            assert!(
                http_tool_text(&denied).contains("not allowed"),
                "standard HTTP eval action {action} must be denied: {denied:#}"
            );
        }

        for (id, scope, summary) in [
            (
                "http-direct-global-e2e",
                "global",
                "global DIRECTHTTPMERGE row",
            ),
            (
                "http-direct-project-e2e",
                "project",
                "project DIRECTHTTPMERGE row",
            ),
        ] {
            let saved = http_mcp_call_tool(
                &client,
                &daemon.url,
                session_headers.clone(),
                if scope == "global" { 5 } else { 6 },
                "tachi_memory",
                serde_json::Map::from_iter([
                    ("action".to_string(), serde_json::json!("save")),
                    ("id".to_string(), serde_json::json!(id)),
                    (
                        "text".to_string(),
                        serde_json::json!(format!("{summary} lossless text")),
                    ),
                    ("summary".to_string(), serde_json::json!(summary)),
                    (
                        "path".to_string(),
                        serde_json::json!("/tests/http-direct-e2e"),
                    ),
                    ("category".to_string(), serde_json::json!("fact")),
                    ("scope".to_string(), serde_json::json!(scope)),
                    ("force".to_string(), serde_json::json!(true)),
                ]),
            )
            .await;
            assert!(
                saved["result"]["isError"] != serde_json::json!(true),
                "save {id} failed: {saved:#}"
            );
        }

        let search = http_mcp_call_tool(
            &client,
            &daemon.url,
            session_headers.clone(),
            7,
            "tachi_memory",
            serde_json::Map::from_iter([
                ("action".to_string(), serde_json::json!("search")),
                ("query".to_string(), serde_json::json!("DIRECTHTTPMERGE")),
                ("scope".to_string(), serde_json::json!("memory")),
                ("top_k".to_string(), serde_json::json!(10)),
                ("format".to_string(), serde_json::json!("json")),
            ]),
        )
        .await;
        assert!(
            search["result"]["isError"] != serde_json::json!(true),
            "search failed: {search:#}"
        );
        let text = http_tool_text(&search);
        assert!(
            text.contains("http-direct-global-e2e"),
            "HTTP search lost global row: {text}"
        );
        assert!(
            text.contains("http-direct-project-e2e"),
            "HTTP search lost bound project row: {text}"
        );

        (ct, daemon_task)
    });

    assert_eq!(
        memory_text_count(&global, "global DIRECTHTTPMERGE row lossless text"),
        1
    );
    assert_eq!(
        memory_text_count(&project, "project DIRECTHTTPMERGE row lossless text"),
        1
    );
    assert_eq!(
        memory_text_count(&global, "project DIRECTHTTPMERGE row lossless text"),
        0
    );

    ct.cancel();
    rt.block_on(daemon_task).expect("daemon task");
}

#[test]
fn http_direct_connect_rejects_privileged_profile_without_authorization_policy() {
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

    let rt = test_runtime();
    let (ct, daemon_task) = rt.block_on(async {
        let server = crate::MemoryServer::new(global.clone(), None).expect("daemon server");
        let (daemon, ct, daemon_task) = spawn_test_http_daemon(server, &global).await;
        for profile in [
            "ops",
            "operate",
            "runtime",
            "openclaw",
            "hermes",
            "adapter",
            "delegate+operate",
            "admin",
            "full",
            "emergency",
        ] {
            let headers = http_headers(&[(crate::session_identity::HEADER_PROFILE, profile)]);
            let (_client, _session_headers, init) =
                http_mcp_initialize(&daemon.url, headers, None).await;
            let error = init.get("error").unwrap_or_else(|| {
                panic!("privileged profile {profile} initialize should fail: {init:#}")
            });
            let message = error["message"].as_str().unwrap_or_default();
            assert!(
                message.contains("requires explicit authorization"),
                "unexpected {profile} rejection: {init:#}"
            );
        }
        let guessed_headers = http_headers(&[
            (crate::session_identity::HEADER_PROFILE, "ops"),
            (
                crate::session_identity::HEADER_INTERNAL_PROXY_TOKEN,
                "caller-guessed-capability",
            ),
        ]);
        let (_client, _session_headers, guessed) =
            http_mcp_initialize(&daemon.url, guessed_headers, None).await;
        assert!(
            guessed["error"]["message"]
                .as_str()
                .is_some_and(|message| message.contains("requires explicit authorization")),
            "a caller-guessed proxy capability must not authorize Ops: {guessed:#}"
        );
        let headers =
            http_headers(&[(crate::session_identity::HEADER_PROFILE, "unknown-principal")]);
        let (_client, _session_headers, init) =
            http_mcp_initialize(&daemon.url, headers, None).await;
        let message = init["error"]["message"].as_str().unwrap_or_default();
        assert!(
            message.contains("unknown HTTP direct-connect Tachi profile"),
            "unknown HTTP profile must fail closed: {init:#}"
        );
        (ct, daemon_task)
    });

    ct.cancel();
    rt.block_on(daemon_task).expect("daemon task");
}

#[test]
fn http_direct_connect_ordinary_profiles_have_exact_facades_and_hide_retired_routes() {
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

    let rt = test_runtime();
    let (ct, daemon_task) = rt.block_on(async {
        let server = crate::MemoryServer::new(global.clone(), None).expect("daemon server");
        let (daemon, ct, daemon_task) = spawn_test_http_daemon(server, &global).await;
        let expected_five = std::collections::BTreeSet::from([
            "tachi_a2a",
            "tachi_gh",
            "tachi_memory",
            "tachi_staff",
            "tachi_task",
        ]);
        let expected_standard = std::collections::BTreeSet::from([
            "tachi_a2a",
            "tachi_agent_eval",
            "tachi_gh",
            "tachi_memory",
            "tachi_staff",
            "tachi_task",
        ]);

        for profile in [
            None,
            Some("standard"),
            Some("lead"),
            Some("delegate"),
            Some("worker"),
            Some("observe"),
            Some("read"),
            Some("reader"),
            Some("remember"),
            Some("write"),
            Some("writer"),
            Some("agent"),
            Some("coordinate"),
            Some("observe+remember"),
            Some("remember+observe"),
            Some("reader+writer"),
            Some("coordinate+observe"),
            Some("observe+coordinate"),
            Some("standard+delegate"),
            Some("delegate+standard"),
        ] {
            let profile_label = profile.unwrap_or("missing/default");
            let headers = profile.map_or_else(
                || http_headers(&[]),
                |value| http_headers(&[(crate::session_identity::HEADER_PROFILE, value)]),
            );
            let (client, session_headers, init) =
                http_mcp_initialize(&daemon.url, headers, None).await;
            assert!(
                init.get("error").is_none(),
                "initialize {profile_label} failed: {init:#}"
            );
            http_mcp_initialized(&client, &daemon.url, session_headers.clone()).await;

            let listed =
                http_mcp_list_tools(&client, &daemon.url, session_headers.clone(), 2).await;
            let names = listed["result"]["tools"]
                .as_array()
                .expect("tools/list tools")
                .iter()
                .filter_map(|tool| tool["name"].as_str())
                .collect::<std::collections::BTreeSet<_>>();
            let expected = if matches!(
                profile,
                None | Some("standard" | "lead" | "standard+delegate" | "delegate+standard")
            ) {
                &expected_standard
            } else {
                &expected_five
            };
            assert_eq!(
                &names, expected,
                "unexpected HTTP tools for {profile_label}"
            );

            for (id, hidden) in [(3, "runtime_info"), (4, "tachi_briefing")] {
                let denied = http_mcp_call_tool(
                    &client,
                    &daemon.url,
                    session_headers.clone(),
                    id,
                    hidden,
                    serde_json::Map::new(),
                )
                .await;
                assert!(
                    http_tool_text(&denied).contains("tool not found"),
                    "{profile_label} HTTP direct call should hide {hidden}: {denied:#}"
                );
            }
        }

        (ct, daemon_task)
    });

    ct.cancel();
    rt.block_on(daemon_task).expect("daemon task");
}

#[test]

fn http_direct_connect_unbound_session_rejects_explicit_cross_project_write() {
    let _guard = crate::utils::global_test_lock()
        .lock()
        .unwrap_or_else(|e| e.into_inner());

    let temp = tempfile::tempdir().expect("tempdir");
    let tachi_home = temp.path().join("home");
    let global = tachi_home.join("global/memory.db");
    let attacker_project_name = "Sigil-http-unbound-attacker";
    let victim_project_name = "Sigil-http-unbound-victim";
    let attacker_project = tachi_home
        .join("projects")
        .join(attacker_project_name)
        .join("memory.db");
    let victim_project = tachi_home
        .join("projects")
        .join(victim_project_name)
        .join("memory.db");
    std::fs::create_dir_all(global.parent().expect("global parent")).expect("global parent");
    let _tachi_home = EnvRestore::set_path("TACHI_HOME", &tachi_home);
    let _sigil_home = EnvRestore::remove("SIGIL_HOME");
    let _app_home = EnvRestore::remove("TACHI_APP_HOME");
    seed_project_db(&tachi_home, &attacker_project);
    seed_project_db(&tachi_home, &victim_project);

    let victim_text = "C1 victim row must stay unmutated";
    let victim_id = "c1-victim-row";
    assert_eq!(memory_text_count(&victim_project, victim_text), 0);

    let rt = test_runtime();
    let (ct, daemon_task) = rt.block_on(async {
        let server = crate::MemoryServer::new(global.clone(), None).expect("daemon server");
        let (daemon, ct, daemon_task) = spawn_test_http_daemon(server, &global).await;
        // Intentionally NO X-Tachi-Project header → unbound session.
        let headers = http_headers(&[]);
        let (client, session_headers, init) = http_mcp_initialize(&daemon.url, headers, None).await;
        assert!(init.get("error").is_none(), "initialize failed: {init:#}");
        http_mcp_initialized(&client, &daemon.url, session_headers.clone()).await;

        // Cross-project write attempt: unbound session asks to save into the
        // victim project explicitly. Must be rejected.
        let save = http_mcp_call_tool(
            &client,
            &daemon.url,
            session_headers.clone(),
            2,
            "tachi_memory",
            serde_json::Map::from_iter([
                ("action".to_string(), serde_json::json!("save")),
                ("id".to_string(), serde_json::json!(victim_id)),
                ("text".to_string(), serde_json::json!(victim_text)),
                (
                    "summary".to_string(),
                    serde_json::json!("C1 unbound cross-project attempt"),
                ),
                (
                    "path".to_string(),
                    serde_json::json!("/tests/http-unbound-c1"),
                ),
                ("category".to_string(), serde_json::json!("fact")),
                (
                    "project".to_string(),
                    serde_json::json!(victim_project_name),
                ),
                ("force".to_string(), serde_json::json!(true)),
            ]),
        )
        .await;

        // The tool call must surface an MCP-level rejection. rmcp turns an
        // invalid_params ErrorData from call_tool into a JSON-RPC error
        // response (no "result" object).
        let rejected = save
            .get("error")
            .map(|err| {
                let msg = err["message"].as_str().unwrap_or_default();
                msg.contains("not bound to a project")
            })
            .unwrap_or_else(|| {
                // If the daemon returned a result with isError, treat that as
                // a rejection too (defensive); extract the error text.
                if save["result"]["isError"] == serde_json::json!(true) {
                    let text = http_tool_text(&save);
                    text.contains("not bound to a project")
                } else {
                    false
                }
            });
        assert!(
            rejected,
            "unbound session should reject explicit cross-project write, got: {save:#}"
        );

        (ct, daemon_task)
    });

    // Victim project DB must be unmutated by the rejected write.
    assert_eq!(
        memory_text_count(&victim_project, victim_text),
        0,
        "victim project DB was mutated despite rejection"
    );
    assert_eq!(
        memory_id_count(&victim_project, victim_id),
        0,
        "victim project DB contains the rejected row id"
    );

    ct.cancel();
    rt.block_on(daemon_task).expect("daemon task");
}

/// #732: initialize result advertises HTTP direct-connect guidance (reconnect path).
/// Meta key extraction is unit-tested in `server_handler::tests` (wire path prefers headers).
#[test]
fn http_direct_connect_initialize_advertises_http_guidance() {
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

    let rt = test_runtime();
    let (ct, daemon_task) = rt.block_on(async {
        let server = crate::MemoryServer::new(global.clone(), None).expect("daemon server");
        let (daemon, ct, daemon_task) = spawn_test_http_daemon(server, &global).await;
        let headers = http_headers(&[]);
        let (_client, _session_headers, init) =
            http_mcp_initialize(&daemon.url, headers, None).await;
        assert!(init.get("error").is_none(), "initialize failed: {init:#}");
        let blob = format!("{init}");
        assert!(
            blob.contains("HTTP direct-connect") || blob.contains("http-direct-connect.md"),
            "initialize should advertise HTTP direct-connect guidance: {init:#}"
        );
        (ct, daemon_task)
    });

    ct.cancel();
    rt.block_on(daemon_task).expect("daemon task");
}

/// #1061: HTTP direct-connect applies the same canonical alias equivalence as stdio.
#[test]
fn http_direct_connect_same_db_alias_write_normalizes_to_bound_identity() {
    let _guard = crate::utils::global_test_lock()
        .lock()
        .unwrap_or_else(|e| e.into_inner());

    let temp = tempfile::tempdir().expect("tempdir");
    let tachi_home = temp.path().join("home");
    let global = tachi_home.join("global/memory.db");
    let repo = temp.path().join("repos/Sigil");
    let project = repo.join(".tachi/memory.db");
    std::fs::create_dir_all(global.parent().expect("global parent")).expect("global parent");
    let _tachi_home = EnvRestore::set_path("TACHI_HOME", &tachi_home);
    let _sigil_home = EnvRestore::remove("SIGIL_HOME");
    let _app_home = EnvRestore::remove("TACHI_APP_HOME");
    seed_project_db(&tachi_home, &project);
    write_repo_project_manifest(&tachi_home, &[&project]);
    let requested_alias =
        crate::path_utils::plan_c_dir_name_from_root(&repo).expect("hashed alias");
    let bound_name =
        crate::path_utils::plan_c_legacy_dir_name_from_root(&repo).expect("legacy binding");
    let saved_text = "HTTP same-DB alias writes once to the bound repo DB";

    let rt = test_runtime();
    let (ct, daemon_task) = rt.block_on(async {
        let server = crate::MemoryServer::new(global.clone(), None).expect("daemon server");
        let (daemon, ct, daemon_task) = spawn_test_http_daemon(server, &global).await;
        let headers = http_headers(&[
            (crate::session_identity::HEADER_PROFILE, "standard"),
            (crate::session_identity::HEADER_PROJECT, bound_name.as_str()),
        ]);
        let (client, session_headers, init) = http_mcp_initialize(&daemon.url, headers, None).await;
        assert!(init.get("error").is_none(), "initialize failed: {init:#}");
        http_mcp_initialized(&client, &daemon.url, session_headers.clone()).await;

        let save = http_mcp_call_tool(
            &client,
            &daemon.url,
            session_headers,
            2,
            "tachi_memory",
            serde_json::Map::from_iter([
                ("action".to_string(), serde_json::json!("save")),
                ("project".to_string(), serde_json::json!(&requested_alias)),
                (
                    "id".to_string(),
                    serde_json::json!("http-same-db-alias-write"),
                ),
                ("text".to_string(), serde_json::json!(saved_text)),
                ("summary".to_string(), serde_json::json!("same DB alias")),
                (
                    "path".to_string(),
                    serde_json::json!("/tests/http-same-db-alias"),
                ),
                ("category".to_string(), serde_json::json!("fact")),
                ("scope".to_string(), serde_json::json!("project")),
                ("force".to_string(), serde_json::json!(true)),
            ]),
        )
        .await;
        assert!(
            save.get("error").is_none() && save["result"]["isError"] != serde_json::json!(true),
            "same-DB alias write failed: {save:#}"
        );
        (ct, daemon_task)
    });

    assert_eq!(memory_text_count(&project, saved_text), 1);
    assert_eq!(memory_text_count(&global, saved_text), 0);
    assert!(
        !tachi_home.join("projects").join(&bound_name).exists(),
        "validation must not create the legacy Plan C alias"
    );
    assert!(
        !tachi_home.join("projects").join(&requested_alias).exists(),
        "validation must not create the hashed Plan C alias"
    );

    ct.cancel();
    rt.block_on(daemon_task).expect("daemon task");
}

/// #732 / #737: bound HTTP session rejects cross-project *writes* (reads remain allowed).
#[test]
fn http_direct_connect_bound_session_rejects_cross_project_write() {
    let _guard = crate::utils::global_test_lock()
        .lock()
        .unwrap_or_else(|e| e.into_inner());

    let temp = tempfile::tempdir().expect("tempdir");
    let tachi_home = temp.path().join("home");
    let global = tachi_home.join("global/memory.db");
    let bound_name = "Sigil-http-bound";
    let other_name = "Sigil-http-other";
    let bound_project = tachi_home
        .join("projects")
        .join(bound_name)
        .join("memory.db");
    let other_project = tachi_home
        .join("projects")
        .join(other_name)
        .join("memory.db");
    std::fs::create_dir_all(global.parent().expect("global parent")).expect("global parent");
    let _tachi_home = EnvRestore::set_path("TACHI_HOME", &tachi_home);
    let _sigil_home = EnvRestore::remove("SIGIL_HOME");
    let _app_home = EnvRestore::remove("TACHI_APP_HOME");
    seed_project_db(&tachi_home, &bound_project);
    seed_project_db(&tachi_home, &other_project);

    let other_text = "bound session must not write foreign project";
    let other_id = "http-bound-foreign-write";

    let rt = test_runtime();
    let (ct, daemon_task) = rt.block_on(async {
        let server = crate::MemoryServer::new(global.clone(), None).expect("daemon server");
        let (daemon, ct, daemon_task) = spawn_test_http_daemon(server, &global).await;
        let headers = http_headers(&[(crate::session_identity::HEADER_PROJECT, bound_name)]);
        let (client, session_headers, init) = http_mcp_initialize(&daemon.url, headers, None).await;
        assert!(init.get("error").is_none(), "initialize failed: {init:#}");
        http_mcp_initialized(&client, &daemon.url, session_headers.clone()).await;

        let save = http_mcp_call_tool(
            &client,
            &daemon.url,
            session_headers,
            2,
            "tachi_memory",
            serde_json::Map::from_iter([
                ("action".to_string(), serde_json::json!("save")),
                ("id".to_string(), serde_json::json!(other_id)),
                ("text".to_string(), serde_json::json!(other_text)),
                ("summary".to_string(), serde_json::json!("foreign write")),
                ("path".to_string(), serde_json::json!("/tests/http-bound")),
                ("category".to_string(), serde_json::json!("fact")),
                ("project".to_string(), serde_json::json!(other_name)),
                ("force".to_string(), serde_json::json!(true)),
            ]),
        )
        .await;

        let rejected = save
            .get("error")
            .map(|err| {
                err["message"]
                    .as_str()
                    .unwrap_or_default()
                    .contains("project binding mismatch")
            })
            .unwrap_or_else(|| {
                if save["result"]["isError"] == serde_json::json!(true) {
                    http_tool_text(&save).contains("project binding mismatch")
                } else {
                    false
                }
            });
        assert!(
            rejected,
            "bound HTTP session must reject cross-project write: {save:#}"
        );
        (ct, daemon_task)
    });

    assert_eq!(memory_text_count(&other_project, other_text), 0);
    assert_eq!(memory_id_count(&other_project, other_id), 0);

    ct.cancel();
    rt.block_on(daemon_task).expect("daemon task");
}

fn initialize_request(
    meta: Option<rmcp::model::RequestMetaObject>,
) -> rmcp::model::InitializeRequestParams {
    let mut request = rmcp::model::InitializeRequestParams::new(
        rmcp::model::ClientCapabilities::default(),
        rmcp::model::Implementation::new("1761-test", "0"),
    );
    request.meta = meta;
    request
}

fn identity_probe_proxy() -> StdioProxyServer {
    StdioProxyServer {
        adapter_started_at: chrono::Utc::now(),
        tool_profile: None,
        daemon: std::sync::Arc::new(std::sync::RwLock::new(daemon(None, None))),
        app_home: PathBuf::from("/tmp"),
        global_db_path: PathBuf::from("/tmp/global.db"),
        project_db_path: None,
        client_project: None,
        resolved_agent_identity: Default::default(),
        rate_limit_session: ProxyRateLimitSession::mint(),
    }
}

#[test]
fn capture_initialize_identity_meta_wins_blank_omits_absent_uses_env() {
    let _lock = crate::utils::global_test_lock()
        .lock()
        .unwrap_or_else(|err| err.into_inner());
    let _env = EnvRestore::set(crate::session_identity::ENV_AGENT_IDENTITY, "agent.env");
    let proxy = identity_probe_proxy();

    let mut meta_wins = serde_json::Map::new();
    meta_wins.insert(
        crate::session_identity::META_AGENT_IDENTITY.to_string(),
        serde_json::json!("agent.meta"),
    );
    proxy.capture_initialize_identity(&initialize_request(Some(
        rmcp::model::RequestMetaObject::from(meta_wins),
    )));
    assert_eq!(
        proxy.forwarded_agent_identity(),
        crate::cli_client::ProxyIdentityForward::Header("agent.meta".to_string()),
        "present _meta must win over env"
    );

    let mut blank = serde_json::Map::new();
    blank.insert(
        crate::session_identity::META_AGENT_IDENTITY.to_string(),
        serde_json::json!("   "),
    );
    proxy.capture_initialize_identity(&initialize_request(Some(
        rmcp::model::RequestMetaObject::from(blank),
    )));
    assert_eq!(
        proxy.forwarded_agent_identity(),
        crate::cli_client::ProxyIdentityForward::Omit,
        "present-but-blank _meta must omit, not fall through to env"
    );

    proxy.capture_initialize_identity(&initialize_request(None));
    assert_eq!(
        proxy.forwarded_agent_identity(),
        crate::cli_client::ProxyIdentityForward::Header("agent.env".to_string()),
        "absent _meta key may take a valid process env"
    );
}

/// #1761 review: daemon-process env must not confer identity on a direct
/// HTTP session that omitted both `_meta` and `X-Tachi-Agent-Identity`.
/// Pre-fix that leak admits the session as `unavailable` (has identity,
/// not local). Post-fix it stays identity-less → `rejected`.
#[test]
fn http_direct_connect_does_not_inherit_daemon_process_env_identity() {
    let _lock = crate::utils::global_test_lock()
        .lock()
        .unwrap_or_else(|err| err.into_inner());
    let temp = tempfile::tempdir().expect("tempdir");
    let tachi_home = temp.path().join("home");
    let global = tachi_home.join("global/memory.db");
    std::fs::create_dir_all(global.parent().expect("global parent")).expect("global parent");
    let _tachi_home = EnvRestore::set_path("TACHI_HOME", &tachi_home);
    let _sigil_home = EnvRestore::remove("SIGIL_HOME");
    let _app_home = EnvRestore::remove("TACHI_APP_HOME");
    let _env = EnvRestore::set(
        crate::session_identity::ENV_AGENT_IDENTITY,
        "agent.daemon.env",
    );

    let rt = test_runtime();
    let (ct, daemon_task) = rt.block_on(async {
        let server = crate::MemoryServer::new(global.clone(), None).expect("daemon server");
        let (daemon, ct, daemon_task) = spawn_test_http_daemon(server, &global).await;
        let headers = http_headers(&[]);
        let (client, session_headers, init) = http_mcp_initialize(&daemon.url, headers, None).await;
        assert!(init.get("error").is_none(), "initialize failed: {init:#}");
        http_mcp_initialized(&client, &daemon.url, session_headers.clone()).await;

        let a2a = http_mcp_call_tool(
            &client,
            &daemon.url,
            session_headers,
            2,
            "tachi_a2a",
            serde_json::Map::from_iter([("action".to_string(), serde_json::json!("status"))]),
        )
        .await;
        let text = a2a
            .get("error")
            .and_then(|err| err.get("message"))
            .and_then(|message| message.as_str())
            .map(str::to_string)
            .unwrap_or_else(|| http_tool_text(&a2a));
        assert!(
            text.contains("a2a issuer admission rejected"),
            "direct HTTP must stay identity-less when env is the only assertion: {text}\n{a2a:#}"
        );
        assert!(
            !text.contains("unavailable"),
            "daemon env leak would admit as unavailable, not rejected: {text}\n{a2a:#}"
        );
        (ct, daemon_task)
    });

    ct.cancel();
    rt.block_on(daemon_task).expect("daemon task");
}

/// #1761 live follow-through: the stdio adapter forwards its explicitly
/// resolved identity over the daemon's loopback HTTP rail. The daemon's
/// `loopback-trust-v1` posture must retain that assertion as local so A2A can
/// use it, while the adjacent no-header test proves daemon env cannot mint it.
#[test]
fn http_loopback_explicit_agent_identity_is_self_asserted_for_a2a() {
    let _lock = crate::utils::global_test_lock()
        .lock()
        .unwrap_or_else(|err| err.into_inner());
    let temp = tempfile::tempdir().expect("tempdir");
    let tachi_home = temp.path().join("home");
    let global = tachi_home.join("global/memory.db");
    std::fs::create_dir_all(global.parent().expect("global parent")).expect("global parent");
    let _tachi_home = EnvRestore::set_path("TACHI_HOME", &tachi_home);
    let _sigil_home = EnvRestore::remove("SIGIL_HOME");
    let _app_home = EnvRestore::remove("TACHI_APP_HOME");
    let _daemon_env = EnvRestore::remove(crate::session_identity::ENV_AGENT_IDENTITY);

    let rt = test_runtime();
    let (ct, daemon_task) = rt.block_on(async {
        let server = crate::MemoryServer::new(global.clone(), None).expect("daemon server");
        let (daemon, ct, daemon_task) = spawn_test_http_daemon(server, &global).await;
        let headers = http_headers(&[(
            crate::session_identity::HEADER_AGENT_IDENTITY,
            "agent.cursor.loopback",
        )]);
        let (client, session_headers, init) = http_mcp_initialize(&daemon.url, headers, None).await;
        assert!(init.get("error").is_none(), "initialize failed: {init:#}");
        http_mcp_initialized(&client, &daemon.url, session_headers.clone()).await;

        let a2a = http_mcp_call_tool(
            &client,
            &daemon.url,
            session_headers,
            2,
            "tachi_a2a",
            serde_json::Map::from_iter([("action".to_string(), serde_json::json!("status"))]),
        )
        .await;
        assert!(a2a.get("error").is_none(), "A2A status failed: {a2a:#}");
        let body: serde_json::Value =
            serde_json::from_str(&http_tool_text(&a2a)).expect("A2A status JSON");
        assert_eq!(body["contract"], serde_json::json!("tachi.a2a.v1"));
        assert_eq!(body["status"], serde_json::json!("completed"));
        assert_eq!(
            body["actor_agent_identity_id"],
            serde_json::json!("agent.cursor.loopback"),
            "the exact explicit loopback identity must become the A2A issuer"
        );
        (ct, daemon_task)
    });

    ct.cancel();
    rt.block_on(daemon_task).expect("daemon task");
}

fn stuck_probe_args(query: &str) -> serde_json::Map<String, serde_json::Value> {
    serde_json::Map::from_iter([
        ("action".to_string(), serde_json::json!("search")),
        ("query".to_string(), serde_json::json!(query)),
        ("scope".to_string(), serde_json::json!("memory")),
        ("top_k".to_string(), serde_json::json!(1)),
    ])
}

fn has_stuck_warning(result: &rmcp::model::CallToolResult) -> bool {
    result.content.iter().any(|content| {
        matches!(content, rmcp::model::ContentBlock::Text(text) if text.text.contains("stuck-detection"))
    })
}

/// Audit C1: every proxied `tools/call` opens its own short-lived daemon MCP
/// session, so without a per-connection bucket key the daemon's burst window
/// started empty on every call and stuck/loop detection never fired. One
/// proxy connection must now share one burst window across its calls, and a
/// second proxy connection must not inherit it.
#[test]
fn stdio_proxy_calls_share_one_rate_limit_bucket_per_connection() {
    let temp = tempfile::tempdir().expect("tempdir");
    let tachi_home = temp.path().join("home");
    let global = tachi_home.join("global/memory.db");
    std::fs::create_dir_all(global.parent().expect("global parent")).expect("global parent");

    with_tachi_home(&tachi_home, || {
        test_runtime().block_on(async {
            let server = crate::MemoryServer::new(global.clone(), None).expect("daemon server");
            let (daemon, ct, daemon_task) = spawn_test_http_daemon(server, &global).await;
            let make_proxy = || StdioProxyServer {
                adapter_started_at: chrono::Utc::now(),
                tool_profile: None,
                daemon: std::sync::Arc::new(std::sync::RwLock::new(daemon.clone())),
                app_home: tachi_home.clone(),
                global_db_path: global.clone(),
                project_db_path: None,
                client_project: None,
                resolved_agent_identity: Default::default(),
                rate_limit_session: ProxyRateLimitSession::mint(),
            };
            let proxy_a = make_proxy();
            let proxy_b = make_proxy();
            assert_ne!(
                proxy_a.rate_limit_session.as_str(),
                proxy_b.rate_limit_session.as_str(),
                "each proxy connection mints its own bucket key"
            );
            let args = || stuck_probe_args("C1-STDIO-PROXY-BUCKET-PROBE");

            for call in 1..=2 {
                let result = call_tool_via_stdio_proxy(proxy_a.clone(), "tachi_memory", args())
                    .await
                    .unwrap_or_else(|err| panic!("proxy A call {call}: {err}"));
                assert_tool_ok(&result);
                assert!(
                    !has_stuck_warning(&result),
                    "call {call} is below the stuck threshold: {result:?}"
                );
            }

            let other = call_tool_via_stdio_proxy(proxy_b.clone(), "tachi_memory", args())
                .await
                .expect("proxy B call");
            assert_tool_ok(&other);
            assert!(
                !has_stuck_warning(&other),
                "a different proxy connection must not inherit proxy A's burst window: {other:?}"
            );

            let third = call_tool_via_stdio_proxy(proxy_a.clone(), "tachi_memory", args())
                .await
                .expect("proxy A call 3");
            assert_tool_ok(&third);
            assert!(
                has_stuck_warning(&third),
                "the third identical call through one proxy connection must share the burst \
                 window across its per-call daemon sessions: {third:?}"
            );

            ct.cancel();
            daemon_task.await.expect("daemon task");
        });
    });
}
