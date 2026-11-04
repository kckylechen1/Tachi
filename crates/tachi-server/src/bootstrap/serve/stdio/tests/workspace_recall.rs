//! Workspace-first recall across direct HTTP and the stdio proxy.

use super::*;

fn assert_memory_search_source(
    parsed: &serde_json::Value,
    expected_id: &str,
    expected_db: &str,
    expected_store: serde_json::Value,
) {
    assert_search_section_rows_are_objects(parsed);
    assert_eq!(parsed["status"], "completed", "{parsed:#}");
    let sections = parsed["sections"].as_array().expect("search sections");
    assert_eq!(sections.len(), 1, "memory-only search: {parsed:#}");
    assert_eq!(sections[0]["name"], "Memory", "{parsed:#}");
    let rows = sections[0]["rows"].as_array().expect("memory rows");
    assert_eq!(
        rows.len(),
        1,
        "search must stop at the first matching library: {parsed:#}"
    );
    assert_eq!(rows[0]["id"], expected_id, "{parsed:#}");
    assert_eq!(rows[0]["db"], expected_db, "{parsed:#}");
    assert_eq!(rows[0]["store"], expected_store, "{parsed:#}");
}

#[test]
fn stdio_proxy_memory_search_prefers_bound_project_before_global_fallback() {
    let _guard = crate::utils::global_test_lock()
        .lock()
        .unwrap_or_else(|e| e.into_inner());

    let temp = tempfile::tempdir().expect("tempdir");
    let tachi_home = temp.path().join("home");
    let global = tachi_home.join("global/memory.db");
    let project_name = "Sigil-proxy-search-e2e";
    let project = tachi_home
        .join("projects")
        .join(project_name)
        .join("memory.db");
    std::fs::create_dir_all(global.parent().expect("global parent")).expect("global parent");
    let _tachi_home = EnvRestore::set_path("TACHI_HOME", &tachi_home);
    let _sigil_home = EnvRestore::remove("SIGIL_HOME");
    let _app_home = EnvRestore::remove("TACHI_APP_HOME");
    let _embedding_disabled = EnvRestore::set("TACHI_SEARCH_DISABLE_QUERY_EMBEDDING", "1");
    seed_project_db(&tachi_home, &project);

    let rt = test_runtime();
    let (ct, daemon_task) = rt.block_on(async {
        let server = crate::MemoryServer::new(global.clone(), None).expect("daemon server");
        let (daemon, ct, daemon_task) = spawn_test_http_daemon(server, &global).await;
        let proxy = StdioProxyServer {
            adapter_started_at: chrono::Utc::now(),
            tool_profile: None,
            daemon: std::sync::Arc::new(std::sync::RwLock::new(daemon)),
            app_home: tachi_home.clone(),
            global_db_path: global.clone(),
            project_db_path: Some(project.clone()),
            client_project: Some(project_name.to_string()),
            resolved_agent_identity: Default::default(),
            rate_limit_session: ProxyRateLimitSession::mint(),
        };

        for (id, scope, summary) in [
            (
                "global-proxy-search-e2e",
                "global",
                "global PROXYREADMERGE row",
            ),
            (
                "project-proxy-search-e2e",
                "project",
                "project PROXYREADMERGE row",
            ),
            (
                "global-proxy-search-fallback-e2e",
                "global",
                "global PROXYREADFALLBACK row",
            ),
        ] {
            let result = call_tool_via_stdio_proxy(
                proxy.clone(),
                "tachi_memory",
                serde_json::Map::from_iter([
                    ("action".to_string(), serde_json::json!("save")),
                    ("id".to_string(), serde_json::json!(id)),
                    (
                        "text".to_string(),
                        serde_json::json!(format!("{summary} lossless text")),
                    ),
                    ("summary".to_string(), serde_json::json!(summary)),
                    (
                        "path".to_string(),
                        serde_json::json!("/notes/stdio-proxy-search-e2e"),
                    ),
                    ("category".to_string(), serde_json::json!("fact")),
                    ("scope".to_string(), serde_json::json!(scope)),
                    ("force".to_string(), serde_json::json!(true)),
                ]),
            )
            .await
            .unwrap_or_else(|err| panic!("seed {id}: {err}"));
            assert_tool_ok(&result);
        }

        for (query, expected_id, expected_db, expected_store) in [
            (
                "PROXYREADMERGE",
                "project-proxy-search-e2e",
                "project",
                serde_json::json!({"kind": "named_project", "project": project_name}),
            ),
            (
                "PROXYREADFALLBACK",
                "global-proxy-search-fallback-e2e",
                "global",
                serde_json::json!({"kind": "legacy_global"}),
            ),
        ] {
            let result = call_tool_via_stdio_proxy(
                proxy.clone(),
                "tachi_memory",
                serde_json::Map::from_iter([
                    ("action".to_string(), serde_json::json!("search")),
                    ("query".to_string(), serde_json::json!(query)),
                    ("scope".to_string(), serde_json::json!("memory")),
                    ("top_k".to_string(), serde_json::json!(10)),
                    ("format".to_string(), serde_json::json!("json")),
                ]),
            )
            .await
            .expect("proxied tachi_memory search should succeed");
            assert_tool_ok(&result);
            assert_memory_search_source(
                &first_text_json(&result),
                expected_id,
                expected_db,
                expected_store,
            );
        }

        (ct, daemon_task)
    });

    ct.cancel();
    rt.block_on(daemon_task).expect("daemon task");
    for id in [
        "global-proxy-search-e2e",
        "global-proxy-search-fallback-e2e",
    ] {
        assert_eq!(
            memory_access_count(&global, id),
            0,
            "workspace recall must not record access in the global fallback library"
        );
    }
}

