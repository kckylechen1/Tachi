use super::*;
use axum::{extract::State, routing::post, Json};
use rmcp::{model::*, ServiceExt};
use std::sync::{Arc, Mutex};

fn upstream_tool(name: &str) -> Tool {
    serde_json::from_value(json!({
        "name": name, "description": "Upstream action with its own schema",
        "inputSchema": {"type":"object", "properties":{
            "action":{"type":"string"}, "project":{"type":"string"},
            "nested":{"type":"object", "additionalProperties":true}
        }, "required":["action"]},
        "outputSchema":{"type":"object", "properties":{"ok":{"type":"boolean"}}},
        "annotations":{"readOnlyHint":false},
        "_meta":{"vendor":"fixture"}
    }))
    .unwrap()
}

fn rich_result() -> serde_json::Value {
    json!({
        "content":[
            {"type":"text","text":"upstream result"},
            {"type":"image","data":"aW1hZ2U=","mimeType":"image/png"},
            {"type":"resource_link","uri":"https://example.test/result","name":"Result"}
        ],
        "structuredContent":{"ok":true},
        "isError":true,
        "_meta":{"vendor":"preserved"}
    })
}

async fn upstream(
    State(calls): State<Arc<Mutex<Vec<serde_json::Value>>>>,
    Json(request): Json<serde_json::Value>,
) -> axum::response::Response {
    use axum::response::IntoResponse;
    let result = match request["method"].as_str().unwrap_or_default() {
        "initialize" => json!({"protocolVersion":"2025-11-25", "capabilities":{"tools":{}},
            "serverInfo":{"name":"managed-fixture", "version":"1"}}),
        "notifications/initialized" => return axum::http::StatusCode::ACCEPTED.into_response(),
        "tools/list" if request["params"]["cursor"] == "second" => {
            json!({"tools":[upstream_tool("other")]})
        }
        "tools/list" => json!({"tools":[upstream_tool("invoke")], "nextCursor":"second"}),
        "tools/call" => {
            calls.lock().unwrap().push(request["params"].clone());
            rich_result()
        }
        _ => return axum::http::StatusCode::METHOD_NOT_ALLOWED.into_response(),
    };
    let reply = json!({"jsonrpc":"2.0", "id":request["id"], "result":result});
    if request["method"] == "tools/call" && calls.lock().unwrap().len() == 2 {
        (
            [(axum::http::header::CONTENT_TYPE, "text/event-stream")],
            format!("event: message\ndata: {reply}\n\n"),
        )
            .into_response()
    } else {
        Json(reply).into_response()
    }
}

