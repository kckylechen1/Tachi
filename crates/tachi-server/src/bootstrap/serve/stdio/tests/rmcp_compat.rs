//! Dual-era wire conformance for the RMCP 3.3 protocol adapter.
use super::*;
use serde_json::json;

fn modern_meta(extra: serde_json::Value) -> serde_json::Value {
    let mut meta = serde_json::Map::from_iter([
        (
            "io.modelcontextprotocol/protocolVersion".to_string(),
            json!("2026-07-28"),
        ),
        (
            "io.modelcontextprotocol/clientCapabilities".to_string(),
            json!({}),
        ),
        (
            "io.modelcontextprotocol/clientInfo".to_string(),
            json!({"name":"tachi-conformance", "version":"1"}),
        ),
    ]);
    if let Some(extra) = extra.as_object() {
        meta.extend(extra.clone());
    }
    serde_json::Value::Object(meta)
}

async fn stdio_first_response(request: serde_json::Value) -> serde_json::Value {
    stdio_responses(identity_probe_proxy(), &[request])
        .await
        .into_iter()
        .next()
        .expect("one stdio response")
}

async fn stdio_responses(
    proxy: StdioProxyServer,
    requests: &[serde_json::Value],
) -> Vec<serde_json::Value> {
    use rmcp::ServiceExt;
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

    let (server_io, client_io) = tokio::io::duplex(32768);
    let server = tokio::spawn(async move {
        match proxy.serve(server_io).await {
            Ok(service) => {
                let _ = service.waiting().await;
            }
            Err(_expected_for_rejected_first_request) => {}
        }
    });
    let (reader, mut writer) = tokio::io::split(client_io);
    let mut reader = BufReader::new(reader);
    let mut responses = Vec::with_capacity(requests.len());
    for request in requests {
        writer
            .write_all(format!("{request}\n").as_bytes())
            .await
            .expect("write stdio request");
        let mut line = String::new();
        tokio::time::timeout(
            std::time::Duration::from_secs(30),
            reader.read_line(&mut line),
        )
        .await
        .expect("stdio response deadline")
        .expect("read stdio response");
        responses.push(serde_json::from_str(&line).expect("stdio response JSON"));
    }
    drop(reader);
    drop(writer);
    tokio::time::timeout(std::time::Duration::from_secs(30), server)
        .await
        .expect("stdio server exit deadline")
        .expect("stdio server task");
    responses
}

#[test]
fn legacy_http_initialize_meta_survives_wire_dispatch() {
    let temp = tempfile::tempdir().expect("tempdir");
    with_tachi_home(temp.path(), || {
        let global = temp.path().join("global/memory.db");
        let project_name = "legacy-meta-project";
        seed_identity_db(temp.path(), project_name);
        test_runtime().block_on(async {
            let server = crate::MemoryServer::new(global.clone(), None).expect("server");
            let (daemon, cancel, task) = spawn_test_http_daemon(server, &global).await;
            let (client, headers, init) = http_mcp_initialize(
                &daemon.url,
                http_headers(&[]),
                Some(json!({
                    "tachiClient": "legacy-meta-client",
                    "tachiProject": project_name,
                    "tachiAgentIdentity": "agent.legacy.meta"
                })),
            )
            .await;
            assert!(init.get("error").is_none(), "{init:#}");
            assert_eq!(init["result"]["protocolVersion"], "2024-11-05");
            assert!(headers.contains_key("mcp-session-id"));
            http_mcp_initialized(&client, &daemon.url, headers.clone()).await;
            let runtime = http_mcp_call_tool(
                &client,
                &daemon.url,
                headers.clone(),
                2,
                "runtime_info",
                serde_json::Map::new(),
            )
            .await;
            assert!(runtime["result"].get("resultType").is_none(), "{runtime:#}");
            let body: serde_json::Value =
                serde_json::from_str(&http_tool_text(&runtime)).expect("runtime JSON");
            assert_eq!(body["runtime"]["session_client"], "legacy-meta-client");
            assert_eq!(body["runtime"]["session_project"], project_name);
            let a2a = http_mcp_call_tool(
                &client,
                &daemon.url,
                headers,
                3,
                "tachi_a2a",
                serde_json::Map::from_iter([("action".into(), json!("status"))]),
            )
            .await;
            let body: serde_json::Value =
                serde_json::from_str(&http_tool_text(&a2a)).expect("A2A JSON");
            assert_eq!(body["actor_agent_identity_id"], "agent.legacy.meta");
            assert_eq!(body["status"], "completed");

            // Present-but-malformed wire metadata must not become absence.
            for project in [json!(""), json!("   "), json!(0), json!(null)] {
                let (_, _, init) = http_mcp_initialize(
                    &daemon.url,
                    http_headers(&[]),
                    Some(json!({"tachiProject": project})),
                )
                .await;
                assert!(
                    init.get("error").is_some(),
                    "malformed project admitted: {init:#}"
                );
            }
            cancel.cancel();
            task.await.expect("daemon task");
        });
    });
}

