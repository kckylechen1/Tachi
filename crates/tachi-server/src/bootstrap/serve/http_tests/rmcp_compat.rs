//! Dual-era wire conformance for the RMCP 3.8 protocol adapter.
mod capability_lifecycle;
mod request_boundaries;
mod resources;
mod skills;
#[cfg(any(unix, windows))]
mod tasks;
use super::*;
use serde_json::json;

#[derive(Clone)]
struct LegacyProxyFixture;

impl rmcp::ServerHandler for LegacyProxyFixture {
    fn supported_protocol_versions(
        &self,
    ) -> std::borrow::Cow<'static, [rmcp::model::ProtocolVersion]> {
        std::borrow::Cow::Borrowed(rmcp::model::ProtocolVersion::known_up_to(
            &rmcp::model::ProtocolVersion::V_2024_11_05,
        ))
    }

    fn get_info(&self) -> rmcp::model::ServerConfig {
        let mut info = rmcp::model::ServerConfig::new(
            rmcp::model::ServerCapabilities::builder()
                .enable_tools()
                .build(),
        );
        info.protocol_version = rmcp::model::ProtocolVersion::V_2024_11_05;
        info
    }

    async fn call_tool(
        &self,
        request: rmcp::model::CallToolRequestParams,
        _context: rmcp::service::RequestContext<rmcp::service::RoleServer>,
    ) -> Result<rmcp::model::CallToolResponse, rmcp::ErrorData> {
        let result = match request.name.as_ref() {
            "succeed" => {
                rmcp::model::CallToolResult::success(vec![rmcp::model::ContentBlock::text(
                    "legacy upstream success",
                )])
            }
            "tool_error" => {
                rmcp::model::CallToolResult::error(vec![rmcp::model::ContentBlock::text(
                    "legacy upstream tool error",
                )])
            }
            other => rmcp::model::CallToolResult::error(vec![rmcp::model::ContentBlock::text(
                format!("unknown legacy fixture tool: {other}"),
            )]),
        };
        Ok(result.into())
    }
}

async fn install_legacy_proxy_fixture(
    server: &crate::MemoryServer,
    server_name: &str,
) -> tokio::task::JoinHandle<()> {
    use rmcp::ServiceExt;

    let (server_io, client_io) = tokio::io::duplex(16384);
    let upstream = tokio::spawn(async move {
        let service = LegacyProxyFixture
            .serve(server_io)
            .await
            .expect("start legacy upstream");
        let _ = service.waiting().await;
    });
    let client = legacy_client_config()
        .serve(client_io)
        .await
        .expect("initialize legacy upstream");
    assert_eq!(
        client
            .peer()
            .peer_info()
            .expect("legacy upstream server info")
            .protocol_version,
        rmcp::model::ProtocolVersion::V_2024_11_05
    );
    server.pool.install_test_connection(server_name, client);
    upstream
}

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

fn privileged_fixture_client(daemon: &crate::cli_client::DaemonInfo) -> reqwest::Client {
    reqwest::Client::builder()
        .default_headers(http_headers(&[(
            crate::session_identity::HEADER_INTERNAL_PROXY_TOKEN,
            daemon
                .internal_proxy_token
                .as_deref()
                .expect("fixture capability"),
        )]))
        .build()
        .expect("fixture client")
}

async fn modern_http_tool_call(
    client: &reqwest::Client,
    url: &str,
    id: i64,
    name: &str,
    identity: serde_json::Value,
    arguments: serde_json::Value,
) -> serde_json::Value {
    modern_http_tool_call_with_headers(client, url, id, name, identity, arguments, &[]).await
}

