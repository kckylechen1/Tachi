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
    handler: impl rmcp::ServerHandler + 'static,
    requests: &[serde_json::Value],
) -> Vec<serde_json::Value> {
    use rmcp::ServiceExt;
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

    let (server_io, client_io) = tokio::io::duplex(32768);
    let server = tokio::spawn(async move {
        match handler.serve(server_io).await {
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
        if request.get("id").is_none() {
            continue;
        }
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

async fn modern_http_tool_call(
    client: &reqwest::Client,
    url: &str,
    id: i64,
    name: &str,
    identity: serde_json::Value,
    arguments: serde_json::Value,
) -> serde_json::Value {
    let response = client
        .post(url)
        .headers(http_headers(&[
            ("mcp-protocol-version", "2026-07-28"),
            ("mcp-method", "tools/call"),
            ("mcp-name", name),
        ]))
        .json(&json!({
            "jsonrpc":"2.0", "id":id, "method":"tools/call",
            "params":{"_meta":modern_meta(identity), "name":name, "arguments":arguments}
        }))
        .send()
        .await
        .expect("modern HTTP tool call");
    assert!(!response.headers().contains_key("mcp-session-id"));
    parse_http_mcp_payload(&response.text().await.expect("modern HTTP body"), id)
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
                crate::cli_client::ProxyIdentityForward::Header("agent.legacy-session".to_string()),
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
fn modern_stdio_rejects_identity_drift_and_malformed_metadata_before_dispatch() {
    let temp = tempfile::tempdir().expect("tempdir");
    with_tachi_home(temp.path(), || {
        let global = temp.path().join("global/memory.db");
        let project_a_name = "stdio-modern-project-a";
        let project_b_name = "stdio-modern-project-b";
        let project_a = temp
            .path()
            .join("projects")
            .join(project_a_name)
            .join("memory.db");
        let project_b = temp
            .path()
            .join("projects")
            .join(project_b_name)
            .join("memory.db");
        seed_project_db(temp.path(), &project_a);
        seed_project_db(temp.path(), &project_b);
        let _agent_env = EnvRestore::remove(crate::session_identity::ENV_AGENT_IDENTITY);

        test_runtime().block_on(async {
            let server = crate::MemoryServer::new(global.clone(), None).expect("server");
            let (daemon, cancel, task) = spawn_test_http_daemon(server, &global).await;
            let proxy = StdioProxyServer {
                adapter_started_at: chrono::Utc::now(),
                tool_profile: Some(tachi_hub::ToolProfile::remember()),
                daemon: std::sync::Arc::new(std::sync::RwLock::new(daemon)),
                app_home: temp.path().to_path_buf(),
                global_db_path: global.clone(),
                project_db_path: Some(project_a.clone()),
                client_project: Some(project_a_name.to_string()),
                resolved_agent_identity: Default::default(),
            };

            let write = |id: &str, identity: serde_json::Value| {
                json!({
                    "jsonrpc":"2.0", "id":id, "method":"tools/call", "params":{
                        "_meta":modern_meta(identity), "name":"tachi_memory", "arguments":{
                            "action":"save", "scope":"project", "id":id,
                            "text":format!("{id} must not dispatch"), "summary":"identity guard",
                            "path":"/tests/modern-stdio-identity", "category":"fact", "force":true
                        }
                    }
                })
            };
            let requests = vec![
                // Validation must happen before the local runtime_info fast path.
                json!({
                    "jsonrpc":"2.0", "id":40, "method":"tools/call", "params":{
                        "_meta":modern_meta(json!({
                            "tachiClient":"client-a", "tachi.client":"client-b"
                        })),
                        "name":"runtime_info", "arguments":{}
                    }
                }),
                write(
                    "stdio-project-drift",
                    json!({"tachiProject":project_b_name, "tachiProfile":"remember"}),
                ),
                write(
                    "stdio-profile-escalation",
                    json!({"tachiProject":project_a_name, "tachiProfile":"admin"}),
                ),
                write(
                    "stdio-profile-change",
                    json!({"tachiProject":project_a_name, "tachiProfile":"observe"}),
                ),
                write(
                    "stdio-malformed-client",
                    json!({"tachiProject":project_a_name, "tachiProfile":"remember", "tachiClient":0}),
                ),
                write(
                    "stdio-agent-conflict",
                    json!({
                        "tachiProject":project_a_name, "tachiProfile":"remember",
                        "tachiAgentIdentity":"agent.canonical",
                        "tachi.agentIdentity":"agent.alias"
                    }),
                ),
                write(
                    "stdio-malformed-agent",
                    json!({
                        "tachiProject":project_a_name, "tachiProfile":"remember",
                        "tachiAgentIdentity":"agent identity with spaces"
                    }),
                ),
            ];
            let responses = stdio_responses(proxy, &requests).await;
            assert_eq!(responses.len(), requests.len());
            for response in &responses {
                assert_eq!(response["error"]["code"], -32602, "{response:#}");
                assert!(response.get("result").is_none(), "{response:#}");
            }
            for id in [
                "stdio-project-drift",
                "stdio-profile-escalation",
                "stdio-profile-change",
                "stdio-malformed-client",
                "stdio-agent-conflict",
                "stdio-malformed-agent",
            ] {
                assert_eq!(memory_id_count(&project_a, id), 0, "{id}");
                assert_eq!(memory_id_count(&project_b, id), 0, "{id}");
                assert_eq!(memory_id_count(&global, id), 0, "{id}");
            }

            cancel.cancel();
            task.await.expect("daemon task");
        });
    });
}

#[test]
fn modern_stdio_implicit_default_stays_standard_against_admin_daemon() {
    let temp = tempfile::tempdir().expect("tempdir");
    with_tachi_home(temp.path(), || {
        let global = temp.path().join("global/memory.db");
        let _agent_env = EnvRestore::remove(crate::session_identity::ENV_AGENT_IDENTITY);
        test_runtime().block_on(async {
            let server = crate::MemoryServer::new(global.clone(), None).expect("server");
            server.set_tool_profile(Some(tachi_hub::ToolProfile::admin()));
            let (daemon, cancel, task) = spawn_test_http_daemon(server, &global).await;
            let proxy = identity_probe_proxy();
            assert!(proxy.tool_profile.is_none(), "fixture must use process default");
            *proxy.daemon.write().expect("proxy daemon lock") = daemon;

            let responses = stdio_responses(
                proxy,
                &[
                    json!({
                        "jsonrpc":"2.0", "id":45, "method":"tools/list",
                        "params":{"_meta":modern_meta(json!({}))}
                    }),
                    json!({
                        "jsonrpc":"2.0", "id":46, "method":"tools/list",
                        "params":{"_meta":modern_meta(json!({"tachiProfile":"standard"}))}
                    }),
                    json!({
                        "jsonrpc":"2.0", "id":47, "method":"tools/call", "params":{
                            "_meta":modern_meta(json!({})),
                            "name":"tachi_memory", "arguments":{
                                "action":"save", "scope":"global",
                                "id":"stdio-default-profile-archive",
                                "text":"standard requests must not inherit admin",
                                "summary":"default profile authority",
                                "path":"/tests/modern-stdio-profile", "category":"fact", "force":true
                            }
                        }
                    }),
                    json!({
                        "jsonrpc":"2.0", "id":48, "method":"tools/call", "params":{
                            "_meta":modern_meta(json!({"tachiProfile":"standard"})),
                            "name":"archive_memory", "arguments":{
                                "id":"stdio-default-profile-archive"
                            }
                        }
                    }),
                    json!({
                        "jsonrpc":"2.0", "id":49, "method":"tools/call", "params":{
                            "_meta":modern_meta(json!({"tachiProfile":"observe"})),
                            "name":"tachi_memory", "arguments":{
                                "action":"save", "scope":"global",
                                "id":"stdio-default-profile-drift",
                                "text":"non-standard declaration must not dispatch",
                                "summary":"default profile equivalence",
                                "path":"/tests/modern-stdio-profile", "category":"fact", "force":true
                            }
                        }
                    }),
                ],
            )
            .await;
            for response in &responses[..2] {
                assert!(response.get("error").is_none(), "{response:#}");
                assert_eq!(response["result"]["resultType"], "complete");
                let tools = response["result"]["tools"]
                    .as_array()
                    .expect("standard tools array");
                assert!(tools.iter().any(|tool| tool["name"] == "tachi_memory"));
                assert!(
                    !tools.iter().any(|tool| tool["name"] == "archive_memory"),
                    "implicit default must not inherit the daemon's admin surface: {response:#}"
                );
            }
            assert_eq!(responses[2]["result"]["resultType"], "complete");
            assert_ne!(
                responses[2]["result"]["isError"],
                json!(true),
                "{:#}",
                responses[2]
            );
            assert_eq!(
                responses[3]["result"]["isError"],
                json!(true),
                "{:#}",
                responses[3]
            );
            assert!(
                http_tool_text(&responses[3]).contains("tool not found"),
                "{:#}",
                responses[3]
            );
            assert_eq!(
                memory_archived_value(&global, "stdio-default-profile-archive"),
                0,
                "standard modern request must not gain admin archive authority"
            );
            assert_eq!(
                responses[4]["error"]["code"],
                -32602,
                "{:#}",
                responses[4]
            );
            assert_eq!(memory_id_count(&global, "stdio-default-profile-drift"), 0);

            cancel.cancel();
            task.await.expect("daemon task");
        });
    });
}

#[test]
fn modern_stdio_forwards_validated_client_and_agent_per_request_without_stickiness() {
    let temp = tempfile::tempdir().expect("tempdir");
    with_tachi_home(temp.path(), || {
        let global = temp.path().join("global/memory.db");
        let _agent_env = EnvRestore::remove(crate::session_identity::ENV_AGENT_IDENTITY);
        test_runtime().block_on(async {
            let server = crate::MemoryServer::new(global.clone(), None).expect("server");
            let (daemon, cancel, task) = spawn_test_http_daemon(server, &global).await;
            let proxy = identity_probe_proxy();
            *proxy.daemon.write().expect("proxy daemon lock") = daemon;
            *proxy
                .resolved_agent_identity
                .lock()
                .expect("proxy identity lock") = Some(
                crate::cli_client::ProxyIdentityForward::Header("agent.legacy-session".to_string()),
            );
            let observer = proxy.clone();
            let responses = stdio_responses(
                proxy,
                &[
                    json!({
                        "jsonrpc":"2.0", "id":50, "method":"tools/call", "params":{
                            "_meta":modern_meta(json!({
                                "tachiClient":"modern-stdio-client",
                                "tachiAgentIdentity":"agent.modern-stdio"
                            })),
                            "name":"tachi_task", "arguments":{
                                "action":"claim", "issue_ref":"owner/repo#1939-stdio",
                                "branch":"identity-test", "worktree_path":"",
                                "claim_scope":["crates/tachi-server/src/bootstrap/serve/stdio.rs"],
                                "claim_role":"executor", "claim_mode":"writable",
                                "expected_head":"stdio-head",
                                "lease_expires_at":"2030-01-01T00:00:00Z"
                            }
                        }
                    }),
                    json!({
                        "jsonrpc":"2.0", "id":51, "method":"tools/call", "params":{
                            "_meta":modern_meta(json!({})),
                            "name":"tachi_a2a", "arguments":{"action":"status"}
                        }
                    }),
                ],
            )
            .await;
            assert_eq!(
                responses[0]["result"]["resultType"], "complete",
                "{:#}",
                responses[0]
            );
            let omitted = http_tool_text(&responses[1]);
            assert!(omitted.contains("admission rejected"), "{:#}", responses[1]);
            assert!(!omitted.contains("agent.modern-stdio"), "{omitted}");

            let (session_client, agent_identity): (String, String) = rusqlite::Connection::open(
                &global,
            )
            .expect("open global DB")
            .query_row(
                "SELECT session_client, agent_identity_id FROM session_claims WHERE issue_ref = ?1",
                ["owner/repo#1939-stdio"],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .expect("modern stdio work claim");
            assert_eq!(session_client, "modern-stdio-client");
            assert_eq!(agent_identity, "agent.modern-stdio");
            assert_eq!(
                observer.forwarded_agent_identity(),
                crate::cli_client::ProxyIdentityForward::Header("agent.legacy-session".to_string())
            );

            cancel.cancel();
            task.await.expect("daemon task");
        });
    });
}

#[test]
fn modern_stdio_omitted_agent_identity_reresolves_process_binding_per_call() {
    let temp = tempfile::tempdir().expect("tempdir");
    with_tachi_home(temp.path(), || {
        let global = temp.path().join("global/memory.db");
        test_runtime().block_on(async {
            let server = crate::MemoryServer::new(global.clone(), None).expect("server");
            let (daemon, cancel, task) = spawn_test_http_daemon(server, &global).await;
            let proxy = identity_probe_proxy();
            *proxy.daemon.write().expect("proxy daemon lock") = daemon;
            let request = json!({
                "jsonrpc":"2.0", "id":52, "method":"tools/call", "params":{
                    "_meta":modern_meta(json!({})),
                    "name":"tachi_a2a", "arguments":{"action":"status"}
                }
            });

            let first = {
                let _agent_env = EnvRestore::set(
                    crate::session_identity::ENV_AGENT_IDENTITY,
                    "agent.env-first",
                );
                stdio_responses(proxy.clone(), std::slice::from_ref(&request)).await
            };
            let second = {
                let _agent_env = EnvRestore::set(
                    crate::session_identity::ENV_AGENT_IDENTITY,
                    "agent.env-second",
                );
                stdio_responses(proxy, std::slice::from_ref(&request)).await
            };

            for (response, expected) in [
                (&first[0], "agent.env-first"),
                (&second[0], "agent.env-second"),
            ] {
                assert_eq!(response["result"]["resultType"], "complete", "{response:#}");
                let body: serde_json::Value =
                    serde_json::from_str(&http_tool_text(response)).expect("A2A status JSON");
                assert_eq!(body["actor_agent_identity_id"], expected, "{body:#}");
            }

            cancel.cancel();
            task.await.expect("daemon task");
        });
    });
}

#[test]
fn proxy_restores_modern_result_envelopes_and_preserves_legacy_shape() {
    let temp = tempfile::tempdir().expect("tempdir");
    with_tachi_home(temp.path(), || {
        let global = temp.path().join("global/memory.db");
        test_runtime().block_on(async {
            let server = crate::MemoryServer::new(global.clone(), None).expect("server");
            let (daemon, cancel, task) = spawn_test_http_daemon(server, &global).await;
            let proxy = identity_probe_proxy();
            *proxy.daemon.write().expect("proxy daemon lock") = daemon.clone();

            let modern = stdio_responses(
                proxy.clone(),
                &[
                    json!({"jsonrpc":"2.0", "id":60, "method":"tools/list", "params":{"_meta":modern_meta(json!({}))}}),
                    json!({"jsonrpc":"2.0", "id":61, "method":"prompts/list", "params":{"_meta":modern_meta(json!({}))}}),
                    json!({"jsonrpc":"2.0", "id":62, "method":"resources/list", "params":{"_meta":modern_meta(json!({}))}}),
                    json!({"jsonrpc":"2.0", "id":63, "method":"resources/templates/list", "params":{"_meta":modern_meta(json!({}))}}),
                    json!({
                        "jsonrpc":"2.0", "id":64, "method":"tools/call", "params":{
                            "_meta":modern_meta(json!({})), "name":"tachi_memory",
                            "arguments":{"action":"search", "query":"result envelope", "scope":"global"}
                        }
                    }),
                ],
            )
            .await;
            for response in &modern[..4] {
                assert_eq!(response["result"]["resultType"], "complete", "{response:#}");
                assert_eq!(response["result"]["ttlMs"], 0, "{response:#}");
                assert_eq!(response["result"]["cacheScope"], "private", "{response:#}");
            }
            assert_eq!(modern[4]["result"]["resultType"], "complete", "{:#}", modern[4]);

            let legacy = stdio_responses(
                proxy,
                &[
                    json!({
                        "jsonrpc":"2.0", "id":65, "method":"initialize", "params":{
                            "protocolVersion":"2025-11-25", "capabilities":{},
                            "clientInfo":{"name":"legacy-result-shape", "version":"1"}
                        }
                    }),
                    json!({"jsonrpc":"2.0", "method":"notifications/initialized"}),
                    json!({"jsonrpc":"2.0", "id":66, "method":"tools/list", "params":{}}),
                    json!({
                        "jsonrpc":"2.0", "id":67, "method":"tools/call", "params":{
                            "name":"tachi_memory",
                            "arguments":{"action":"search", "query":"legacy result envelope", "scope":"global"}
                        }
                    }),
                ],
            )
            .await;
            for response in &legacy[1..] {
                assert!(response["result"].get("resultType").is_none(), "{response:#}");
                assert!(response["result"].get("ttlMs").is_none(), "{response:#}");
                assert!(response["result"].get("cacheScope").is_none(), "{response:#}");
            }

            cancel.cancel();
            task.await.expect("daemon task");
        });
    });
}

#[test]
fn direct_stdio_is_legacy_only_and_modern_rejection_cannot_mutate() {
    let temp = tempfile::tempdir().expect("tempdir");
    with_tachi_home(temp.path(), || {
        let global = temp.path().join("global/memory.db");
        test_runtime().block_on(async {
            let server = crate::MemoryServer::new(global.clone(), None).expect("server");
            let legacy = stdio_responses(
                server.clone_for_mcp_session(),
                &[json!({
                    "jsonrpc":"2.0", "id":68, "method":"initialize", "params":{
                        "protocolVersion":"2025-11-25", "capabilities":{},
                        "clientInfo":{"name":"direct-legacy", "version":"1"}
                    }
                })],
            )
            .await;
            assert_eq!(legacy[0]["result"]["protocolVersion"], "2025-11-25");

            server.set_tool_profile(Some(tachi_hub::ToolProfile::admin()));
            let rejected = [
                json!({
                    "jsonrpc":"2.0", "id":69, "method":"tools/call", "params":{
                        "_meta":modern_meta(json!({"tachiProfile":"observe"})),
                        "name":"tachi_memory", "arguments":{
                            "action":"save", "scope":"global", "id":"direct-modern-profile-drift",
                            "text":"must not dispatch", "summary":"direct stdio is legacy only",
                            "path":"/tests/direct-stdio", "category":"fact", "force":true
                        }
                    }
                }),
                json!({
                    "jsonrpc":"2.0", "id":70, "method":"tools/call", "params":{
                        "_meta":modern_meta(json!({
                            "tachiAgentIdentity":"agent.direct-canonical",
                            "tachi.agentIdentity":"agent.direct-alias"
                        })),
                        "name":"tachi_memory", "arguments":{
                            "action":"save", "scope":"global", "id":"direct-modern-alias-conflict",
                            "text":"must not dispatch", "summary":"direct stdio is legacy only",
                            "path":"/tests/direct-stdio", "category":"fact", "force":true
                        }
                    }
                }),
            ];
            for request in rejected {
                let response = stdio_responses(
                    server.clone_for_mcp_session(),
                    std::slice::from_ref(&request),
                )
                .await;
                assert_eq!(response[0]["error"]["code"], -32022, "{:#}", response[0]);
            }
            assert_eq!(memory_id_count(&global, "direct-modern-profile-drift"), 0);
            assert_eq!(memory_id_count(&global, "direct-modern-alias-conflict"), 0);
        });
    });
}

#[test]
fn modern_http_list_results_include_required_cache_metadata() {
    let temp = tempfile::tempdir().expect("tempdir");
    with_tachi_home(temp.path(), || {
        let global = temp.path().join("global/memory.db");
        test_runtime().block_on(async {
            let server = crate::MemoryServer::new(global.clone(), None).expect("server");
            let (daemon, cancel, task) = spawn_test_http_daemon(server, &global).await;
            let client = reqwest::Client::new();
            for (id, method) in [
                (71, "tools/list"),
                (72, "prompts/list"),
                (73, "resources/list"),
                (74, "resources/templates/list"),
            ] {
                let response = client
                    .post(&daemon.url)
                    .headers(http_headers(&[
                        ("mcp-protocol-version", "2026-07-28"),
                        ("mcp-method", method),
                    ]))
                    .json(&json!({
                        "jsonrpc":"2.0", "id":id, "method":method,
                        "params":{"_meta":modern_meta(json!({}))}
                    }))
                    .send()
                    .await
                    .expect("modern HTTP list request");
                let body = parse_http_mcp_payload(
                    &response.text().await.expect("modern HTTP list body"),
                    id,
                );
                assert_eq!(body["result"]["resultType"], "complete", "{method}: {body:#}");
                assert_eq!(body["result"]["ttlMs"], 0, "{method}: {body:#}");
                assert_eq!(body["result"]["cacheScope"], "private", "{method}: {body:#}");
            }
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
fn modern_http_concurrent_requests_isolate_project_profile_actor_and_work_claim() {
    let temp = tempfile::tempdir().expect("tempdir");
    with_tachi_home(temp.path(), || {
        let global = temp.path().join("global/memory.db");
        let project_a_name = "http-modern-project-a";
        let project_b_name = "http-modern-project-b";
        let project_a = temp
            .path()
            .join("projects")
            .join(project_a_name)
            .join("memory.db");
        let project_b = temp
            .path()
            .join("projects")
            .join(project_b_name)
            .join("memory.db");
        seed_project_db(temp.path(), &project_a);
        seed_project_db(temp.path(), &project_b);

        test_runtime().block_on(async {
            let server = crate::MemoryServer::new(global.clone(), None).expect("server");
            let (daemon, cancel, task) = spawn_test_http_daemon(server, &global).await;
            let client = reqwest::Client::new();
            let identity_a = || {
                json!({
                    "tachiProject":project_a_name, "tachiProfile":"remember",
                    "tachiClient":"http-client-a", "tachiAgentIdentity":"agent.http-a"
                })
            };
            let identity_b = || {
                json!({
                    "tachiProject":project_b_name, "tachiProfile":"coordinate",
                    "tachiClient":"http-client-b", "tachiAgentIdentity":"agent.http-b"
                })
            };

            let (save_a, save_b) = tokio::join!(
                modern_http_tool_call(
                    &client,
                    &daemon.url,
                    60,
                    "tachi_memory",
                    identity_a(),
                    json!({
                        "action":"save", "scope":"project", "id":"http-isolation-a",
                        "text":"HTTP actor A isolated write", "summary":"actor A",
                        "path":"/tests/http-isolation", "category":"fact", "force":true
                    })
                ),
                modern_http_tool_call(
                    &client,
                    &daemon.url,
                    61,
                    "tachi_memory",
                    identity_b(),
                    json!({
                        "action":"save", "scope":"project", "id":"http-isolation-b",
                        "text":"HTTP actor B isolated write", "summary":"actor B",
                        "path":"/tests/http-isolation", "category":"fact", "force":true
                    })
                )
            );
            for response in [&save_a, &save_b] {
                assert!(response.get("error").is_none(), "{response:#}");
                assert_eq!(response["result"]["resultType"], "complete");
            }
            assert_eq!(memory_id_count(&project_a, "http-isolation-a"), 1);
            assert_eq!(memory_id_count(&project_a, "http-isolation-b"), 0);
            assert_eq!(memory_id_count(&project_b, "http-isolation-a"), 0);
            assert_eq!(memory_id_count(&project_b, "http-isolation-b"), 1);
            assert_eq!(memory_id_count(&global, "http-isolation-a"), 0);
            assert_eq!(memory_id_count(&global, "http-isolation-b"), 0);

            let claim_args = |issue_ref: &str, scope: &str, role: &str| {
                json!({
                    "action":"claim", "issue_ref":issue_ref,
                    "branch":format!("identity-{role}"), "worktree_path":"",
                    "claim_scope":[scope], "claim_role":role, "claim_mode":"writable",
                    "expected_head":format!("head-{role}"),
                    "lease_expires_at":"2030-01-01T00:00:00Z"
                })
            };
            let (claim_a, claim_b) = tokio::join!(
                modern_http_tool_call(
                    &client,
                    &daemon.url,
                    62,
                    "tachi_task",
                    identity_a(),
                    claim_args("owner/repo#1939-http-a", "a/**", "executor-a")
                ),
                modern_http_tool_call(
                    &client,
                    &daemon.url,
                    63,
                    "tachi_task",
                    identity_b(),
                    claim_args("owner/repo#1939-http-b", "b/**", "executor-b")
                )
            );
            for response in [&claim_a, &claim_b] {
                assert!(response.get("error").is_none(), "{response:#}");
                assert_eq!(response["result"]["resultType"], "complete");
            }

            let connection = rusqlite::Connection::open(&global).expect("open global DB");
            for (issue_ref, expected_client, expected_agent, expected_role) in [
                (
                    "owner/repo#1939-http-a",
                    "http-client-a",
                    "agent.http-a",
                    "executor-a",
                ),
                (
                    "owner/repo#1939-http-b",
                    "http-client-b",
                    "agent.http-b",
                    "executor-b",
                ),
            ] {
                let actual: (String, String, String) = connection
                    .query_row(
                        "SELECT session_client, agent_identity_id, role FROM session_claims WHERE issue_ref = ?1",
                        [issue_ref],
                        |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
                    )
                    .expect("request-local work claim authority");
                assert_eq!(actual, (expected_client.into(), expected_agent.into(), expected_role.into()));
            }
            drop(connection);

            let (actor_a, actor_b) = tokio::join!(
                modern_http_tool_call(
                    &client,
                    &daemon.url,
                    64,
                    "tachi_a2a",
                    identity_a(),
                    json!({"action":"status"})
                ),
                modern_http_tool_call(
                    &client,
                    &daemon.url,
                    65,
                    "tachi_a2a",
                    identity_b(),
                    json!({"action":"status"})
                )
            );
            for (response, expected_actor) in
                [(&actor_a, "agent.http-a"), (&actor_b, "agent.http-b")]
            {
                let body: serde_json::Value = serde_json::from_str(&http_tool_text(response))
                    .expect("A2A status JSON");
                assert_eq!(body["actor_agent_identity_id"], expected_actor, "{body:#}");
            }

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