#[test]
fn dual_era_conformance_matrix_covers_stdio_and_http() {
    let temp = tempfile::tempdir().expect("tempdir");
    with_tachi_home(temp.path(), || {
        let global = temp.path().join("global/memory.db");
        test_runtime().block_on(async {
            let server = crate::MemoryServer::new(global.clone(), None).expect("server");
            let (daemon, cancel, task) = spawn_test_http_daemon(server, &global).await;
            let client = reqwest::Client::new();

            // HTTP legacy: exact initialization version and a real session.
            let (_, legacy_headers, legacy) =
                http_mcp_initialize(&daemon.url, http_headers(&[]), None).await;
            assert_eq!(legacy["result"]["protocolVersion"], "2024-11-05");
            assert!(legacy_headers.contains_key("mcp-session-id"));

            // HTTP modern discovery: typed 2026 result, capabilities, no session.
            let discovery = client
                .post(&daemon.url)
                .headers(http_headers(&[
                    ("mcp-protocol-version", "2026-07-28"),
                    ("mcp-method", "server/discover"),
                ]))
                .json(&json!({
                    "jsonrpc":"2.0", "id":20, "method":"server/discover",
                    "params":{"_meta":modern_meta(json!({}))}
                }))
                .send()
                .await
                .expect("HTTP discovery");
            assert!(!discovery.headers().contains_key("mcp-session-id"));
            let discovery = parse_http_mcp_payload(
                &discovery.text().await.expect("discovery body"),
                20,
            );
            assert_eq!(discovery["result"]["resultType"], "complete");
            assert!(discovery["result"]["supportedVersions"]
                .as_array()
                .expect("supported versions")
                .contains(&json!("2026-07-28")));
            assert!(discovery["result"]["capabilities"]["tools"].is_object());

            // HTTP partial modern context fails before application dispatch.
            let partial = client
                .post(&daemon.url)
                .headers(http_headers(&[
                    ("mcp-protocol-version", "2026-07-28"),
                    ("mcp-method", "tools/list"),
                ]))
                .json(&json!({
                    "jsonrpc":"2.0", "id":21, "method":"tools/list",
                    "params":{"_meta":{
                        "io.modelcontextprotocol/protocolVersion":"2026-07-28"
                    }}
                }))
                .send()
                .await
                .expect("partial HTTP request");
            assert_eq!(partial.status(), reqwest::StatusCode::BAD_REQUEST);
            let partial = parse_http_mcp_payload(&partial.text().await.expect("partial body"), 21);
            assert_eq!(partial["error"]["code"], -32602, "{partial:#}");

            // Explicitly naming modern through the removed initialize lifecycle
            // is a typed failure, never a 2025-11-25 success response.
            let modern_initialize = client
                .post(&daemon.url)
                .headers(http_headers(&[(
                    "mcp-protocol-version",
                    "2026-07-28",
                )]))
                .json(&json!({
                    "jsonrpc":"2.0", "id":28, "method":"initialize", "params":{
                        "protocolVersion":"2026-07-28", "capabilities":{},
                        "clientInfo":{"name":"wrong-http-lifecycle", "version":"1"}
                    }
                }))
                .send()
                .await
                .expect("modern initialize");
            let modern_initialize = parse_http_mcp_payload(
                &modern_initialize.text().await.expect("modern initialize body"),
                28,
            );
            assert_eq!(modern_initialize["error"]["code"], -32022);
            assert!(modern_initialize.get("result").is_none());

            // HTTP version and routing identity conflicts are transport errors.
            let version_conflict = client
                .post(&daemon.url)
                .headers(http_headers(&[
                    ("mcp-protocol-version", "2026-07-28"),
                    ("mcp-method", "tools/list"),
                ]))
                .json(&json!({
                    "jsonrpc":"2.0", "id":22, "method":"tools/list",
                    "params":{"_meta":{
                        "io.modelcontextprotocol/protocolVersion":"2025-11-25",
                        "io.modelcontextprotocol/clientCapabilities":{}
                    }}
                }))
                .send()
                .await
                .expect("version conflict");
            assert_eq!(version_conflict.status(), reqwest::StatusCode::BAD_REQUEST);
            let version_conflict = parse_http_mcp_payload(
                &version_conflict.text().await.expect("conflict body"),
                22,
            );
            assert_eq!(version_conflict["error"]["code"], -32020);

            let route_conflict = client
                .post(&daemon.url)
                .headers(http_headers(&[
                    ("mcp-protocol-version", "2026-07-28"),
                    ("mcp-method", "tools/call"),
                    ("mcp-name", "wrong_tool"),
                ]))
                .json(&json!({
                    "jsonrpc":"2.0", "id":23, "method":"tools/call",
                    "params":{"_meta":modern_meta(json!({})), "name":"runtime_info", "arguments":{}}
                }))
                .send()
                .await
                .expect("route conflict");
            assert_eq!(route_conflict.status(), reqwest::StatusCode::BAD_REQUEST);
            let route_conflict = parse_http_mcp_payload(
                &route_conflict.text().await.expect("route conflict body"),
                23,
            );
            assert_eq!(route_conflict["error"]["code"], -32020);

            // Tachi header/body identity conflicts also fail before the tool can write.
            let identity_conflict = client
                .post(&daemon.url)
                .headers(http_headers(&[
                    ("mcp-protocol-version", "2026-07-28"),
                    ("mcp-method", "tools/call"),
                    ("mcp-name", "tachi_memory"),
                    ("x-tachi-client", "header-client"),
                ]))
                .json(&json!({
                    "jsonrpc":"2.0", "id":24, "method":"tools/call",
                    "params":{
                        "_meta":modern_meta(json!({"tachiClient":"body-client"})),
                        "name":"tachi_memory",
                        "arguments":{"action":"save", "scope":"global", "id":"identity-conflict-write",
                            "text":"must not dispatch", "summary":"identity conflict",
                            "path":"/tests/protocol", "category":"fact", "force":true}
                    }
                }))
                .send()
                .await
                .expect("identity conflict");
            let identity_conflict = parse_http_mcp_payload(
                &identity_conflict.text().await.expect("identity conflict body"),
                24,
            );
            assert_eq!(identity_conflict["error"]["code"], -32020);
            assert_eq!(memory_id_count(&global, "identity-conflict-write"), 0);

            // Modern identity is request-scoped; it cannot become session authority.
            let first_runtime = client
                .post(&daemon.url)
                .headers(http_headers(&[
                    ("mcp-protocol-version", "2026-07-28"),
                    ("mcp-method", "tools/call"),
                    ("mcp-name", "runtime_info"),
                ]))
                .json(&json!({
                    "jsonrpc":"2.0", "id":25, "method":"tools/call",
                    "params":{"_meta":modern_meta(json!({"tachiClient":"request-a"})),
                        "name":"runtime_info", "arguments":{}}
                }))
                .send().await.expect("first runtime");
            let first_runtime = parse_http_mcp_payload(
                &first_runtime.text().await.expect("first runtime body"),
                25,
            );
            let first_runtime: serde_json::Value =
                serde_json::from_str(&http_tool_text(&first_runtime)).expect("runtime JSON");
            assert_eq!(first_runtime["runtime"]["session_client"], "request-a");

            let second_runtime = client
                .post(&daemon.url)
                .headers(http_headers(&[
                    ("mcp-protocol-version", "2026-07-28"),
                    ("mcp-method", "tools/call"),
                    ("mcp-name", "runtime_info"),
                ]))
                .json(&json!({
                    "jsonrpc":"2.0", "id":26, "method":"tools/call",
                    "params":{"_meta":modern_meta(json!({})), "name":"runtime_info", "arguments":{}}
                }))
                .send().await.expect("second runtime");
            let second_runtime = parse_http_mcp_payload(
                &second_runtime.text().await.expect("second runtime body"),
                26,
            );
            let second_runtime: serde_json::Value =
                serde_json::from_str(&http_tool_text(&second_runtime)).expect("runtime JSON");
            assert!(second_runtime["runtime"]["session_client"].is_null());

            // A legacy-only method cannot borrow modern request authority.
            let legacy_route = client
                .post(&daemon.url)
                .headers(http_headers(&[
                    ("mcp-protocol-version", "2026-07-28"),
                    ("mcp-method", "resources/subscribe"),
                ]))
                .json(&json!({
                    "jsonrpc":"2.0", "id":27, "method":"resources/subscribe",
                    "params":{"_meta":modern_meta(json!({})), "uri":"tachi://legacy-only"}
                }))
                .send().await.expect("legacy-only route");
            let legacy_route = parse_http_mcp_payload(
                &legacy_route.text().await.expect("legacy route body"),
                27,
            );
            assert_eq!(legacy_route["error"]["code"], -32601);

            // Stdio legacy, modern, partial, and explicit-modern-initialize rows.
            let stdio_legacy = stdio_first_response(json!({
                "jsonrpc":"2.0", "id":30, "method":"initialize", "params":{
                    "protocolVersion":"2025-11-25", "capabilities":{},
                    "clientInfo":{"name":"legacy-matrix", "version":"1"}
                }
            })).await;
            assert_eq!(stdio_legacy["result"]["protocolVersion"], "2025-11-25");

            let stdio_modern = stdio_first_response(json!({
                "jsonrpc":"2.0", "id":31, "method":"server/discover",
                "params":{"_meta":modern_meta(json!({}))}
            })).await;
            assert_eq!(stdio_modern["result"]["resultType"], "complete");
            assert!(stdio_modern["result"]["supportedVersions"]
                .as_array().expect("stdio versions")
                .contains(&json!("2026-07-28")));

            let stdio_partial = stdio_first_response(json!({
                "jsonrpc":"2.0", "id":32, "method":"server/discover",
                "params":{"_meta":{
                    "io.modelcontextprotocol/protocolVersion":"2026-07-28"
                }}
            })).await;
            assert_eq!(stdio_partial["error"]["code"], -32602);

            let stdio_modern_initialize = stdio_first_response(json!({
                "jsonrpc":"2.0", "id":33, "method":"initialize", "params":{
                    "protocolVersion":"2026-07-28", "capabilities":{},
                    "clientInfo":{"name":"wrong-lifecycle", "version":"1"}
                }
            })).await;
            assert_eq!(stdio_modern_initialize["error"]["code"], -32022);

            // Stdio modern identity is selected per request. Even if the
            // adapter carries a legacy initialize identity, modern requests
            // use their own metadata without promoting either identity into
            // the protocol session.
            let proxy = identity_probe_proxy();
            *proxy.daemon.write().expect("proxy daemon lock") = daemon.clone();
            *proxy
                .resolved_agent_identity
                .lock()
                .expect("proxy identity lock") = Some(
                crate::cli_client::ProxyIdentityForward::Header(
                    "agent.legacy-session".to_string(),
                ),
            );
            let observer = proxy.clone();
            let stdio_identity = stdio_responses(
                proxy,
                &[
                    json!({
                        "jsonrpc":"2.0", "id":34, "method":"tools/call", "params":{
                            "_meta":modern_meta(json!({"tachiAgentIdentity":"agent.request-a"})),
                            "name":"tachi_a2a", "arguments":{"action":"status"}
                        }
                    }),
                    json!({
                        "jsonrpc":"2.0", "id":35, "method":"tools/call", "params":{
                            "_meta":modern_meta(json!({"tachiAgentIdentity":"agent.request-b"})),
                            "name":"tachi_a2a", "arguments":{"action":"status"}
                        }
                    }),
                ],
            )
            .await;
            for (response, expected) in stdio_identity
                .iter()
                .zip(["agent.request-a", "agent.request-b"])
            {
                assert_eq!(response["result"]["resultType"], "complete", "{response:#}");
                let body: serde_json::Value = serde_json::from_str(&http_tool_text(response))
                    .expect("stdio A2A status JSON");
                assert_eq!(body["actor_agent_identity_id"], expected, "{body:#}");
            }
            assert_eq!(
                observer.forwarded_agent_identity(),
                crate::cli_client::ProxyIdentityForward::Header(
                    "agent.legacy-session".to_string()
                ),
                "modern request identity must not replace legacy session authority"
            );

            cancel.cancel();
            task.await.expect("daemon task");
        });
    });
}

