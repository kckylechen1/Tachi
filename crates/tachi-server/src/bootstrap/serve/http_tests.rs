mod memory_routing;
mod presence_reporting;
mod rmcp_compat;
mod vault_management;
mod workspace_context;
mod workspace_recall;

use super::*;
use crate::{mcp_connection::legacy_client_config, test_support::EnvRestore};
use tokio_util::sync::CancellationToken;

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
    spawn_test_http_daemon_on(
        server,
        global_db_path,
        client_observer,
        response_observer,
        0,
    )
    .await
}

async fn spawn_test_http_daemon_on(
    server: crate::MemoryServer,
    global_db_path: &Path,
    client_observer: Option<ClientHeaderObservations>,
    response_observer: Option<ToolResponseObservations>,
    port: u16,
) -> (
    crate::cli_client::DaemonInfo,
    CancellationToken,
    tokio::task::JoinHandle<()>,
) {
    let listener = tokio::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, port))
        .await
        .expect("bind daemon listener");
    let local_addr = listener.local_addr().expect("local addr");
    let ct = CancellationToken::new();
    let ct_shutdown = ct.clone();
    let internal_proxy_token = uuid::Uuid::new_v4().simple().to_string();
    server.set_daemon_proxy_token(internal_proxy_token.clone());

    let router =
        crate::bootstrap::daemon_http_router(server, ct.clone()).layer(axum::middleware::from_fn(
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
                                value["action"]
                                    .as_str()
                                    .map(|action| format!("owner:{action}"))
                                    .or_else(|| value["params"]["name"].as_str().map(str::to_owned))
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
                message.contains("Ops/Admin selectors are retired"),
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
                .is_some_and(|message| message.contains("Ops/Admin selectors are retired")),
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
        let expected_core = std::collections::BTreeSet::from([
            "tachi_a2a",
            "tachi_memory",
            "tachi_staff",
            "tachi_task",
            "vault_connections",
        ]);
        let expected_standard = std::collections::BTreeSet::from([
            "tachi_a2a",
            "tachi_agent_eval",
            "tachi_memory",
            "tachi_staff",
            "tachi_task",
            "vault_connections",
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
                &expected_core
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

/// #1061: HTTP writes normalize equivalent aliases to the same canonical DB.
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

#[derive(Clone)]
struct HttpTestClient {
    daemon: crate::cli_client::DaemonInfo,
    tool_profile: Option<tachi_hub::ToolProfile>,
    client_project: Option<String>,
}

async fn call_tool_over_http(
    client: HttpTestClient,
    tool_name: &str,
    arguments: serde_json::Map<String, serde_json::Value>,
) -> Result<rmcp::model::CallToolResult, crate::cli_client::DaemonCallError> {
    crate::cli_client::call_daemon_tool_raw_with_access(
        &client.daemon,
        rmcp::model::CallToolRequestParams::new(tool_name.to_string()).with_arguments(arguments),
        client.client_project.as_deref(),
        match client.tool_profile {
            Some(profile) if profile == tachi_hub::ToolProfile::admin() => {
                crate::cli_client::DaemonToolAccess::Owner
            }
            profile => crate::cli_client::DaemonToolAccess::Agent(profile),
        },
    )
    .await
}

async fn http_responses(
    daemon: crate::cli_client::DaemonInfo,
    requests: &[serde_json::Value],
) -> Vec<serde_json::Value> {
    let client = reqwest::Client::new();
    let mut session = reqwest::header::HeaderMap::new();
    let mut responses = Vec::new();
    for request in requests {
        let method = request["method"].as_str().expect("method");
        let mut headers = session.clone();
        if request
            .pointer("/params/_meta/io.modelcontextprotocol~1protocolVersion")
            .is_some()
        {
            headers = http_headers(&[
                ("mcp-protocol-version", "2026-07-28"),
                ("mcp-method", method),
            ]);
            if let Some(name) = request
                .pointer(if method.starts_with("tasks/") {
                    "/params/taskId"
                } else {
                    "/params/name"
                })
                .and_then(serde_json::Value::as_str)
            {
                headers.insert("mcp-name", name.parse().unwrap());
            }
        } else if headers.is_empty() {
            headers = http_headers(&[]);
        }
        let response = client
            .post(&daemon.url)
            .headers(headers)
            .json(request)
            .send()
            .await
            .expect("HTTP request");
        if method == "initialize" {
            session = http_headers(&[]);
            if let Some(id) = response.headers().get("mcp-session-id") {
                session.insert("mcp-session-id", id.clone());
            }
        }
        if let Some(id) = request["id"].as_i64() {
            responses.push(parse_http_mcp_payload(
                &response.text().await.expect("HTTP body"),
                id,
            ));
        }
    }
    responses
}

fn memory_archived_value(db_path: &Path, id: &str) -> i64 {
    rusqlite::Connection::open(db_path)
        .expect("open db")
        .query_row("SELECT archived FROM memories WHERE id = ?1", [id], |row| {
            row.get(0)
        })
        .expect("archived value")
}