#[test]
fn stdio_proxy_tachi_memory_search_rows_stay_objects_under_parallel_forwarding() {
    let _guard = crate::utils::global_test_lock()
        .lock()
        .unwrap_or_else(|e| e.into_inner());

    let temp = tempfile::tempdir().expect("tempdir");
    let tachi_home = temp.path().join("home");
    let global = tachi_home.join("global/memory.db");
    let project_name = "Sigil-proxy-row-shape-e2e";
    let project = tachi_home
        .join("projects")
        .join(project_name)
        .join("memory.db");
    std::fs::create_dir_all(global.parent().expect("global parent")).expect("global parent");
    let _tachi_home = EnvRestore::set_path("TACHI_HOME", &tachi_home);
    let _sigil_home = EnvRestore::remove("SIGIL_HOME");
    let _app_home = EnvRestore::remove("TACHI_APP_HOME");
    // This row-shape/forwarding contract must not inherit an ambient provider
    // key and fan out 32 provider-health writes against the fixture DB.
    let _embedding_disabled = EnvRestore::set("TACHI_SEARCH_DISABLE_QUERY_EMBEDDING", "1");
    seed_project_db(&tachi_home, &project);

    let rt = test_runtime();
    let (ct, daemon_task) = rt.block_on(async {
        let server = crate::MemoryServer::new(global.clone(), None).expect("daemon server");
        let (daemon, ct, daemon_task) = spawn_test_http_daemon(server, &global).await;
        let proxy = StdioProxyServer {
            adapter_started_at: chrono::Utc::now(),
            tool_profile: Some(tachi_hub::ToolProfile::operate()),
            daemon: std::sync::Arc::new(std::sync::RwLock::new(daemon)),
            app_home: tachi_home.clone(),
            global_db_path: global.clone(),
            project_db_path: Some(project.clone()),
            client_project: Some(project_name.to_string()),
            resolved_agent_identity: Default::default(),
            rate_limit_session: ProxyRateLimitSession::mint(),
        };

        for (id, scope, summary) in [
            (
                "global-proxy-row-shape-e2e",
                "global",
                "global PROXYROWSHAPE row",
            ),
            (
                "project-proxy-row-shape-e2e",
                "project",
                "project PROXYROWSHAPE row",
            ),
            (
                "global-proxy-row-shape-fallback-e2e",
                "global",
                "global PROXYROWFALLBACK row",
            ),
        ] {
            let result = call_tool_via_stdio_proxy(
                proxy.clone(),
                "tachi_memory",
                serde_json::Map::from_iter([
                    ("action".to_string(), serde_json::json!("save")),
                    ("id".to_string(), serde_json::json!(id)),
                    (
                        "text".to_string(),
                        serde_json::json!(format!("{summary} lossless text")),
                    ),
                    ("summary".to_string(), serde_json::json!(summary)),
                    (
                        "path".to_string(),
                        serde_json::json!("/notes/stdio-proxy-row-shape-e2e"),
                    ),
                    ("category".to_string(), serde_json::json!("fact")),
                    ("scope".to_string(), serde_json::json!(scope)),
                    ("force".to_string(), serde_json::json!(true)),
                ]),
            )
            .await
            .unwrap_or_else(|err| panic!("seed {id}: {err}"));
            assert_tool_ok(&result);
        }

        let search_args = |i: usize, query: &str| {
            serde_json::Map::from_iter([
                ("action".to_string(), serde_json::json!("search")),
                ("query".to_string(), serde_json::json!(query)),
                ("scope".to_string(), serde_json::json!("memory")),
                ("top_k".to_string(), serde_json::json!(10 + i)),
                ("format".to_string(), serde_json::json!("json")),
            ])
        };
        let calls = (0..32).map(|i| {
            let query = if i % 2 == 0 {
                "PROXYROWSHAPE"
            } else {
                "PROXYROWFALLBACK"
            };
            call_tool_via_stdio_proxy(proxy.clone(), "tachi_memory", search_args(i, query))
        });
        let results = futures::future::join_all(calls).await;

        for (i, result) in results.into_iter().enumerate() {
            let result = result.expect("proxied tachi_memory search should succeed");
            assert_tool_ok(&result);
            let parsed = first_text_json(&result);
            if i % 2 == 0 {
                assert_memory_search_source(
                    &parsed,
                    "project-proxy-row-shape-e2e",
                    "project",
                    serde_json::json!({"kind": "named_project", "project": project_name}),
                );
            } else {
                assert_memory_search_source(
                    &parsed,
                    "global-proxy-row-shape-fallback-e2e",
                    "global",
                    serde_json::json!({"kind": "legacy_global"}),
                );
            }
        }
        for id in [
            "global-proxy-row-shape-e2e",
            "global-proxy-row-shape-fallback-e2e",
        ] {
            assert_eq!(
                memory_access_count(&global, id),
                0,
                "parallel workspace recall must keep global fallback reads free of access writes"
            );
        }

        let global_proxy = StdioProxyServer {
            project_db_path: None,
            client_project: None,
            rate_limit_session: ProxyRateLimitSession::mint(),
            ..proxy.clone()
        };
        let calls = (0..32).map(|i| {
            call_tool_via_stdio_proxy(
                global_proxy.clone(),
                "tachi_memory",
                search_args(i, "PROXYROWSHAPE"),
            )
        });
        for result in futures::future::join_all(calls).await {
            let result = result.expect("global-only proxied search should succeed");
            assert_tool_ok(&result);
            assert_memory_search_source(
                &first_text_json(&result),
                "global-proxy-row-shape-e2e",
                "global",
                serde_json::json!({"kind": "legacy_global"}),
            );
        }

        let runtime = call_tool_via_stdio_proxy(proxy, "runtime_info", serde_json::Map::new())
            .await
            .expect("runtime_info should succeed");
        assert_tool_ok(&runtime);

        (ct, daemon_task)
    });

    ct.cancel();
    rt.block_on(daemon_task).expect("daemon task");
    assert_eq!(
        memory_access_count(&global, "global-proxy-row-shape-e2e"),
        32,
        "the isolated forwarding test must retain per-search access recording"
    );
    assert_eq!(
        memory_access_count(&global, "global-proxy-row-shape-fallback-e2e"),
        0,
        "fallback-only rows must not acquire access writes from parallel forwarding"
    );
}

