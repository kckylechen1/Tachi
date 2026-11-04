use super::*;

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
                    "tachiProfile": "standard"
                })),
            )
            .await;
            assert!(init.get("error").is_none(), "{init:#}");
            assert_eq!(init["result"]["protocolVersion"], "2024-11-05");
            assert!(headers.contains_key("mcp-session-id"));
            http_mcp_initialized(&client, &daemon.url, headers.clone()).await;
            let listed = http_mcp_list_tools(&client, &daemon.url, headers.clone(), 2).await;
            let names = listed["result"]["tools"]
                .as_array()
                .expect("legacy tools/list result")
                .iter()
                .filter_map(|tool| tool["name"].as_str())
                .collect::<std::collections::BTreeSet<_>>();
            assert_eq!(
                names,
                std::collections::BTreeSet::from([
                    "tachi_a2a",
                    "tachi_agent_eval",
                    "tachi_memory",
                    "tachi_staff",
                    "tachi_task",
                    "vault_connections",
                ])
            );
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
fn modern_http_capability_requires_explicit_profile_and_never_leaks_between_requests() {
    let temp = tempfile::tempdir().expect("tempdir");
    with_tachi_home(temp.path(), || {
        test_runtime().block_on(async {
            let global = temp.path().join("global/memory.db");
            let server = crate::MemoryServer::new(global.clone(), None).expect("server");
            server.set_tool_profile(Some(tachi_hub::ToolProfile::admin()));
            let (daemon, cancel, task) = spawn_test_http_daemon(server, &global).await;
            let client = reqwest::Client::new();
            for (offset, (capability, profile, count)) in [
                (None, None, Some(6)),
                (None, Some("ops"), None),
                (None, Some("admin"), None),
                (Some("invalid-fixture-capability"), Some("ops"), None),
                (daemon.internal_proxy_token.as_deref(), None, Some(6)),
                (
                    daemon.internal_proxy_token.as_deref(),
                    Some("ops"),
                    Some(36),
                ),
                (
                    daemon.internal_proxy_token.as_deref(),
                    Some("admin"),
                    Some(61),
                ),
                (
                    daemon.internal_proxy_token.as_deref(),
                    Some("worker"),
                    Some(5),
                ),
                (None, None, Some(6)),
            ]
            .into_iter()
            .enumerate()
            {
                let id = 200 + offset as i64;
                let mut identity = json!({});
                if let Some(profile) = profile {
                    identity["tachiProfile"] = json!(profile);
                }
                let mut request = client.post(&daemon.url).headers(http_headers(&[("mcp-protocol-version", "2026-07-28"), ("mcp-method", "tools/list")])).json(&json!({"jsonrpc":"2.0", "id":id, "method":"tools/list", "params":{"_meta":modern_meta(identity)}}));
                if let Some(capability) = capability {
                    request = request.header(
                        crate::session_identity::HEADER_INTERNAL_PROXY_TOKEN,
                        capability,
                    );
                }
                let response = request.send().await.expect("list response");
                let status = response.status();
                assert!(!response.headers().contains_key("mcp-session-id"));
                let body = parse_http_mcp_payload(&response.text().await.expect("body"), id);
                if let Some(count) = count {
                    assert_eq!(status, reqwest::StatusCode::OK, "case {offset}");
                    assert_eq!(
                        body["result"]["tools"].as_array().expect("tools").len(),
                        count,
                        "case {offset}"
                    );
                    if profile == Some("admin") {
                        let names = body["result"]["tools"].as_array().expect("tools")
                            .iter().filter_map(|tool| tool["name"].as_str())
                            .collect::<std::collections::BTreeSet<_>>();
                        assert!(names.contains("tachi_sandbox"), "canonical sandbox route must remain");
                        for retired in [
                            "sandbox_set_rule", "sandbox_check", "sandbox_set_policy",
                            "sandbox_get_policy", "sandbox_list_policies", "sandbox_exec_audit",
                        ] {
                            assert!(!names.contains(retired), "retired alias {retired} must not appear in admin HTTP tools/list");
                        }
                    }
                    if count == 6 || count == 5 {
                        let names = body["result"]["tools"].as_array().expect("tools")
                            .iter().filter_map(|tool| tool["name"].as_str())
                            .collect::<std::collections::BTreeSet<_>>();
                        let mut expected = std::collections::BTreeSet::from([
                            "tachi_a2a", "tachi_memory", "tachi_staff", "tachi_task", "vault_connections"
                        ]);
                        if count == 6 {
                            expected.insert("tachi_agent_eval");
                        }
                        assert_eq!(names, expected, "case {offset} profile isolation");
                    }
                    assert_eq!(body["result"]["resultType"], "complete");
                } else {
                    assert_eq!(status, reqwest::StatusCode::OK, "case {offset}");
                    assert_eq!(body["error"]["code"], -32602, "case {offset}");
                    assert!(body.get("result").is_none());
                }
            }
            let ordinary = modern_http_tool_call(
                &privileged_fixture_client(&daemon),
                &daemon.url,
                300,
                "runtime_info",
                json!({}),
                json!({}),
            )
            .await;
            assert_eq!(
                ordinary["result"]["isError"], true,
                "capability alone must not expose Ops tools"
            );
            let ops = modern_http_tool_call(
                &privileged_fixture_client(&daemon),
                &daemon.url,
                301,
                "runtime_info",
                json!({"tachiProfile":"ops"}),
                json!({}),
            )
            .await;
            assert_ne!(ops["result"]["isError"], true);
            assert!(
                ops["result"]["content"][0]["text"]
                    .as_str()
                    .and_then(|text| serde_json::from_str::<serde_json::Value>(text).ok())
                    .is_some(),
                "authorized runtime_info must actually execute"
            );
            cancel.cancel();
            task.await.expect("daemon stopped");
        })
    });
}