#[test]
fn modern_http_request_uses_2026_headers_metadata_and_no_session() {
    let temp = tempfile::tempdir().expect("tempdir");
    with_tachi_home(temp.path(), || {
        let global = temp.path().join("global/memory.db");
        test_runtime().block_on(async {
            let server = crate::MemoryServer::new(global.clone(), None).expect("server");
            let (daemon, cancel, task) = spawn_test_http_daemon(server, &global).await;
            let client = reqwest::Client::new();
            let response = client.post(&daemon.url)
                .headers(http_headers(&[("mcp-protocol-version", "2026-07-28"), ("mcp-method", "tools/call"), ("mcp-name", "tachi_memory")]))
                .json(&json!({
                    "jsonrpc": "2.0", "id": 7, "method": "tools/call",
                    "params": {
                        "_meta": {
                            "io.modelcontextprotocol/protocolVersion": "2026-07-28",
                            "io.modelcontextprotocol/clientCapabilities": {},
                            "io.modelcontextprotocol/clientInfo": {"name":"modern-probe", "version":"1"}
                        },
                        "name": "tachi_memory",
                        "arguments": {"action":"save", "scope":"global", "id":"modern-write",
                            "text":"modern request dispatches without a legacy session", "summary":"protocol conformance",
                            "path":"/tests/protocol", "category":"fact", "force":true}
                    }
                })).send().await.expect("modern request");
            assert!(
                !response.headers().contains_key("mcp-session-id"),
                "modern requests must not mint legacy sessions"
            );
            let body = response.text().await.expect("response body");
            let payload = parse_http_mcp_payload(&body, 7);
            assert!(payload.get("error").is_none(), "{payload:#}");
            assert_eq!(payload["result"]["resultType"], "complete", "{payload:#}");
            assert_eq!(memory_id_count(&global, "modern-write"), 1);

            // The same listener still accepts its legacy lifecycle after a
            // stateless modern request.
            let (_, headers, init) = http_mcp_initialize(&daemon.url, http_headers(&[]), None).await;
            assert_eq!(init["result"]["protocolVersion"], "2024-11-05");
            assert!(headers.contains_key("mcp-session-id"));
            cancel.cancel();
            task.await.expect("daemon task");
        });
    });
}

