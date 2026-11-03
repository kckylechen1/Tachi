use super::*;

#[test]
fn stdio_proxy_profile_is_forwarded_once_and_denies_attachment_before_handler() {
    let _guard = crate::utils::global_test_lock()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let temp = tempfile::tempdir().expect("tempdir");
    let tachi_home = temp.path().join("home");
    let global = tachi_home.join("global/memory.db");
    std::fs::create_dir_all(global.parent().expect("global parent")).expect("global parent");

    let rt = test_runtime();
    let (ct, daemon_task) = rt.block_on(async {
        let server = crate::MemoryServer::new(global.clone(), None).expect("daemon server");
        server.set_tool_profile(Some(tachi_hub::ToolProfile::coordinate()));
        let (daemon, ct, daemon_task) = spawn_test_http_daemon(server, &global).await;
        let delegate_proxy = StdioProxyServer {
            adapter_started_at: chrono::Utc::now(),
            tool_profile: Some(tachi_hub::ToolProfile::delegate()),
            resolved_agent_identity: Default::default(),
            rate_limit_session: ProxyRateLimitSession::mint(),
            daemon: std::sync::Arc::new(std::sync::RwLock::new(daemon.clone())),
            app_home: tachi_home.clone(),
            global_db_path: global.clone(),
            project_db_path: None,
            client_project: None,
        };
        let listed = list_tools_via_stdio_proxy(delegate_proxy.clone()).await;
        assert!(
            listed.is_ok(),
            "profile-bound tools/list should succeed: {listed:?}"
        );
        assert!(
            !listed
                .as_ref()
                .unwrap()
                .tools
                .iter()
                .any(|tool| tool.name.as_ref() == "tachi_agent_eval"),
            "delegate profile must remain bound during tools/list"
        );

        let delegate_get = call_tool_via_stdio_proxy(
            delegate_proxy.clone(),
            "tachi_agent_eval",
            serde_json::Map::from_iter([(
                "action".to_string(),
                serde_json::json!("get_attachment"),
            )]),
        )
        .await
        .expect("delegate profile refusal should be an MCP error result");
        assert!(delegate_get.is_error.unwrap_or(false));
        assert!(
            first_text(&delegate_get).contains("tool not found"),
            "delegate must not reach the attachment handler: {delegate_get:?}"
        );

        // Changing launch configuration after construction is not a profile
        // redeclaration. The connection keeps the parsed delegate profile;
        // it must not widen when a later call observes a broader environment.
        let _profile_env = EnvRestore::set("TACHI_PROFILE", "admin");
        let listed_after_redeclare = list_tools_via_stdio_proxy(delegate_proxy.clone()).await;
        assert!(listed_after_redeclare.is_ok());
        assert!(
            !listed_after_redeclare
                .as_ref()
                .unwrap()
                .tools
                .iter()
                .any(|tool| tool.name.as_ref() == "tachi_agent_eval"),
            "mid-session profile widening must be ignored"
        );

        for action in ["attach_session", "get_attachment"] {
            let proxy = StdioProxyServer {
                adapter_started_at: chrono::Utc::now(),
                tool_profile: Some(tachi_hub::ToolProfile::observe()),
                resolved_agent_identity: Default::default(),
                rate_limit_session: ProxyRateLimitSession::mint(),
                daemon: std::sync::Arc::new(std::sync::RwLock::new(daemon.clone())),
                app_home: tachi_home.clone(),
                global_db_path: global.clone(),
                project_db_path: None,
                client_project: None,
            };
            let result = call_tool_via_stdio_proxy(
                proxy,
                "tachi_agent_eval",
                serde_json::Map::from_iter([("action".to_string(), serde_json::json!(action))]),
            )
            .await
            .expect("profile-denied call should return an MCP error result");
            assert!(result.is_error.unwrap_or(false));
            assert!(
                first_text(&result).contains("tool not found"),
                "hidden route must be refused before proxy dispatch: {result:?}"
            );
        }

        let expected_core = vec![
            "tachi_a2a".to_string(),
            "tachi_memory".to_string(),
            "tachi_staff".to_string(),
            "tachi_task".to_string(),
            "vault_connections".to_string(),
        ];
        let expected_standard = vec![
            "tachi_a2a".to_string(),
            "tachi_agent_eval".to_string(),
            "tachi_memory".to_string(),
            "tachi_staff".to_string(),
            "tachi_task".to_string(),
            "vault_connections".to_string(),
        ];
        for raw_profile in [
            "standard",
            "lead",
            "delegate",
            "worker",
            "observe",
            "read",
            "reader",
            "remember",
            "write",
            "writer",
            "agent",
            "coordinate",
            "observe+remember",
            "remember+observe",
            "reader+writer",
            "coordinate+observe",
            "observe+coordinate",
            "delegate+operate",
            "standard+delegate",
            "delegate+standard",
        ] {
            let profile = tachi_hub::parse_tool_profile(raw_profile)
                .unwrap_or_else(|| panic!("profile {raw_profile} should parse"));
            let proxy = StdioProxyServer {
                adapter_started_at: chrono::Utc::now(),
                tool_profile: Some(profile),
                resolved_agent_identity: Default::default(),
                rate_limit_session: ProxyRateLimitSession::mint(),
                daemon: std::sync::Arc::new(std::sync::RwLock::new(daemon.clone())),
                app_home: tachi_home.clone(),
                global_db_path: global.clone(),
                project_db_path: None,
                client_project: None,
            };
            let mut names = list_tools_via_stdio_proxy(proxy.clone())
                .await
                .unwrap_or_else(|error| panic!("{raw_profile} tools/list failed: {error}"))
                .tools
                .into_iter()
                .map(|tool| tool.name.into_owned())
                .collect::<Vec<_>>();
            names.sort();
            let is_standard = profile.as_str() == "standard";
            assert_eq!(
                &names,
                if is_standard {
                    &expected_standard
                } else {
                    &expected_core
                },
                "stdio profile {raw_profile}"
            );

            if is_standard {
                for action in ["attach_session", "aggregate_live", "future_action"] {
                    let denied = call_tool_via_stdio_proxy(
                        proxy.clone(),
                        "tachi_agent_eval",
                        serde_json::Map::from_iter([(
                            "action".to_string(),
                            serde_json::json!(action),
                        )]),
                    )
                    .await
                    .expect("standard eval action refusal should be an MCP result");
                    assert_eq!(denied.is_error, Some(true), "{raw_profile}:{action}");
                    assert!(
                        first_text(&denied).contains("not allowed"),
                        "{raw_profile}:{action}: {denied:?}"
                    );
                }
            }

            for hidden in ["runtime_info", "tachi_briefing"] {
                let result =
                    call_tool_via_stdio_proxy(proxy.clone(), hidden, serde_json::Map::new())
                        .await
                        .unwrap_or_else(|error| {
                            panic!("{raw_profile} hidden call {hidden} failed: {error}")
                        });
                assert_eq!(result.is_error, Some(true), "{raw_profile}:{hidden}");
                assert!(
                    first_text(&result).contains("tool not found"),
                    "{raw_profile}:{hidden}: {result:?}"
                );
            }
        }

        let default_proxy = StdioProxyServer {
            adapter_started_at: chrono::Utc::now(),
            tool_profile: None,
            resolved_agent_identity: Default::default(),
            rate_limit_session: ProxyRateLimitSession::mint(),
            daemon: std::sync::Arc::new(std::sync::RwLock::new(daemon)),
            app_home: tachi_home,
            global_db_path: global,
            project_db_path: None,
            client_project: None,
        };
        let denied_runtime = call_tool_via_stdio_proxy(
            default_proxy.clone(),
            "runtime_info",
            serde_json::Map::new(),
        )
        .await
        .expect("default proxy diagnostic refusal should be an MCP result");
        assert_eq!(denied_runtime.is_error, Some(true));
        assert!(
            first_text(&denied_runtime).contains("tool not found"),
            "missing proxy profile must not call diagnostics: {denied_runtime:?}"
        );

        let mut default_names = list_tools_via_stdio_proxy(default_proxy)
            .await
            .expect("default proxy tools/list")
            .tools
            .into_iter()
            .map(|tool| tool.name.into_owned())
            .collect::<Vec<_>>();
        default_names.sort();
        assert_eq!(
            default_names, expected_standard,
            "missing proxy profile must not inherit the broader daemon profile"
        );
        (ct, daemon_task)
    });
    rt.block_on(async {
        ct.cancel();
        daemon_task.await.expect("daemon task");
    });
}

