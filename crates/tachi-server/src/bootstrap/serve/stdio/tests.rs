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
        move || Ok(server.clone()),
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
        .find_map(|content| match &content.raw {
            rmcp::model::RawContent::Text(text) => Some(text.text.clone()),
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
            daemon: std::sync::Arc::new(std::sync::RwLock::new(daemon)),
            app_home: tachi_home.clone(),
            global_db_path: global.clone(),
            project_db_path: Some(project.clone()),
            client_project: Some(project_name.to_string()),
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
            daemon: std::sync::Arc::new(std::sync::RwLock::new(daemon)),
            app_home: tachi_home.clone(),
            global_db_path: global.clone(),
            project_db_path: Some(project.clone()),
            client_project: Some(bound_name.clone()),
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
            daemon: std::sync::Arc::new(std::sync::RwLock::new(daemon)),
            app_home: tachi_home.clone(),
            global_db_path: global.clone(),
            project_db_path: Some(bound_project.clone()),
            client_project: Some(bound_project_name.to_string()),
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
            daemon: std::sync::Arc::new(std::sync::RwLock::new(daemon)),
            app_home: tachi_home.clone(),
            global_db_path: global.clone(),
            project_db_path: Some(project.clone()),
            client_project: Some(project_name.to_string()),
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
            daemon: daemon.clone(),
            app_home: tachi_home.clone(),
            global_db_path: global.clone(),
            project_db_path: Some(bound_project.clone()),
            client_project: Some(bound_project_name.to_string()),
        };
        let other_proxy = StdioProxyServer {
            adapter_started_at: chrono::Utc::now(),
            daemon,
            app_home: tachi_home.clone(),
            global_db_path: global.clone(),
            project_db_path: Some(other_project.clone()),
            client_project: Some(other_project_name.to_string()),
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
            daemon: std::sync::Arc::new(std::sync::RwLock::new(daemon)),
            app_home: tachi_home.clone(),
            global_db_path: global.clone(),
            project_db_path: Some(project.clone()),
            client_project: Some(project_name.to_string()),
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
        };
        let proxy = StdioProxyServer {
            adapter_started_at: chrono::Utc::now(),
            daemon: std::sync::Arc::new(std::sync::RwLock::new(stale_cached)),
            app_home: tachi_home.clone(),
            global_db_path: global.clone(),
            project_db_path: None,
            client_project: None,
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
        };
        let proxy = StdioProxyServer {
            adapter_started_at: chrono::Utc::now(),
            daemon: std::sync::Arc::new(std::sync::RwLock::new(cached_but_dead)),
            app_home: tachi_home.clone(),
            global_db_path: global.clone(),
            project_db_path: None,
            client_project: None,
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
fn stdio_proxy_delete_and_archive_global_rows_with_bound_project() {
    let _guard = crate::utils::global_test_lock()
        .lock()
        .unwrap_or_else(|e| e.into_inner());

    let temp = tempfile::tempdir().expect("tempdir");
    let tachi_home = temp.path().join("home");
    let global = tachi_home.join("global/memory.db");
    let project_name = "Sigil-proxy-delete-e2e";
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
            daemon: std::sync::Arc::new(std::sync::RwLock::new(daemon)),
            app_home: tachi_home.clone(),
            global_db_path: global.clone(),
            project_db_path: Some(project.clone()),
            client_project: Some(project_name.to_string()),
        };

        for (id, summary) in [
            ("global-proxy-delete-e2e", "global delete fallback row"),
            ("global-proxy-archive-e2e", "global archive fallback row"),
        ] {
            let result = call_tool_via_stdio_proxy(
                proxy.clone(),
                "tachi_memory",
                serde_json::Map::from_iter([
                    ("action".to_string(), serde_json::json!("save")),
                    ("id".to_string(), serde_json::json!(id)),
                    (
                        "text".to_string(),
                        serde_json::json!(format!("{summary} PROXYMUTATEGLOBAL")),
                    ),
                    ("summary".to_string(), serde_json::json!(summary)),
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
            .unwrap_or_else(|err| panic!("seed {id}: {err}"));
            assert_tool_ok(&result);
        }

        let deleted = call_tool_via_stdio_proxy(
            proxy.clone(),
            "tachi_memory",
            serde_json::Map::from_iter([
                ("action".to_string(), serde_json::json!("delete")),
                (
                    "id".to_string(),
                    serde_json::json!("global-proxy-delete-e2e"),
                ),
            ]),
        )
        .await
        .expect("proxied delete should succeed");
        assert_tool_ok(&deleted);
        let deleted = first_text_json(&deleted);
        assert_eq!(deleted["deleted"], serde_json::json!(true));
        assert_eq!(deleted["db"], serde_json::json!("global"));

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

    assert_eq!(memory_id_count(&global, "global-proxy-delete-e2e"), 0);
    assert_eq!(
        memory_archived_value(&global, "global-proxy-archive-e2e"),
        1
    );
    assert_eq!(memory_id_count(&project, "global-proxy-delete-e2e"), 0);
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
fn proxy_maps_zero_arg_briefing_to_project_memory_briefing() {
    let request = rmcp::model::CallToolRequestParams::new("tachi_briefing");

    let mapped = prepare_proxy_tool_call(request, Some("Sigil-abc123")).expect("mapped");

    assert_eq!(mapped.name.as_ref(), "tachi_memory");
    let args = mapped.arguments.expect("briefing args");
    assert_eq!(args["action"], serde_json::json!("briefing"));
    assert_eq!(args["format"], serde_json::json!("markdown"));
    assert_eq!(args["compact"], serde_json::json!(true));
    assert_eq!(args["project"], serde_json::json!("Sigil-abc123"));
    assert!(args["query"]
        .as_str()
        .expect("query")
        .contains("Sigil-abc123"));
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

    for action in [
        "consolidate",
        "pattern_feedback",
        // #757 fold: delete/ingest/ingest_source now default to bound project
        // (previously standalone delete_memory/ingest/ingest_source did).
        "delete",
        "ingest",
        "ingest_source",
    ] {
        let request = rmcp::model::CallToolRequestParams::new("tachi_memory").with_arguments(
            serde_json::Map::from_iter([("action".to_string(), serde_json::json!(action))]),
        );
        let mapped = prepare_proxy_tool_call(request, Some("Sigil-abc123")).expect("action mapped");
        assert!(
            !mapped.arguments.expect("args").contains_key("project"),
            "{action} preflight must not inject a default project"
        );
    }
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
        ("tachi_memory", Some("delete")),
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
            ("tachi_memory", Some("readiness")),
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
            (crate::session_identity::HEADER_PROFILE, "delegate"),
            (crate::session_identity::HEADER_CLIENT, "codex-http-test"),
            (crate::session_identity::HEADER_PROJECT, project_name),
        ]);
        let (client, session_headers, init) = http_mcp_initialize(&daemon.url, headers, None).await;
        assert!(init.get("error").is_none(), "initialize failed: {init:#}");
        http_mcp_initialized(&client, &daemon.url, session_headers.clone()).await;

        let runtime = http_mcp_call_tool(
            &client,
            &daemon.url,
            session_headers.clone(),
            2,
            "runtime_info",
            serde_json::Map::new(),
        )
        .await;
        assert!(
            runtime["result"]["isError"] != serde_json::json!(true),
            "runtime_info failed: {runtime:#}"
        );
        let runtime_text = http_tool_text(&runtime);
        let runtime_json: serde_json::Value =
            serde_json::from_str(&runtime_text).expect("runtime_info JSON");
        assert_eq!(runtime_json["runtime"]["tool_profile"], "delegate");
        assert_eq!(runtime_json["runtime"]["session_client"], "codex-http-test");
        assert_eq!(runtime_json["runtime"]["session_project"], project_name);

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
                if scope == "global" { 3 } else { 4 },
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
            session_headers,
            5,
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
fn http_direct_connect_rejects_admin_profile_without_authorization_policy() {
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
        let headers = http_headers(&[(crate::session_identity::HEADER_PROFILE, "admin")]);
        let (_client, _session_headers, init) =
            http_mcp_initialize(&daemon.url, headers, None).await;
        let error = init
            .get("error")
            .unwrap_or_else(|| panic!("admin initialize should fail: {init:#}"));
        let message = error["message"].as_str().unwrap_or_default();
        assert!(
            message.contains("requires explicit authorization"),
            "unexpected admin rejection: {init:#}"
        );
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
        let headers =
            http_headers(&[(crate::session_identity::HEADER_PROJECT, bound_name.as_str())]);
        let (client, session_headers, init) = http_mcp_initialize(&daemon.url, headers, None).await;
        assert!(init.get("error").is_none(), "initialize failed: {init:#}");
        http_mcp_initialized(&client, &daemon.url, session_headers.clone()).await;

        let save = http_mcp_call_tool(
            &client,
            &daemon.url,
            session_headers.clone(),
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

        let runtime = http_mcp_call_tool(
            &client,
            &daemon.url,
            session_headers,
            3,
            "runtime_info",
            serde_json::Map::new(),
        )
        .await;
        let runtime_json: serde_json::Value =
            serde_json::from_str(&http_tool_text(&runtime)).expect("runtime_info JSON");
        assert_eq!(
            runtime_json["runtime"]["session_project"],
            serde_json::json!(&bound_name),
            "immutable HTTP binding must remain the legacy identity"
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