#[test]
fn inline_legacy_version_cannot_bypass_initialize() {
    let temp = tempfile::tempdir().expect("tempdir");
    with_tachi_home(temp.path(), || {
        let global = temp.path().join("global/memory.db");
        test_runtime().block_on(async {
            let server = crate::MemoryServer::new(global.clone(), None).expect("server");
            let (daemon, cancel, task) = spawn_test_http_daemon(server, &global).await;
            let client = reqwest::Client::new();
            let response = client.post(&daemon.url)
                .headers(http_headers(&[("mcp-protocol-version", "2025-11-25"), ("mcp-method", "tools/call"), ("mcp-name", "tachi_memory")]))
                .json(&json!({
                    "jsonrpc": "2.0", "id": 7, "method": "tools/call",
                    "params": {
                        "_meta": {
                            "io.modelcontextprotocol/protocolVersion": "2025-11-25",
                            "io.modelcontextprotocol/clientCapabilities": {},
                            "io.modelcontextprotocol/clientInfo": {"name":"modern-probe", "version":"1"}
                        },
                        "name": "tachi_memory",
                        "arguments": {"action":"save", "scope":"global", "id":"inline-legacy-blocked-write",
                            "text":"modern request must not dispatch", "summary":"protocol guard",
                            "path":"/tests/protocol", "category":"fact", "force":true}
                    }
                })).send().await.expect("modern request");
            let body = response.text().await.expect("response body");
            let payload = parse_http_mcp_payload(&body, 7);
            assert_eq!(payload["error"]["code"], -32600, "{payload:#}");
            assert!(payload.get("result").is_none(), "{payload:#}");
            assert_eq!(memory_id_count(&global, "inline-legacy-blocked-write"), 0);

            // The same listener still accepts its legacy lifecycle after rejection.
            let (_, headers, init) = http_mcp_initialize(&daemon.url, http_headers(&[]), None).await;
            assert_eq!(init["result"]["protocolVersion"], "2024-11-05");
            assert!(headers.contains_key("mcp-session-id"));
            cancel.cancel();
            task.await.expect("daemon task");
        });
    });
}