#[test]
fn http_direct_connect_header_identity_binds_profile_and_project() {
    let _guard = crate::utils::global_test_lock()
        .lock()
        .unwrap_or_else(|e| e.into_inner());

    let temp = tempfile::tempdir().expect("tempdir");
    let tachi_home = temp.path().join("home");
    let global = tachi_home.join("global/memory.db");
    let project_name = "Sigil-http-direct-e2e";
    let project = tachi_home
        .join("projects")
        .join(project_name)
        .join("memory.db");
    std::fs::create_dir_all(global.parent().expect("global parent")).expect("global parent");
    let _tachi_home = EnvRestore::set_path("TACHI_HOME", &tachi_home);
    let _sigil_home = EnvRestore::remove("SIGIL_HOME");
    let _app_home = EnvRestore::remove("TACHI_APP_HOME");
    let _embedding_disabled = EnvRestore::set("TACHI_SEARCH_DISABLE_QUERY_EMBEDDING", "1");
    seed_project_db(&tachi_home, &project);

    let rt = test_runtime();
    let (ct, daemon_task) = rt.block_on(async {
        let server = crate::MemoryServer::new(global.clone(), None).expect("daemon server");
        let (daemon, ct, daemon_task) = spawn_test_http_daemon(server, &global).await;
        let headers = http_headers(&[
            (crate::session_identity::HEADER_PROFILE, "standard"),
            (crate::session_identity::HEADER_CLIENT, "codex-http-test"),
            (crate::session_identity::HEADER_PROJECT, project_name),
        ]);
        let (client, session_headers, init) = http_mcp_initialize(&daemon.url, headers, None).await;
        assert!(init.get("error").is_none(), "initialize failed: {init:#}");
        http_mcp_initialized(&client, &daemon.url, session_headers.clone()).await;

        let listed = http_mcp_list_tools(&client, &daemon.url, session_headers.clone(), 2).await;
        let names = listed["result"]["tools"]
            .as_array()
            .expect("tools/list tools")
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
            ])
        );

        for (id, hidden) in [(3, "runtime_info"), (4, "tachi_briefing")] {
            let denied = http_mcp_call_tool(
                &client,
                &daemon.url,
                session_headers.clone(),
                id,
                hidden,
                serde_json::Map::new(),
            )
            .await;
            assert!(
                http_tool_text(&denied).contains("tool not found"),
                "standard HTTP direct call should hide {hidden}: {denied:#}"
            );
        }

        for (id, action) in [
            (20, "attach_session"),
            (21, "aggregate_live"),
            (22, "future_action"),
        ] {
            let denied = http_mcp_call_tool(
                &client,
                &daemon.url,
                session_headers.clone(),
                id,
                "tachi_agent_eval",
                serde_json::Map::from_iter([("action".to_string(), serde_json::json!(action))]),
            )
            .await;
            assert!(
                http_tool_text(&denied).contains("not allowed"),
                "standard HTTP eval action {action} must be denied: {denied:#}"
            );
        }

        for (request_id, id, scope, summary) in [
            (
                5,
                "http-direct-global-e2e",
                "global",
                "global DIRECTHTTPMERGE row",
            ),
            (
                6,
                "http-direct-project-e2e",
                "project",
                "project DIRECTHTTPMERGE row",
            ),
            (
                7,
                "http-direct-global-fallback-e2e",
                "global",
                "global DIRECTHTTPFALLBACK row",
            ),
        ] {
            let saved = http_mcp_call_tool(
                &client,
                &daemon.url,
                session_headers.clone(),
                request_id,
                "tachi_memory",
                serde_json::Map::from_iter([
                    ("action".to_string(), serde_json::json!("save")),
                    ("id".to_string(), serde_json::json!(id)),
                    (
                        "text".to_string(),
                        serde_json::json!(format!("{summary} lossless text")),
                    ),
                    ("summary".to_string(), serde_json::json!(summary)),
                    (
                        "path".to_string(),
                        serde_json::json!("/notes/http-direct-e2e"),
                    ),
                    ("category".to_string(), serde_json::json!("fact")),
                    ("scope".to_string(), serde_json::json!(scope)),
                    ("force".to_string(), serde_json::json!(true)),
                ]),
            )
            .await;
            assert!(
                saved["result"]["isError"] != serde_json::json!(true),
                "save {id} failed: {saved:#}"
            );
        }

        for (request_id, query, expected_id, expected_db, expected_store) in [
            (
                8,
                "DIRECTHTTPMERGE",
                "http-direct-project-e2e",
                "project",
                serde_json::json!({"kind": "named_project", "project": project_name}),
            ),
            (
                9,
                "DIRECTHTTPFALLBACK",
                "http-direct-global-fallback-e2e",
                "global",
                serde_json::json!({"kind": "legacy_global"}),
            ),
        ] {
            let search = http_mcp_call_tool(
                &client,
                &daemon.url,
                session_headers.clone(),
                request_id,
                "tachi_memory",
                serde_json::Map::from_iter([
                    ("action".to_string(), serde_json::json!("search")),
                    ("query".to_string(), serde_json::json!(query)),
                    ("scope".to_string(), serde_json::json!("memory")),
                    ("top_k".to_string(), serde_json::json!(10)),
                    ("format".to_string(), serde_json::json!("json")),
                ]),
            )
            .await;
            assert!(search.get("error").is_none(), "search failed: {search:#}");
            assert!(
                search["result"]["isError"] != serde_json::json!(true),
                "search failed: {search:#}"
            );
            let text = http_tool_text(&search);
            let parsed = serde_json::from_str(&text).expect("HTTP search JSON");
            assert_memory_search_source(&parsed, expected_id, expected_db, expected_store);
        }

        (ct, daemon_task)
    });

    assert_eq!(
        memory_text_count(&global, "global DIRECTHTTPMERGE row lossless text"),
        1
    );
    assert_eq!(
        memory_text_count(&project, "project DIRECTHTTPMERGE row lossless text"),
        1
    );
    assert_eq!(
        memory_text_count(&global, "project DIRECTHTTPMERGE row lossless text"),
        0
    );
    for id in ["http-direct-global-e2e", "http-direct-global-fallback-e2e"] {
        assert_eq!(
            memory_access_count(&global, id),
            0,
            "HTTP workspace recall must keep the global fallback library read-only"
        );
    }

    ct.cancel();
    rt.block_on(daemon_task).expect("daemon task");
}