async fn modern_http_tool_call_with_headers(
    client: &reqwest::Client,
    url: &str,
    id: i64,
    name: &str,
    identity: serde_json::Value,
    arguments: serde_json::Value,
    extra_headers: &[(&str, &str)],
) -> serde_json::Value {
    let mut headers = vec![
        ("mcp-protocol-version", "2026-07-28"),
        ("mcp-method", "tools/call"),
        ("mcp-name", name),
    ];
    headers.extend_from_slice(extra_headers);
    let response = client
        .post(url)
        .headers(http_headers(&headers))
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
fn dual_era_conformance_matrix_covers_http() {
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
            assert_eq!(identity_conflict.status(), reqwest::StatusCode::BAD_REQUEST);
            let identity_conflict = parse_http_mcp_payload(
                &identity_conflict.text().await.expect("identity conflict body"),
                24,
            );
            assert_eq!(identity_conflict["error"]["code"], -32020);
            assert_eq!(memory_id_count(&global, "identity-conflict-write"), 0);

            // Modern identity is request-scoped; it cannot become session authority.
            let ops_client = privileged_fixture_client(&daemon);
            let first_runtime = ops_client
                .post(&daemon.url)
                .headers(http_headers(&[
                    ("mcp-protocol-version", "2026-07-28"),
                    ("mcp-method", "tools/call"),
                    ("mcp-name", "runtime_info"),
                    (crate::session_identity::HEADER_OWNER_TOOL, "runtime_info"),
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

            let second_runtime = ops_client
                .post(&daemon.url)
                .headers(http_headers(&[
                    ("mcp-protocol-version", "2026-07-28"),
                    ("mcp-method", "tools/call"),
                    ("mcp-name", "runtime_info"),
                    (crate::session_identity::HEADER_OWNER_TOOL, "runtime_info"),
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
                    ("mcp-name", "tachi://legacy-only"),
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
            assert_eq!(legacy_route["error"]["code"], -32601, "{legacy_route:#}");

            // Stdio legacy, modern, partial, and explicit-modern-initialize rows.
            cancel.cancel();
            task.await.expect("daemon task");
        });
    });
}

#[test]
fn modern_client_info_is_validated_from_each_http_request() {
    let temp = tempfile::tempdir().expect("tempdir");
    with_tachi_home(temp.path(), || {
        let global = temp.path().join("global/memory.db");
        test_runtime().block_on(async {
            let server = crate::MemoryServer::new(global.clone(), None).expect("server");
            let (daemon, cancel, task) = spawn_test_http_daemon(server, &global).await;
            let client_info_cases = || {
                vec![
                    (
                        "absent",
                        json!({
                            "io.modelcontextprotocol/protocolVersion":"2026-07-28",
                            "io.modelcontextprotocol/clientCapabilities":{}
                        }),
                        true,
                    ),
                    ("valid", modern_meta(json!({})), true),
                    (
                        "malformed",
                        modern_meta(json!({
                            "io.modelcontextprotocol/clientInfo":{"name":0,"version":"1"}
                        })),
                        false,
                    ),
                ]
            };

            let client = reqwest::Client::new();
            for (offset, (label, meta, accepted)) in client_info_cases().into_iter().enumerate() {
                let memory_id = format!("client-info-http-{label}");
                let request_id = 90 + offset as i64;
                let response = client
                    .post(&daemon.url)
                    .headers(http_headers(&[
                        ("mcp-protocol-version", "2026-07-28"),
                        ("mcp-method", "tools/call"),
                        ("mcp-name", "tachi_memory"),
                    ]))
                    .json(&json!({
                        "jsonrpc":"2.0", "id":request_id, "method":"tools/call",
                        "params":{
                            "_meta":meta, "name":"tachi_memory", "arguments":{
                                "action":"save", "scope":"global", "id":memory_id,
                                "text":"modern HTTP clientInfo validation",
                                "summary":"clientInfo wire discriminator",
                                "path":"/tests/client-info", "category":"fact", "force":true
                            }
                        }
                    }))
                    .send()
                    .await
                    .expect("modern HTTP clientInfo request");
                assert_eq!(
                    response.status(),
                    reqwest::StatusCode::OK,
                    "{label}: exact application response HTTP mapping"
                );
                let payload = parse_http_mcp_payload(
                    &response.text().await.expect("modern HTTP clientInfo body"),
                    request_id,
                );
                if accepted {
                    assert!(payload.get("error").is_none(), "{label}: {payload:#}");
                    assert_eq!(payload["result"]["resultType"], "complete", "{payload:#}");
                    assert_eq!(memory_id_count(&global, &memory_id), 1, "{label}");
                } else {
                    assert_eq!(payload["error"]["code"], -32602, "{payload:#}");
                    assert_eq!(memory_id_count(&global, &memory_id), 0, "{label}");
                }
            }

            cancel.cancel();
            task.await.expect("daemon task");
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
                assert_eq!(
                    body["result"]["resultType"], "complete",
                    "{method}: {body:#}"
                );
                assert_eq!(body["result"]["ttlMs"], 0, "{method}: {body:#}");
                assert_eq!(
                    body["result"]["cacheScope"], "private",
                    "{method}: {body:#}"
                );
            }
            cancel.cancel();
            task.await.expect("daemon task");
        });
    });
}

#[test]
fn modern_http_hydrates_legacy_proxy_success_and_tool_error_results() {
    let temp = tempfile::tempdir().expect("tempdir");
    with_tachi_home(temp.path(), || {
        let global = temp.path().join("global/memory.db");
        test_runtime().block_on(async {
            let server_name = "legacy-result-fixture";
            let capability_id = format!("mcp:{server_name}");
            let server = crate::MemoryServer::new(global.clone(), None).expect("server");
            server.set_tool_profile(Some(tachi_hub::ToolProfile::admin()));
            let mut capability = crate::tests::make_mcp_capability(&capability_id, 1);
            capability.definition = json!({
                "transport":"stdio", "command":"unused-test-connection",
                "tool_exposure":"flatten", "max_concurrency":1
            })
            .to_string();
            capability.exposure_mode = "direct".to_string();
            server
                .with_global_store(|store| {
                    store
                        .hub_register(&capability)
                        .map_err(|error| error.to_string())?;
                    store
                        .set_sandbox_policy(
                            &capability_id,
                            "process",
                            "[]",
                            "[]",
                            "[]",
                            "[]",
                            5_000,
                            5_000,
                            1,
                            true,
                        )
                        .map_err(|error| error.to_string())
                })
                .expect("register legacy proxy fixture");
            let tools: Vec<rmcp::model::Tool> = ["succeed", "tool_error"]
                .into_iter()
                .map(|name| {
                    serde_json::from_value(json!({
                        "name":name, "description":format!("legacy fixture {name}"),
                        "inputSchema":{"type":"object","additionalProperties":true}
                    }))
                    .expect("legacy fixture tool")
                })
                .collect();
            server.cache_proxy_tools(server_name, tools);
            let upstream = install_legacy_proxy_fixture(&server, server_name).await;
            let (daemon, cancel, task) = spawn_test_http_daemon(server.clone(), &global).await;

            for (id, tool_name, is_error, expected_text) in [
                (75, "succeed", false, "legacy upstream success"),
                (76, "tool_error", true, "legacy upstream tool error"),
            ] {
                let full_name = format!("{server_name}__{tool_name}");
                let (legacy_client, legacy_headers, init) = http_mcp_initialize(
                    &daemon.url,
                    http_headers(&[
                        (crate::session_identity::HEADER_OWNER_TOOL, &full_name),
                        (
                            crate::session_identity::HEADER_INTERNAL_PROXY_TOKEN,
                            daemon
                                .internal_proxy_token
                                .as_deref()
                                .expect("fixture capability"),
                        ),
                    ]),
                    None,
                )
                .await;
                assert!(init.get("error").is_none(), "{init:#}");
                http_mcp_initialized(&legacy_client, &daemon.url, legacy_headers.clone()).await;
                let legacy = http_mcp_call_tool(
                    &legacy_client,
                    &daemon.url,
                    legacy_headers.clone(),
                    id,
                    &format!("{server_name}__{tool_name}"),
                    serde_json::Map::new(),
                )
                .await;
                assert!(legacy["result"].get("resultType").is_none(), "{legacy:#}");
                assert_eq!(http_tool_text(&legacy), expected_text, "{legacy:#}");
                if is_error {
                    assert_eq!(legacy["result"]["isError"], true, "{legacy:#}");
                } else {
                    assert_ne!(legacy["result"]["isError"], true, "{legacy:#}");
                }
            }

            let modern_client = privileged_fixture_client(&daemon);
            let success_name = format!("{server_name}__succeed");
            let error_name = format!("{server_name}__tool_error");
            let success_headers = [(
                crate::session_identity::HEADER_OWNER_TOOL,
                success_name.as_str(),
            )];
            let error_headers = [(
                crate::session_identity::HEADER_OWNER_TOOL,
                error_name.as_str(),
            )];
            let (modern_success, modern_tool_error) = tokio::join!(
                modern_http_tool_call_with_headers(
                    &modern_client,
                    &daemon.url,
                    77,
                    &success_name,
                    json!({}),
                    json!({}),
                    &success_headers,
                ),
                modern_http_tool_call_with_headers(
                    &modern_client,
                    &daemon.url,
                    78,
                    &error_name,
                    json!({}),
                    json!({}),
                    &error_headers,
                )
            );
            assert_eq!(
                modern_success["result"]["resultType"], "complete",
                "{modern_success:#}"
            );
            assert_ne!(
                modern_success["result"]["isError"], true,
                "{modern_success:#}"
            );
            assert_eq!(
                http_tool_text(&modern_success),
                "legacy upstream success",
                "{modern_success:#}"
            );
            assert_eq!(
                modern_tool_error["result"]["resultType"], "complete",
                "{modern_tool_error:#}"
            );
            assert_eq!(
                modern_tool_error["result"]["isError"], true,
                "{modern_tool_error:#}"
            );
            assert_eq!(
                http_tool_text(&modern_tool_error),
                "legacy upstream tool error",
                "{modern_tool_error:#}"
            );

            cancel.cancel();
            task.await.expect("daemon task");
            assert!(server.pool.remove_connection(server_name));
            tokio::time::timeout(std::time::Duration::from_secs(30), upstream)
                .await
                .expect("legacy upstream exit deadline")
                .expect("legacy upstream task");
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

/// Audit C1: `clone_for_modern_request` mints a per-request rate-limit key,
/// so without a stable bucket a modern peer's burst window started empty on
/// every request and stuck/loop detection never fired. Requests with the same
/// resolved identity now share a bucket; a different identity does not; a
/// malformed `X-Tachi-Rate-Limit-Session` is ignored (neither an error nor a
/// bucket). PR2009 repair: every modern request is a fresh clone, so a valid
/// bucket header must NOT override the identity-derived bucket — one
/// identified peer rotating fresh legal keys per call would otherwise shed
/// its own burst window; rotation must still reach the hard `Loop detected`
/// block. The header stays the bucket selector only for identityless requests
/// (the stdio proxy's per-connection key), and a request with no
/// AgentIdentity and no header stays per-request, even with a client label.
#[test]
fn modern_http_rate_limit_bucket_follows_identity_or_bucket_header() {
    let temp = tempfile::tempdir().expect("tempdir");
    with_tachi_home(temp.path(), || {
        let global = temp.path().join("global/memory.db");
        test_runtime().block_on(async {
            let server = crate::MemoryServer::new(global.clone(), None).expect("server");
            let (daemon, cancel, task) = spawn_test_http_daemon(server, &global).await;
            let client = reqwest::Client::new();
            let identity = |who: &str| {
                json!({"tachiClient":format!("c1-{who}"), "tachiAgentIdentity":format!("agent.c1-{who}")})
            };
            let args = || {
                json!({"action":"search", "query":"C1-MODERN-BUCKET-PROBE", "scope":"memory", "top_k":1})
            };
            let stuck = |payload: &serde_json::Value| {
                assert!(payload.get("error").is_none(), "{payload:#}");
                payload["result"]["content"]
                    .as_array()
                    .expect("tool content")
                    .iter()
                    .any(|block| {
                        block["text"]
                            .as_str()
                            .is_some_and(|text| text.contains("stuck-detection"))
                    })
            };
            let loop_blocked = |payload: &serde_json::Value| {
                payload
                    .get("error")
                    .and_then(|err| err.get("message"))
                    .and_then(|message| message.as_str())
                    .is_some_and(|message| message.contains("Loop detected"))
            };
            let call = |id: i64, identity: serde_json::Value, headers: &'static [(&'static str, &'static str)]| {
                modern_http_tool_call_with_headers(
                    &client, &daemon.url, id, "tachi_memory", identity, args(), headers,
                )
            };

            for id in [1, 2] {
                assert!(!stuck(&call(id, identity("a"), &[]).await), "request {id}");
            }
            assert!(
                !stuck(&call(3, identity("b"), &[]).await),
                "a different identity must not inherit identity A's burst window"
            );
            let malformed: &'static [(&'static str, &'static str)] =
                &[(crate::session_identity::HEADER_RATE_LIMIT_SESSION, "not a valid key!")];
            assert!(
                stuck(&call(4, identity("a"), malformed).await),
                "a malformed bucket header is ignored and the third identical request \
                 from identity A shares its identity-derived burst window"
            );

            // PR2009 repair: every modern request is a fresh clone, so a
            // valid bucket header must not override the identity-derived
            // bucket — rotating fresh legal keys would let identity A shed
            // its own burst window. The soft stuck warning keeps firing
            // across rotations (requests 5-8 land in identity A's window,
            // which already holds the three stamps from requests 1, 2, 4)...
            for (i, id) in (5..=8).enumerate() {
                let key = format!("c1-rotate-header-{i:04}");
                let headers = [(
                    crate::session_identity::HEADER_RATE_LIMIT_SESSION,
                    key.as_str(),
                )];
                let payload = modern_http_tool_call_with_headers(
                    &client,
                    &daemon.url,
                    id,
                    "tachi_memory",
                    identity("a"),
                    args(),
                    &headers,
                )
                .await;
                assert!(
                    stuck(&payload),
                    "rotated legal header {i} must keep identity A's burst window: {payload:#}"
                );
            }
            // ...the ninth identical call is the last the default burst
            // window (limit 8) permits: no soft warning at exactly the
            // limit, and no block yet...
            let ninth = {
                let headers = [(
                    crate::session_identity::HEADER_RATE_LIMIT_SESSION,
                    "c1-rotate-header-0004",
                )];
                modern_http_tool_call_with_headers(
                    &client,
                    &daemon.url,
                    9,
                    "tachi_memory",
                    identity("a"),
                    args(),
                    &headers,
                )
                .await
            };
            assert!(
                !stuck(&ninth) && !loop_blocked(&ninth),
                "the ninth identical call is the last permitted one: {ninth:#}"
            );
            // ...and the tenth identical call hits the hard loop block even
            // though it presents yet another fresh legal key.
            let tenth = {
                let headers = [(
                    crate::session_identity::HEADER_RATE_LIMIT_SESSION,
                    "c1-rotate-header-0005",
                )];
                modern_http_tool_call_with_headers(
                    &client,
                    &daemon.url,
                    10,
                    "tachi_memory",
                    identity("a"),
                    args(),
                    &headers,
                )
                .await
            };
            assert!(
                loop_blocked(&tenth),
                "rotating legal headers must still reach the loop block: {tenth:#}"
            );

            // The header stays the bucket selector for identityless requests
            // (the stdio proxy's per-connection key): one key with no
            // AgentIdentity still accumulates one shared window.
            let anon: &'static [(&'static str, &'static str)] = &[(
                crate::session_identity::HEADER_RATE_LIMIT_SESSION,
                "c1-anon-shared-bucket-01",
            )];
            for id in [11, 12] {
                assert!(!stuck(&call(id, json!({}), anon).await), "request {id}");
            }
            assert!(stuck(&call(13, json!({}), anon).await));

            // Nothing stable to key on: per-request buckets stay the fallback.
            for id in [14, 15, 16] {
                assert!(!stuck(&call(id, json!({}), &[]).await), "request {id}");
            }
            // A client label alone is shared by every instance of that client,
            // so it must not anchor a bucket either.
            for id in [17, 18, 19] {
                let label_only = json!({"tachiClient":"c1-shared-label"});
                assert!(!stuck(&call(id, label_only, &[]).await), "request {id}");
            }

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
                let worktree = temp.path().join(format!("http-worktree-{role}"));
                std::fs::create_dir(&worktree).expect("create independent worktree fixture");
                json!({
                    "action":"claim", "issue_ref":issue_ref,
                    "branch":format!("identity-{role}"), "worktree_path":worktree,
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
                assert_ne!(response["result"]["isError"], true, "claim failed: {response:#}");
                assert_eq!(response["result"]["resultType"], "complete");
            }

            let (remember_coordinate_op, coordinate_op) = tokio::join!(
                modern_http_tool_call(
                    &client,
                    &daemon.url,
                    79,
                    "tachi_task",
                    identity_a(),
                    json!({"action":"handoff"})
                ),
                modern_http_tool_call(
                    &client,
                    &daemon.url,
                    80,
                    "tachi_task",
                    identity_b(),
                    json!({"action":"handoff"})
                )
            );
            assert_eq!(
                remember_coordinate_op["result"]["isError"],
                true,
                "{remember_coordinate_op:#}"
            );
            assert!(
                http_tool_text(&remember_coordinate_op)
                    .contains("is not allowed for ToolProfile 'observe,remember'"),
                "remember must not gain the coordinate-tier action: {remember_coordinate_op:#}"
            );
            assert!(
                !http_tool_text(&coordinate_op).contains("is not allowed for ToolProfile"),
                "concurrent Coordinate request must keep its own profile: {coordinate_op:#}"
            );
            let privileged_client = privileged_fixture_client(&daemon);
            let owner_identity = |mut identity: serde_json::Value| { identity["tachiProfile"] = json!("standard"); identity };

            let (runtime_a, runtime_b) = tokio::join!(
                modern_http_tool_call_with_headers(&privileged_client, &daemon.url, 81, "runtime_info", owner_identity(identity_a()), json!({}), &[(crate::session_identity::HEADER_OWNER_TOOL, "runtime_info")]),
                modern_http_tool_call_with_headers(&privileged_client, &daemon.url, 82, "runtime_info", owner_identity(identity_b()), json!({}), &[(crate::session_identity::HEADER_OWNER_TOOL, "runtime_info")]),
            );
            let connection = rusqlite::Connection::open(&global).expect("open global DB");
            for (issue_ref, expected_client, expected_agent, expected_role, claim_response, runtime_response) in [
                ("owner/repo#1939-http-a", "http-client-a", "agent.http-a", "executor-a", &claim_a, &runtime_a),
                ("owner/repo#1939-http-b", "http-client-b", "agent.http-b", "executor-b", &claim_b, &runtime_b),
            ] {
                let claim: serde_json::Value = serde_json::from_str(&http_tool_text(claim_response))
                    .expect("claim response JSON");
                let claim_id = claim["claim_id"].as_str().expect("returned claim id");
                let actual: (String, String, String, String) = connection
                    .query_row(
                        "SELECT session_client, agent_identity_id, role, worktree_path FROM session_claims WHERE claim_id = ?1 AND issue_ref = ?2",
                        [claim_id, issue_ref],
                        |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
                    )
                    .expect("request-local work claim authority");
                let expected_path = std::fs::canonicalize(temp.path().join(format!("http-worktree-{expected_role}")))
                    .expect("canonical worktree").to_string_lossy().into_owned();
                assert_eq!(actual, (format!("work-claim:{claim_id}"), expected_agent.into(), expected_role.into(), expected_path));
                let runtime: serde_json::Value = serde_json::from_str(&http_tool_text(runtime_response))
                    .expect("runtime response JSON");
                assert_eq!(runtime["runtime"]["session_client"], expected_client);
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