#[test]
fn legacy_stdio_wire_negotiates_and_preserves_explicit_identity() {
    use rmcp::ServiceExt;
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

    let temp = tempfile::tempdir().expect("tempdir");
    with_tachi_home(temp.path(), || {
        let _identity = EnvRestore::set(crate::session_identity::ENV_AGENT_IDENTITY, "agent.env");
        test_runtime().block_on(async {
            for (requested, expected) in [
                ("2024-11-05", "2024-11-05"), ("2025-03-26", "2025-03-26"),
                ("2025-06-18", "2025-06-18"), ("2025-11-25", "2025-11-25"),
            ] {
                for (meta, expected_identity) in [
                    (Some(json!({"tachiAgentIdentity":"agent.wire"})), crate::cli_client::ProxyIdentityForward::Header("agent.wire".into())),
                    (Some(json!({"tachiAgentIdentity":""})), crate::cli_client::ProxyIdentityForward::Omit),
                    (Some(json!({"tachiAgentIdentity":0})), crate::cli_client::ProxyIdentityForward::Omit),
                    (None, crate::cli_client::ProxyIdentityForward::Header("agent.env".into())),
                ] {
                    let proxy = identity_probe_proxy();
                    let observer = proxy.clone();
                    let (server_io, client_io) = tokio::io::duplex(16384);
                    let (reader, mut writer) = tokio::io::split(client_io);
                    let mut reader = BufReader::new(reader);
                    let client = async {
                        let mut params = json!({"protocolVersion":requested, "capabilities":{},
                            "clientInfo":{"name":"legacy-wire", "version":"1"}});
                        if let Some(meta) = meta { params["_meta"] = meta; }
                        let request = json!({"jsonrpc":"2.0", "id":1, "method":"initialize", "params":params});
                        writer.write_all(format!("{request}\n").as_bytes()).await.expect("write init");
                        let mut line = String::new();
                        reader.read_line(&mut line).await.expect("read initialize");
                        let response: serde_json::Value = serde_json::from_str(&line).expect("initialize JSON");
                        assert_eq!(response["result"]["protocolVersion"], expected, "{response:#}");
                        writer.write_all(b"{\"jsonrpc\":\"2.0\",\"method\":\"notifications/initialized\"}\n")
                            .await.expect("initialized notification");
                        (reader, writer)
                    };
                    let (service, _client_io) = tokio::time::timeout(std::time::Duration::from_secs(30), async {
                        tokio::join!(proxy.serve(server_io), client)
                    }).await.expect("stdio handshake deadline");
                    let service = service.expect("stdio handshake");
                    assert_eq!(observer.forwarded_agent_identity(), expected_identity);
                    service.cancel().await.expect("cancel stdio service");
                }
            }
        });
    });
}

