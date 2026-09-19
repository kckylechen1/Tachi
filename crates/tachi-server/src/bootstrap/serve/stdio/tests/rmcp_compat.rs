//! Legacy wire boundaries retained across the RMCP 3.x SDK migration.
use super::*;
use serde_json::json;

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
                    "tachiAgentIdentity": "agent.legacy.meta",
                    "tachiProfile": "ops"
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
fn modern_http_requests_are_rejected_before_memory_write() {
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
                        "arguments": {"action":"save", "scope":"global", "id":"modern-blocked-write",
                            "text":"modern request must not dispatch", "summary":"protocol guard",
                            "path":"/tests/protocol", "category":"fact", "force":true}
                    }
                })).send().await.expect("modern request");
            let body = response.text().await.expect("response body");
            let payload = parse_http_mcp_payload(&body, 7);
            assert_eq!(payload["error"]["code"], -32022, "{payload:#}");
            assert!(payload.get("result").is_none(), "{payload:#}");
            assert_eq!(memory_id_count(&global, "modern-blocked-write"), 0);

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
                ("2026-07-28", "2025-11-25"),
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
fn stdio_auto_client_falls_back_to_legacy_initialize() {
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
            let client = client.expect("client fallback handshake");
            assert_eq!(
                client
                    .peer()
                    .peer_info()
                    .expect("server info")
                    .protocol_version,
                ProtocolVersion::V_2024_11_05
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