/// Exercises the real SDK HTTP lifecycle/pagination and the ordinary Tachi
/// tools/list + tools/call boundary. No provider credentials or live API calls.
#[tokio::test]
async fn managed_mcp_preserves_native_tools_arguments_and_rich_results_for_lead_and_worker() {
    crate::ensure_tls_provider();
    let calls = Arc::new(Mutex::new(Vec::new()));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = format!("http://{}/mcp", listener.local_addr().unwrap());
    let app = axum::Router::new()
        .route("/mcp", post(upstream))
        .with_state(calls.clone());
    let http_task = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    let server = crate::tests::make_server();
    // Inject only the transport in this fixture; production public-host checks
    // remain unchanged and continue to reject loopback destinations.
    let transport = StreamableHttpClientTransport::with_client(
        http_client::McpHttpClient(reqwest::Client::new()),
        rmcp::transport::streamable_http_client::StreamableHttpClientTransportConfig::with_uri(
            endpoint.clone(),
        ),
    );
    let connection = legacy_client_config().serve(transport).await.unwrap();
    let tools = connection.peer().list_all_tools().await.unwrap();
    assert_eq!(tools.len(), 2, "SDK must follow tools/list pagination");
    let mut cap = crate::tests::make_mcp_capability("mcp:fixture", 1);
    let mut def = json!({"transport":"streamable-http", "url":endpoint,
        "tool_exposure":"gateway", "policy":{"visibility":"discoverable"},
        "permissions":{"allow":["invoke"]}});
    crate::mcp_proxy::set_mcp_discovery_success(&mut def, &tools);
    cap.definition = def.to_string();
    server
        .with_global_store(|store| store.hub_register(&cap).map_err(|e| e.to_string()))
        .unwrap();
    server.pool.install_test_connection(
        &server.mcp_connection_key(
            &cap,
            None,
            server
                .get_effective_sandbox_policy(None, &cap.id)
                .0
                .as_ref(),
        ),
        connection,
    );

    for profile in [
        tachi_hub::ToolProfile::standard(),
        tachi_hub::ToolProfile::delegate(),
    ] {
        server.set_tool_profile(Some(profile));
        let (server_io, client_io) = tokio::io::duplex(65536);
        let local = server.clone();
        let daemon =
            tokio::spawn(async move { local.serve(server_io).await.unwrap().waiting().await });
        let client = legacy_client_config().serve(client_io).await.unwrap();
        let listed = client.peer().list_all_tools().await.unwrap();
        let mut expected = upstream_tool("invoke");
        expected.name = "fixture__invoke".into();
        let actual = listed
            .iter()
            .find(|tool| tool.name == expected.name)
            .expect("ordinary profile sees upstream");
        assert_eq!(
            serde_json::to_value(actual).unwrap(),
            serde_json::to_value(expected).unwrap()
        );
        assert!(!listed
            .iter()
            .any(|tool| tool.name == "fixture__other" || tool.name == "hub_call"));
        let args = json!({"action":"delete", "project":"upstream-project", "nested":{"ids":[1,2]}});
        let result = client
            .peer()
            .call_tool(
                CallToolRequestParams::new("fixture__invoke")
                    .with_arguments(args.as_object().unwrap().clone()),
            )
            .await
            .unwrap();
        let result = serde_json::to_value(result).unwrap();
        for field in ["content", "structuredContent", "isError", "_meta"] {
            assert_eq!(result[field], rich_result()[field], "preserve {field}");
        }
        let recorded = calls.lock().unwrap().last().unwrap().clone();
        assert_eq!(recorded["name"], "invoke");
        assert_eq!(
            recorded["arguments"], args,
            "Tachi must not rewrite upstream project/action"
        );
        for name in ["fixture__missing", "unregistered__invoke", "fixture__other"] {
            let denied = client
                .peer()
                .call_tool(CallToolRequestParams::new(name))
                .await
                .unwrap();
            assert_eq!(denied.is_error, Some(true));
        }
        client.cancel().await.unwrap();
        daemon.await.unwrap().unwrap();
    }
    assert_eq!(calls.lock().unwrap().len(), 2);
    server.pool.remove_connection("fixture");
    http_task.abort();
}

#[test]
fn explicit_vault_reference_never_uses_environment_or_bypasses_resolver_denial() {
    const KEY: &str = "TACHI_MANAGED_MCP_VAULT_TEST";
    std::env::set_var(KEY, "environment-value-must-not-be-used");
    let placeholder = format!("${{vault:{KEY}}}");
    assert!(expand_placeholders_with_secret_resolver(&placeholder, &|_| Ok(None)).is_err());
    let resolver = |key: &str| -> Result<Option<String>, String> {
        if key == KEY {
            Err("denied".into())
        } else {
            Ok(Some("fallback".into()))
        }
    };
    let error =
        expand_placeholders_with_secret_resolver(&format!("${{vault:{KEY}|OTHER}}"), &resolver)
            .unwrap_err();
    assert!(error.contains("denied"));
    std::env::remove_var(KEY);
}

#[tokio::test]
async fn managed_http_rejects_large_or_invalid_json_without_echoing_the_body() {
    use axum::response::IntoResponse;
    use rmcp::transport::streamable_http_client::StreamableHttpClient;
    crate::ensure_tls_provider();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let origin = format!("http://{}", listener.local_addr().unwrap());
    let app = axum::Router::new()
        .route(
            "/invalid",
            post(|| async {
                (
                    [(axum::http::header::CONTENT_TYPE, "application/json")],
                    "SECRET_SENTINEL in an invalid body",
                )
                    .into_response()
            }),
        )
        .route(
            "/large",
            post(|| async {
                (
                    [(axum::http::header::CONTENT_TYPE, "application/json")],
                    "x".repeat(REMOTE_MCP_MAX_RESPONSE_BYTES + 1),
                )
                    .into_response()
            }),
        );
    let http_task = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    let client = http_client::McpHttpClient(reqwest::Client::new());
    for path in ["/invalid", "/large"] {
        let request: ClientJsonRpcMessage = serde_json::from_value(json!({
            "jsonrpc":"2.0", "id":1, "method":"tools/list", "params":{}
        }))
        .unwrap();
        let error = client
            .post_message(
                format!("{origin}{path}").into(),
                request,
                None,
                None,
                HashMap::new(),
            )
            .await
            .expect_err("invalid/oversized body must fail");
        assert!(!error.to_string().contains("SECRET_SENTINEL"));
    }
    http_task.abort();
}