#[test]
fn stdio_auto_client_negotiates_modern_discovery_without_initialized() {
    use rmcp::model::ProtocolVersion;
    use rmcp::service::{ClientLifecycleMode, ClientServiceExt};
    use rmcp::ServiceExt;
    let temp = tempfile::tempdir().expect("tempdir");
    with_tachi_home(temp.path(), || {
        test_runtime().block_on(async {
            let proxy = identity_probe_proxy();
            let (server_io, client_io) = tokio::io::duplex(16384);
            let (server, client) =
                tokio::time::timeout(std::time::Duration::from_secs(30), async {
                    tokio::join!(
                        proxy.serve(server_io),
                        ().serve_with_lifecycle(
                            client_io,
                            ClientLifecycleMode::Auto {
                                preferred_versions: vec![
                                    ProtocolVersion::V_2026_07_28,
                                    ProtocolVersion::V_2025_11_25
                                ],
                                legacy_version: Some(ProtocolVersion::V_2024_11_05),
                            }
                        )
                    )
                })
                .await
                .expect("Auto fallback deadline");
            let server = server.expect("server handshake");
            let client = client.expect("client modern handshake");
            assert_eq!(
                client
                    .peer()
                    .peer_info()
                    .expect("server info")
                    .protocol_version,
                ProtocolVersion::V_2026_07_28
            );
            client.cancel().await.expect("cancel client");
            server.cancel().await.expect("cancel server");
        });
    });
}

