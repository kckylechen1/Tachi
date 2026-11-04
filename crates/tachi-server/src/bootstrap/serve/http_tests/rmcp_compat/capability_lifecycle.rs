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
fn owner_operations_keep_one_discovery_surface_and_do_not_leak_authority() {
    let temp = tempfile::tempdir().expect("tempdir");
    with_tachi_home(temp.path(), || {
        test_runtime().block_on(async {
            let global = temp.path().join("global/memory.db");
            let server = crate::MemoryServer::new(global.clone(), None).expect("server");
            // Even an internal owner's base policy cannot leak into HTTP clients.
            server.set_tool_profile(Some(tachi_hub::ToolProfile::admin()));
            let (daemon, cancel, task) = spawn_test_http_daemon(server, &global).await;
            let client = reqwest::Client::new();
            let token = daemon.internal_proxy_token.as_deref();
            for (offset, (capability, profile, owner_tool, count)) in [
                (None, None, None, Some(6)),
                (None, Some("ops"), None, None),
                (None, Some("admin"), None, None),
                (token, Some("ops"), None, None),
                (token, Some("admin"), None, None),
                (token, None, None, Some(6)),
                (token, None, Some("runtime_info"), Some(6)),
                (None, None, Some("runtime_info"), None),
                (Some("invalid-fixture-capability"), None, Some("runtime_info"), None),
                (token, Some("delegate"), Some("runtime_info"), None),
                (token, Some("observe"), Some("runtime_info"), None),
                (token, Some("standard+delegate"), Some("runtime_info"), None),
                (token, Some("delegate+standard"), Some("runtime_info"), None),
                (token, Some("worker"), None, Some(5)),
                (None, None, None, Some(6)),
            ].into_iter().enumerate() {
                let id = 200 + offset as i64;
                let mut identity = json!({});
                if let Some(profile) = profile {
                    identity["tachiProfile"] = json!(profile);
                }
                let mut request = client.post(&daemon.url)
                    .headers(http_headers(&[("mcp-protocol-version", "2026-07-28"), ("mcp-method", "tools/list")]))
                    .json(&json!({"jsonrpc":"2.0", "id":id, "method":"tools/list", "params":{"_meta":modern_meta(identity)}}));
                if let Some(capability) = capability {
                    request = request.header(crate::session_identity::HEADER_INTERNAL_PROXY_TOKEN, capability);
                }
                if let Some(tool) = owner_tool {
                    request = request.header(crate::session_identity::HEADER_OWNER_TOOL, tool);
                }
                let response = request.send().await.expect("list response");
                assert_eq!(response.status(), reqwest::StatusCode::OK, "case {offset}");
                assert!(!response.headers().contains_key("mcp-session-id"));
                let body = parse_http_mcp_payload(&response.text().await.expect("body"), id);
                if let Some(count) = count {
                    let names = body["result"]["tools"].as_array().expect("tools")
                        .iter().filter_map(|tool| tool["name"].as_str())
                        .collect::<std::collections::BTreeSet<_>>();
                    let mut expected = std::collections::BTreeSet::from([
                        "tachi_a2a", "tachi_memory", "tachi_staff", "tachi_task", "vault_connections"
                    ]);
                    if count == 6 { expected.insert("tachi_agent_eval"); }
                    assert_eq!(names, expected, "case {offset} discovery isolation");
                    assert_eq!(body["result"]["resultType"], "complete");
                } else {
                    assert_eq!(body["error"]["code"], -32602, "case {offset}: {body:#}");
                    assert!(body.get("result").is_none());
                }
            }
            let owner_client = privileged_fixture_client(&daemon);
            for metadata in [json!({}), json!({"tachiOwnerTool":"runtime_info"})] {
                let ordinary = modern_http_tool_call(&owner_client, &daemon.url, 300,
                    "runtime_info", metadata, json!({})).await;
                assert_eq!(ordinary["result"]["isError"], true,
                    "capability and forged metadata must not grant management access");
            }
            let owner = modern_http_tool_call_with_headers(&owner_client, &daemon.url, 301,
                "runtime_info", json!({}), json!({}),
                &[(crate::session_identity::HEADER_OWNER_TOOL, "runtime_info")]).await;
            assert_ne!(owner["result"]["isError"], true, "{owner:#}");
            assert!(serde_json::from_str::<serde_json::Value>(&http_tool_text(&owner)).is_ok(),
                "authorized runtime_info must actually execute");
            let mismatch = modern_http_tool_call_with_headers(&owner_client, &daemon.url, 302,
                "tachi_memory", json!({}), json!({"action":"search","query":"test"}),
                &[(crate::session_identity::HEADER_OWNER_TOOL, "runtime_info")]).await;
            assert_eq!(mismatch["error"]["code"], -32602, "{mismatch:#}");
            let after = modern_http_tool_call(&owner_client, &daemon.url, 303,
                "runtime_info", json!({}), json!({})).await;
            assert_eq!(after["result"]["isError"], true, "owner authority must not persist");

            // Legacy initialization does not retain owner authority. Each
            // tool call must carry both current headers independently.
            let (legacy, headers, init) = http_mcp_initialize(&daemon.url, http_headers(&[
                (crate::session_identity::HEADER_INTERNAL_PROXY_TOKEN, token.expect("token")),
                (crate::session_identity::HEADER_OWNER_TOOL, "runtime_info"),
            ]), None).await;
            assert!(init.get("error").is_none(), "{init:#}");
            http_mcp_initialized(&legacy, &daemon.url, headers.clone()).await;
            let list = http_mcp_list_tools(&legacy, &daemon.url, headers.clone(), 304).await;
            assert_eq!(list["result"]["tools"].as_array().expect("legacy tools").len(), 6);
            let allowed = http_mcp_call_tool(&legacy, &daemon.url, headers.clone(), 305,
                "runtime_info", serde_json::Map::new()).await;
            assert!(serde_json::from_str::<serde_json::Value>(&http_tool_text(&allowed)).is_ok());
            let denied = http_mcp_call_tool(&legacy, &daemon.url, headers.clone(), 306,
                "tachi_memory", serde_json::Map::new()).await;
            assert_eq!(denied["error"]["code"], -32602, "{denied:#}");
            for missing in [
                crate::session_identity::HEADER_OWNER_TOOL,
                crate::session_identity::HEADER_INTERNAL_PROXY_TOKEN,
            ] {
                let mut no_authority = headers.clone();
                no_authority.remove(missing);
                let denied = http_mcp_call_tool(&legacy, &daemon.url, no_authority, 307,
                    "runtime_info", serde_json::Map::new()).await;
                assert!(denied.get("error").is_some() || denied["result"]["isError"] == true,
                    "initialize must not retain missing {missing}: {denied:#}");
            }
            let (worker, mut worker_headers, init) = http_mcp_initialize(&daemon.url,
                http_headers(&[(crate::session_identity::HEADER_PROFILE, "delegate")]), None).await;
            assert!(init.get("error").is_none(), "{init:#}");
            http_mcp_initialized(&worker, &daemon.url, worker_headers.clone()).await;
            worker_headers.insert(crate::session_identity::HEADER_PROFILE,
                reqwest::header::HeaderValue::from_static("standard"));
            worker_headers.insert(crate::session_identity::HEADER_OWNER_TOOL,
                reqwest::header::HeaderValue::from_static("runtime_info"));
            worker_headers.insert(crate::session_identity::HEADER_INTERNAL_PROXY_TOKEN,
                reqwest::header::HeaderValue::from_str(token.expect("token")).unwrap());
            let denied = http_mcp_call_tool(&worker, &daemon.url, worker_headers, 308,
                "runtime_info", serde_json::Map::new()).await;
            assert_eq!(denied["error"]["code"], -32602,
                "current headers cannot override a bound worker session: {denied:#}");
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
