use super::*;

fn non_tool_routes() -> Vec<(&'static str, serde_json::Value)> {
    std::iter::once(("server/discover", json!({})))
        .chain(
            default_legacy_methods()
                .into_iter()
                .map(|(method, params, _)| (method, params)),
        )
        .collect()
}

#[test]
fn modern_http_non_tool_routes_enforce_identity_agreement() {
    let temp = tempfile::tempdir().expect("tempdir");
    with_tachi_home(temp.path(), || {
        let global = temp.path().join("global/memory.db");
        test_runtime().block_on(async {
            let server = crate::MemoryServer::new(global.clone(), None).expect("server");
            let (daemon, cancel, task) = spawn_test_http_daemon(server, &global).await;
            let client = reqwest::Client::new();
            let mut observations = Vec::new();
            for (method, params) in non_tool_routes() {
                for (case, identity, expected_code) in [
                    (
                        "canonical matching",
                        json!({"tachiClient":"header-client"}),
                        None,
                    ),
                    (
                        "alias matching",
                        json!({"tachi.client":"header-client"}),
                        None,
                    ),
                    (
                        "canonical conflict",
                        json!({"tachiClient":"body-client"}),
                        Some(-32020),
                    ),
                    (
                        "alias conflict",
                        json!({"tachi.client":"body-client"}),
                        Some(-32020),
                    ),
                    (
                        "conflicting aliases",
                        json!({"tachiClient":"header-client", "tachi.client":"other-client"}),
                        Some(-32602),
                    ),
                    (
                        "malformed identity",
                        json!({"tachiClient":42}),
                        Some(-32602),
                    ),
                ] {
                    let mut params = params.clone();
                    params["_meta"] = modern_meta(identity);
                    let response = client
                        .post(&daemon.url)
                        .headers(http_headers(&[
                            ("mcp-protocol-version", "2026-07-28"),
                            ("mcp-method", method),
                            ("x-tachi-client", "header-client"),
                        ]))
                        .json(&json!({"jsonrpc":"2.0", "id":90, "method":method, "params":params}))
                        .send()
                        .await
                        .expect("HTTP request");
                    let status = response.status();
                    assert!(!response.headers().contains_key("mcp-session-id"));
                    let payload = parse_http_mcp_payload(&response.text().await.expect("body"), 90);
                    observations.push((method, case, expected_code, status, payload));
                }
            }
            cancel.cancel();
            task.await.expect("daemon task");
            let mut violations = Vec::new();
            for (method, case, expected_code, status, payload) in observations {
                // RMCP maps malformed modern params to HTTP 400; Tachi
                // HEADER_MISMATCH remains a JSON-RPC error over HTTP 200.
                let expected_status = if expected_code == Some(-32602) {
                    reqwest::StatusCode::BAD_REQUEST
                } else {
                    reqwest::StatusCode::OK
                };
                assert_eq!(status, expected_status, "{method} / {case}: {payload}");
                match expected_code {
                    None => assert!(
                        payload.get("result").is_some() && payload.get("error").is_none(),
                        "matching identity must succeed: {method} / {case}: {payload}"
                    ),
                    Some(code)
                        if payload["error"]["code"] == code && payload.get("result").is_none() => {}
                    Some(code) => violations
                        .push(format!("{method} / {case}: expected {code}, got {payload}")),
                }
            }
            assert!(
                violations.is_empty(),
                "modern HTTP identity disagreement accepted:\n{}",
                violations.join("\n")
            );
        });
    });
}

#[test]
fn modern_stdio_non_tool_routes_enforce_process_identity() {
    let temp = tempfile::tempdir().expect("tempdir");
    with_tachi_home(temp.path(), || {
        let _agent = EnvRestore::remove(crate::session_identity::ENV_AGENT_IDENTITY);
        test_runtime().block_on(async {
            let mut requests = Vec::new();
            let mut cases = Vec::new();
            for (method, params) in non_tool_routes() {
                for (case, identity, valid) in [
                    ("matching", json!({"tachiProfile":"standard", "tachiClient":"client"}), true),
                    ("matching alias", json!({"tachi.profile":"standard", "tachi.client":"client"}), true),
                    ("profile drift", json!({"tachiProfile":"admin"}), false),
                    ("project drift", json!({"tachiProject":"not-process-project"}), false),
                    ("alias conflict", json!({"tachiClient":"client", "tachi.client":"other"}), false),
                    ("malformed client", json!({"tachiClient":42}), false),
                    ("malformed agent", json!({"tachiAgentIdentity":"agent invalid"}), false),
                ] {
                    let mut params = params.clone();
                    params["_meta"] = modern_meta(identity);
                    requests.push(json!({"jsonrpc":"2.0", "id":requests.len() as i64, "method":method, "params":params}));
                    cases.push((method, case, valid));
                }
            }
            let responses = stdio_responses(identity_probe_proxy(), &requests).await;
            assert_eq!(responses.len(), cases.len());
            let mut violations = Vec::new();
            for ((method, case, valid), payload) in cases.into_iter().zip(responses) {
                if valid {
                    assert!(payload.get("result").is_some() && payload.get("error").is_none(),
                        "matching process identity must succeed: {method} / {case}: {payload}");
                } else if payload["error"]["code"] != -32602 || payload.get("result").is_some() {
                    violations.push(format!("{method} / {case}: expected invalid params, got {payload}"));
                }
            }
            assert!(violations.is_empty(), "modern stdio process identity disagreement accepted:\n{}", violations.join("\n"));
        });
    });
}

#[test]
fn modern_stdio_runtime_info_has_complete_envelope_and_legacy_shape_is_preserved() {
    let temp = tempfile::tempdir().expect("tempdir");
    with_tachi_home(temp.path(), || {
        test_runtime().block_on(async {
            let mut proxy = identity_probe_proxy();
            proxy.app_home = temp.path().to_path_buf();
            proxy.global_db_path = temp.path().join("global/memory.db");
            let responses = stdio_responses(proxy, &[
                json!({"jsonrpc":"2.0", "id":1, "method":"initialize", "params":{
                    "protocolVersion":"2024-11-05", "capabilities":{}, "clientInfo":{"name":"legacy","version":"1"}
                }}),
                json!({"jsonrpc":"2.0", "method":"notifications/initialized"}),
                json!({"jsonrpc":"2.0", "id":2, "method":"tools/call", "params":{"name":"runtime_info", "arguments":{}}}),
                json!({"jsonrpc":"2.0", "id":3, "method":"tools/call", "params":{
                    "name":"runtime_info", "arguments":{}, "_meta":modern_meta(json!({}))
                }}),
            ]).await;
            assert_eq!(responses[0]["result"]["protocolVersion"], "2024-11-05");
            for response in &responses[1..] {
                let body: serde_json::Value = serde_json::from_str(&http_tool_text(response)).expect("runtime JSON");
                assert_eq!(body["mode"], "stdio_proxy");
                assert_eq!(body["daemon"]["reachable"], false);
                assert!(response.get("error").is_none(), "{response}");
            }
            assert!(responses[1]["result"].get("resultType").is_none(), "legacy envelope changed");
            assert_eq!(responses[2]["result"]["resultType"], "complete", "modern runtime_info must carry complete result discriminator");
        });
    });
}
