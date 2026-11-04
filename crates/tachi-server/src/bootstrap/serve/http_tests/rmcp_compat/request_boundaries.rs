use super::*;
use serde_json::Value;

#[test]
fn modern_http_failed_facade_calls_carry_recovery_guidance() {
    let temp = tempfile::tempdir().expect("tempdir");
    with_tachi_home(temp.path(), || {
        let global = temp.path().join("global/memory.db");
        test_runtime().block_on(async {
            let server = crate::MemoryServer::new(global.clone(), None).expect("server");
            let (daemon, cancel, task) = spawn_test_http_daemon(server, &global).await;
            let client = reqwest::Client::new();
            let mut observations = Vec::new();
            for (id, action, replay) in [(201, "get", true), (202, "save", false)] {
                let mut arguments = json!({"action":action});
                if action == "save" {
                    // Reach the facade's missing-content error after destination admission.
                    arguments["storage_scope"] = json!("global");
                }
                let response = client
                    .post(&daemon.url)
                    .headers(http_headers(&[
                        ("mcp-protocol-version", "2026-07-28"),
                        ("mcp-method", "tools/call"),
                        ("mcp-name", "tachi_memory"),
                    ]))
                    .json(&json!({"jsonrpc":"2.0", "id":id, "method":"tools/call",
                        "params":{"name":"tachi_memory", "arguments":arguments,
                            "_meta":modern_meta(json!({"tachiProfile":"standard"}))}}))
                    .send()
                    .await
                    .expect("request");
                let payload = parse_http_mcp_payload(&response.text().await.expect("body"), id);
                observations.push((action, replay, payload));
            }
            cancel.cancel();
            task.await.expect("daemon task");
            for (action, replay, payload) in observations {
                let guidance = payload
                    .pointer("/error/data/tachi_invocation_guidance")
                    .cloned()
                    .or_else(|| {
                        payload
                            .pointer("/result/content")
                            .and_then(Value::as_array)
                            .and_then(|blocks| {
                                blocks
                                    .iter()
                                    .filter_map(|block| {
                                        serde_json::from_str::<Value>(block.get("text")?.as_str()?)
                                            .ok()
                                    })
                                    .find_map(|value| {
                                        value.get("tachi_invocation_guidance").cloned()
                                    })
                            })
                    })
                    .unwrap_or_else(|| panic!("{action} missing recovery guidance: {payload}"));
                assert!(
                    payload.get("error").is_some() || payload["result"]["isError"] == true,
                    "an incomplete request must remain an error: {payload}"
                );
                assert_eq!(guidance["invocation"], "failed");
                assert_eq!(guidance["actual_effects"], "not_established");
                assert_eq!(guidance["replay_authorization"]["authorized"], replay);
            }
        });
    });
}

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
                // Header mismatches are HTTP 400; handler-level identity
                // errors remain JSON-RPC responses over HTTP 200.
                let expected_status = if expected_code == Some(-32020) {
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