fn default_legacy_methods() -> [(&'static str, serde_json::Value, &'static str); 4] {
    [
        ("prompts/list", json!({}), "prompts"),
        ("resources/list", json!({}), "resources"),
        ("resources/templates/list", json!({}), "resourceTemplates"),
        (
            "completion/complete",
            json!({
                "ref": {"type":"ref/prompt", "name":"legacy-probe"},
                "argument": {"name":"query", "value":""}
            }),
            "completion",
        ),
    ]
}

fn inline_metadata() -> serde_json::Value {
    json!({
        "io.modelcontextprotocol/protocolVersion":"2025-11-25",
        "io.modelcontextprotocol/clientCapabilities":{},
        "io.modelcontextprotocol/clientInfo":{"name":"inline-probe", "version":"1"}
    })
}

#[test]
fn inherited_http_methods_require_initialize_and_preserve_legacy_results() {
    let temp = tempfile::tempdir().expect("tempdir");
    with_tachi_home(temp.path(), || {
        let global = temp.path().join("global/memory.db");
        test_runtime().block_on(async {
            let server = crate::MemoryServer::new(global.clone(), None).expect("server");
            let (daemon, cancel, task) = spawn_test_http_daemon(server, &global).await;
            let (client, session_headers, _) =
                http_mcp_initialize(&daemon.url, http_headers(&[]), None).await;
            http_mcp_initialized(&client, &daemon.url, session_headers.clone()).await;
            for (method, params, field) in default_legacy_methods() {
                let mut inline = params.clone();
                inline["_meta"] = inline_metadata();
                let response = client
                    .post(&daemon.url)
                    .headers(http_headers(&[
                        ("mcp-protocol-version", "2025-11-25"),
                        ("mcp-method", method),
                    ]))
                    .json(&json!({"jsonrpc":"2.0", "id":8, "method":method, "params":inline}))
                    .send()
                    .await
                    .expect("inline request");
                let body = parse_http_mcp_payload(&response.text().await.expect("body"), 8);
                assert_eq!(body["error"]["code"], -32600, "{method}: {body:#}");
                assert!(body.get("result").is_none(), "{method}: {body:#}");
                let response = client
                    .post(&daemon.url)
                    .headers(session_headers.clone())
                    .json(&json!({"jsonrpc":"2.0", "id":9, "method":method, "params":params}))
                    .send()
                    .await
                    .expect("legacy request");
                let body = parse_http_mcp_payload(&response.text().await.expect("body"), 9);
                assert!(body.get("error").is_none(), "{method}: {body:#}");
                assert!(body["result"].get("resultType").is_none(), "{body:#}");
                let value = if field == "completion" {
                    &body["result"][field]["values"]
                } else {
                    &body["result"][field]
                };
                assert_eq!(value, &json!([]), "{method}: {body:#}");
            }
            cancel.cancel();
            task.await.expect("daemon task");
        });
    });
}

#[test]
fn inherited_stdio_proxy_methods_reject_inline_wire_requests() {
    use rmcp::ServiceExt;
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
    let temp = tempfile::tempdir().expect("tempdir");
    with_tachi_home(temp.path(), || {
        test_runtime().block_on(async {
            for (method, mut params, _) in default_legacy_methods() {
                let proxy = identity_probe_proxy();
                let (server_io, client_io) = tokio::io::duplex(16384);
                let server = tokio::spawn(async move {
                    if let Ok(service) = proxy.serve(server_io).await {
                        let _ = service.waiting().await;
                    }
                });
                let (reader, mut writer) = tokio::io::split(client_io);
                let mut reader = BufReader::new(reader);
                params["_meta"] = inline_metadata();
                let request = json!({"jsonrpc":"2.0", "id":10, "method":method, "params":params});
                writer
                    .write_all(format!("{request}\n").as_bytes())
                    .await
                    .expect("write request");
                let mut line = String::new();
                tokio::time::timeout(
                    std::time::Duration::from_secs(30),
                    reader.read_line(&mut line),
                )
                .await
                .expect("response deadline")
                .expect("read response");
                let body: serde_json::Value = serde_json::from_str(&line).expect("wire JSON");
                assert_eq!(body["error"]["code"], -32600, "{method}: {body:#}");
                assert!(body.get("result").is_none(), "{method}: {body:#}");
                drop(reader);
                drop(writer);
                tokio::time::timeout(std::time::Duration::from_secs(30), server)
                    .await
                    .expect("server exit deadline")
                    .expect("server exit");
            }
        });
    });
}
