use super::*;

#[test]
fn modern_ops_proxy_refreshes_rotated_capability_at_same_url_for_list_and_call() {
    let temp = tempfile::tempdir().expect("tempdir");
    with_tachi_home(temp.path(), || {
        let _disable_auto = EnvRestore::set("TACHI_DISABLE_AUTO_DAEMON", "1");
        test_runtime().block_on(async {
            let global = temp.path().join("global/memory.db");
            let server = crate::MemoryServer::new(global.clone(), None).expect("server");
            let (daemon, cancel, task) = spawn_test_http_daemon(server.clone(), &global).await;
            let proxy = StdioProxyServer {
                adapter_started_at: chrono::Utc::now(),
                tool_profile: Some(tachi_hub::ToolProfile::operate()),
                resolved_agent_identity: Default::default(),
                daemon: std::sync::Arc::new(std::sync::RwLock::new(daemon.clone())),
                app_home: temp.path().to_path_buf(),
                global_db_path: global.clone(),
                project_db_path: None,
                client_project: None,
            };
            let receipt = crate::daemon_lock::scoped_daemon_pid_path(temp.path(), &global);
            let port = daemon
                .url
                .strip_prefix("http://127.0.0.1:")
                .and_then(|rest| rest.split('/').next())
                .expect("port")
                .parse::<u16>()
                .expect("port number");
            let mut failures = Vec::new();
            for (id, method, extra) in [
                (1, "tools/list", json!({})),
                (
                    2,
                    "tools/call",
                    json!({"name":"tachi_status", "arguments":{}}),
                ),
            ] {
                let token = uuid::Uuid::new_v4().simple().to_string();
                server.set_daemon_proxy_token(token.clone());
                crate::utils::write_json_file_owner_only(&receipt, &json!({"pid":std::process::id(), "port":port, "url":daemon.url, "global_db":global, "version":env!("CARGO_PKG_VERSION"), "internal_proxy_token":token})).expect("rotate fixture receipt");
                let mut params = extra;
                params["_meta"] = modern_meta(json!({}));
                let responses = stdio_responses(
                    proxy.clone(),
                    &[json!({"jsonrpc":"2.0", "id":id, "method":method, "params":params})],
                )
                .await;
                let response = &responses[0];
                if response.get("error").is_some()
                    || response["result"]["isError"] == true
                    || response["result"]["resultType"] != "complete"
                {
                    failures.push(format!(
                        "{method} failed after same-URL capability rotation: {response}"
                    ));
                } else if method == "tools/list" {
                    assert_eq!(
                        response["result"]["tools"].as_array().expect("tools").len(),
                        39
                    );
                }
                if proxy.current_daemon().internal_proxy_token.as_deref() != Some(token.as_str()) {
                    failures.push(format!("{method} did not retain refreshed daemon identity"));
                }
            }
            let current = proxy.current_daemon();
            let identity = proxy
                .resolve_request_identity(crate::mcp_peer::McpPeerMode::Legacy, &Default::default())
                .expect("identity");
            assert!(
                proxy.refresh_daemon(&current, &identity).await.is_none(),
                "unchanged URL and capability must not trigger a redundant retry"
            );
            let mut ordinary = proxy.clone();
            ordinary.tool_profile = Some(tachi_hub::ToolProfile::standard());
            let listed = stdio_responses(ordinary, &[json!({"jsonrpc":"2.0", "id":3, "method":"tools/list", "params":{"_meta":modern_meta(json!({}))}})]).await;
            assert_eq!(
                listed[0]["result"]["tools"]
                    .as_array()
                    .expect("ordinary tools")
                    .len(),
                5,
                "capability must not implicitly widen the ordinary profile"
            );
            let (_, _, denied) = http_mcp_initialize(
                &daemon.url,
                http_headers(&[(crate::session_identity::HEADER_PROFILE, "ops")]),
                None,
            )
            .await;
            assert_eq!(
                denied["error"]["code"], -32602,
                "no-token Ops must remain denied"
            );
            cancel.cancel();
            task.await.expect("daemon stopped");
            assert!(
                failures.is_empty(),
                "same-URL daemon identity recovery failed:\n{}",
                failures.join("\n")
            );
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
                (None, None, Some(5)),
                (None, Some("ops"), None),
                (None, Some("admin"), None),
                (Some("invalid-fixture-capability"), Some("ops"), None),
                (daemon.internal_proxy_token.as_deref(), None, Some(5)),
                (
                    daemon.internal_proxy_token.as_deref(),
                    Some("ops"),
                    Some(39),
                ),
                (
                    daemon.internal_proxy_token.as_deref(),
                    Some("admin"),
                    Some(83),
                ),
                (
                    daemon.internal_proxy_token.as_deref(),
                    Some("worker"),
                    Some(5),
                ),
                (None, None, Some(5)),
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
                    assert_eq!(body["result"]["resultType"], "complete");
                } else {
                    assert_eq!(status, reqwest::StatusCode::BAD_REQUEST, "case {offset}");
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