#[test]
fn stdio_process_selected_ops_proxy_lists_and_calls_through_production_session_clone() {
    let _guard = crate::utils::global_test_lock()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let temp = tempfile::tempdir().expect("tempdir");
    let tachi_home = temp.path().join("home");
    let global = tachi_home.join("global/memory.db");
    std::fs::create_dir_all(global.parent().expect("global parent")).expect("global parent");
    let _tachi_home = EnvRestore::set_path("TACHI_HOME", &tachi_home);
    let _sigil_home = EnvRestore::remove("SIGIL_HOME");
    let _app_home = EnvRestore::remove("TACHI_APP_HOME");

    let rt = test_runtime();
    let (ct, daemon_task) = rt.block_on(async {
        let server = crate::MemoryServer::new(global.clone(), None).expect("daemon server");
        let (daemon, ct, daemon_task) = spawn_test_http_daemon(server, &global).await;
        let proxy = StdioProxyServer {
            adapter_started_at: chrono::Utc::now(),
            tool_profile: Some(tachi_hub::ToolProfile::operate()),
            resolved_agent_identity: Default::default(),
            rate_limit_session: ProxyRateLimitSession::mint(),
            daemon: std::sync::Arc::new(std::sync::RwLock::new(daemon)),
            app_home: tachi_home,
            global_db_path: global,
            project_db_path: None,
            client_project: None,
        };

        let listed = list_tools_via_stdio_proxy(proxy.clone())
            .await
            .expect("process-selected Ops tools/list must traverse HTTP proxy");
        let names = listed
            .tools
            .iter()
            .map(|tool| tool.name.as_ref())
            .collect::<std::collections::BTreeSet<_>>();
        assert_eq!(
            names.len(),
            37,
            "Ops must retain its explicit diagnostic and discovery surface"
        );
        assert!(names.contains("tachi_status"), "Ops list: {names:?}");
        assert!(names.contains("runtime_info"), "Ops list: {names:?}");

        let status = call_tool_via_stdio_proxy(proxy, "tachi_status", serde_json::Map::new())
            .await
            .expect("process-selected Ops call must traverse HTTP proxy");
        assert_ne!(status.is_error, Some(true), "Ops status call: {status:?}");
        (ct, daemon_task)
    });
    ct.cancel();
    rt.block_on(daemon_task).expect("daemon task");
}