#[test]
fn http_reconnects_to_stable_endpoint_with_fresh_session_and_rotated_owner_capability() {
    let home = tempfile::tempdir().unwrap();
    with_tachi_home(home.path(), || {
        test_runtime().block_on(async {
            let global = home.path().join("global/memory.db");
            let server = crate::MemoryServer::new(global.clone(), None).unwrap();
            let (first, stop, task) = spawn_test_http_daemon(server, &global).await;
            let public_headers = http_headers(&[("x-tachi-profile", "standard"), ("x-tachi-client", "restart-client"), ("x-tachi-agent-identity", "agent.restart")]);
            let (client, old_headers, init) = http_mcp_initialize(&first.url, public_headers.clone(), None).await;
            assert!(init.get("error").is_none(), "{init:#}");
            http_mcp_initialized(&client, &first.url, old_headers.clone()).await;
            let save = http_mcp_call_tool(&client, &first.url, old_headers.clone(), 2, "tachi_memory", serde_json::from_value(json!({
                "action":"save", "id":"http-restart-write", "text":"Durable HTTP reconnect fixture", "summary":"HTTP reconnect fixture", "scope":"global", "path":"/notes/http-reconnect", "category":"fact", "force":true
            })).unwrap()).await;
            assert!(save.get("error").is_none() && save["result"]["isError"] != true, "{save:#}");
            assert_eq!(memory_id_count(&global, "http-restart-write"), 1);
            let port = reqwest::Url::parse(&first.url).unwrap().port().unwrap();
            stop.cancel();
            task.await.unwrap();
            drop(client);

            let replacement = crate::MemoryServer::new(global.clone(), None).unwrap();
            let (second, stop, task) = spawn_test_http_daemon_on(replacement, &global, None, None, port).await;
            assert_eq!(second.url, first.url);
            assert_ne!(second.internal_proxy_token, first.internal_proxy_token);
            let client = reqwest::Client::new();
            let stale = client.post(&second.url).headers(old_headers.clone()).json(&json!({"jsonrpc":"2.0", "id":3, "method":"tools/list", "params":{}})).send().await.unwrap();
            assert!(!stale.status().is_success(), "old session must not survive a daemon restart");
            let (_, _, rejected) = http_mcp_initialize(&second.url, http_headers(&[("x-tachi-profile", "admin"), (crate::session_identity::HEADER_INTERNAL_PROXY_TOKEN, first.internal_proxy_token.as_deref().unwrap())]), None).await;
            assert!(rejected.get("error").is_some(), "old capability must not authorize: {rejected:#}");
            let (client, headers, init) = http_mcp_initialize(&second.url, public_headers, None).await;
            assert!(init.get("error").is_none(), "{init:#}");
            assert_ne!(headers.get("mcp-session-id"), old_headers.get("mcp-session-id"));
            http_mcp_initialized(&client, &second.url, headers.clone()).await;
            let listed = http_mcp_list_tools(&client, &second.url, headers.clone(), 4).await;
            assert_eq!(listed["result"]["tools"].as_array().unwrap().len(), 6);
            let status = http_mcp_call_tool(&client, &second.url, headers, 5, "tachi_a2a", serde_json::from_value(json!({"action":"status"})).unwrap()).await;
            let status: serde_json::Value = serde_json::from_str(&http_tool_text(&status)).unwrap();
            assert_eq!(status["actor_agent_identity_id"], "agent.restart");
            assert_eq!(memory_id_count(&global, "http-restart-write"), 1, "reconnection never replays the saved write");
            stop.cancel(); task.await.unwrap();
        });
    });
}