#[tokio::test]
async fn managed_regression_remote_wrapper_uses_http_connector_without_spawning() {
    let server = crate::tests::make_server();
    let definition = json!({
        "transport":"stdio", "command":"/missing-fixture/mcp-remote",
        "args":["http://127.0.0.1:1/mcp"]
    });
    let error = server
        .discover_mcp_tools("mcp:wrapper", &definition)
        .await
        .unwrap_err();
    assert!(
        error.contains("loopback"),
        "must reach HTTP URL policy, got: {error}"
    );
    assert!(!error.contains("spawn failed"));
}

#[tokio::test]
async fn managed_regression_http_lifecycle_errors_never_include_url_credentials() {
    use rmcp::transport::streamable_http_client::StreamableHttpClient;
    crate::ensure_tls_provider();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint: Arc<str> = format!(
        "http://{}/failure?key=URL_CREDENTIAL_SENTINEL",
        listener.local_addr().unwrap()
    )
    .into();
    let app = axum::Router::new().route(
        "/failure",
        axum::routing::any(|| async { axum::http::StatusCode::BAD_REQUEST }),
    );
    let task = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    let client = http_client::McpHttpClient(reqwest::Client::new());
    let get = client
        .get_stream(endpoint.clone(), None, None, None, HashMap::new())
        .await;
    let get = match get {
        Ok(_) => panic!("expected failed GET"),
        Err(error) => error,
    };
    let delete = client
        .delete_session(endpoint, "session".into(), None, HashMap::new())
        .await
        .unwrap_err();
    task.abort();
    for error in [get, delete] {
        assert!(
            !format!("{error:?} {error}").contains("URL_CREDENTIAL_SENTINEL"),
            "transport error exposed the expanded URL"
        );
    }
}

#[tokio::test]
async fn managed_regression_queued_call_cannot_switch_to_reconfigured_connection() {
    crate::ensure_tls_provider();
    let calls = Arc::new(Mutex::new(Vec::new()));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = format!("http://{}/mcp", listener.local_addr().unwrap());
    let app = axum::Router::new()
        .route("/mcp", post(upstream))
        .with_state(calls.clone());
    let task = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    let server = crate::tests::make_server();
    let mut cap = crate::tests::make_mcp_capability("mcp:queued", 1);
    let mut definition = json!({"transport":"http", "url":endpoint, "max_concurrency":1});
    crate::mcp_proxy::set_mcp_discovery_success(&mut definition, &[upstream_tool("invoke")]);
    cap.definition = definition.to_string();
    server
        .with_global_store(|store| store.hub_register(&cap).map_err(|error| error.to_string()))
        .unwrap();
    let permit = server
        .pool
        .acquire_call_permit(&server.mcp_quota_key(&cap.id), 1)
        .await;
    let mut waiting = Box::pin(server.proxy_call_internal("queued", "invoke", None));
    assert!(
        futures::poll!(waiting.as_mut()).is_pending(),
        "call must be queued"
    );
    definition["permissions"] = json!({"deny":["invoke"]});
    cap.definition = definition.to_string();
    server
        .with_global_store(|store| store.hub_register(&cap).map_err(|error| error.to_string()))
        .unwrap();
    let transport = StreamableHttpClientTransport::with_client(
        http_client::McpHttpClient(reqwest::Client::new()),
        rmcp::transport::streamable_http_client::StreamableHttpClientTransportConfig::with_uri(
            endpoint,
        ),
    );
    let replacement = legacy_client_config().serve(transport).await.unwrap();
    server.pool.install_test_connection(
        &server.mcp_connection_key(
            &cap,
            None,
            server
                .get_effective_sandbox_policy(None, &cap.id)
                .0
                .as_ref(),
        ),
        replacement,
    );
    drop(permit);
    let result = waiting.await;
    server.pool.remove_connection("queued");
    task.abort();
    assert!(
        result.is_err(),
        "a queued call must not execute using replacement credentials or permissions"
    );
    assert!(
        calls.lock().unwrap().is_empty(),
        "replacement upstream must receive no tool call"
    );
    let saved = server
        .with_global_store_read(|store| store.hub_get(&cap.id).map_err(|error| error.to_string()))
        .unwrap()
        .unwrap();
    assert_eq!(
        saved.fail_streak, 0,
        "a refused stale call must not degrade the replacement connection"
    );
}
