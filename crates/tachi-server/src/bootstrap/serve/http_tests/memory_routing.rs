use super::*;

#[test]
fn http_client_call_writes_bound_project_via_global_only_daemon() {
    let _guard = crate::utils::global_test_lock()
        .lock()
        .unwrap_or_else(|e| e.into_inner());

    let temp = tempfile::tempdir().expect("tempdir");
    let tachi_home = temp.path().join("home");
    let global = tachi_home.join("global/memory.db");
    let project_name = "Sigil-proxy-e2e";
    let project = tachi_home
        .join("projects")
        .join(project_name)
        .join("memory.db");
    std::fs::create_dir_all(global.parent().expect("global parent")).expect("global parent");
    let _tachi_home = EnvRestore::set_path("TACHI_HOME", &tachi_home);
    let _sigil_home = EnvRestore::remove("SIGIL_HOME");
    let _app_home = EnvRestore::remove("TACHI_APP_HOME");
    seed_project_db(&tachi_home, &project);

    let rt = test_runtime();
    let (ct, daemon_task) = rt.block_on(async {
        let server = crate::MemoryServer::new(global.clone(), None).expect("daemon server");
        let (daemon, ct, daemon_task) = spawn_test_http_daemon(server, &global).await;
        let proxy = HttpTestClient {
            tool_profile: None,
            daemon: daemon.clone(),
            client_project: Some(project_name.to_string()),
        };

        let saved_text = "stdio proxy e2e writes only the bound project db";
        let result = call_tool_over_http(
            proxy,
            "tachi_memory",
            serde_json::Map::from_iter([
                ("action".to_string(), serde_json::json!("save")),
                ("text".to_string(), serde_json::json!(saved_text)),
                ("summary".to_string(), serde_json::json!("stdio proxy e2e")),
                (
                    "path".to_string(),
                    serde_json::json!("/tests/stdio-proxy-e2e"),
                ),
                ("category".to_string(), serde_json::json!("fact")),
                ("scope".to_string(), serde_json::json!("project")),
                ("force".to_string(), serde_json::json!(true)),
            ]),
        )
        .await
        .expect("proxied save_memory should succeed");

        assert_tool_ok(&result);
        (ct, daemon_task)
    });

    let saved_text = "stdio proxy e2e writes only the bound project db";
    assert_eq!(memory_text_count(&project, saved_text), 1);
    assert_eq!(memory_text_count(&global, saved_text), 0);

    ct.cancel();
    rt.block_on(daemon_task).expect("daemon task");
}

#[test]
fn http_client_allows_explicit_cross_project_read() {
    let _guard = crate::utils::global_test_lock()
        .lock()
        .unwrap_or_else(|e| e.into_inner());

    let temp = tempfile::tempdir().expect("tempdir");
    let tachi_home = temp.path().join("home");
    let global = tachi_home.join("global/memory.db");
    let bound_project_name = "Sigil-proxy-cross-read-e2e";
    let other_project_name = "Quant-proxy-cross-read-e2e";
    let bound_project = tachi_home
        .join("projects")
        .join(bound_project_name)
        .join("memory.db");
    let other_project = tachi_home
        .join("projects")
        .join(other_project_name)
        .join("memory.db");
    std::fs::create_dir_all(global.parent().expect("global parent")).expect("global parent");
    let _tachi_home = EnvRestore::set_path("TACHI_HOME", &tachi_home);
    let _sigil_home = EnvRestore::remove("SIGIL_HOME");
    let _app_home = EnvRestore::remove("TACHI_APP_HOME");
    seed_project_db(&tachi_home, &bound_project);
    seed_project_db(&tachi_home, &other_project);

    let rt = test_runtime();
    let (ct, daemon_task) = rt.block_on(async {
        let server = crate::MemoryServer::new(global.clone(), None).expect("daemon server");
        let (daemon, ct, daemon_task) = spawn_test_http_daemon(server, &global).await;
        let bound_proxy = HttpTestClient {
            tool_profile: None,
            daemon: daemon.clone(),
            client_project: Some(bound_project_name.to_string()),
        };
        let other_proxy = HttpTestClient {
            tool_profile: None,
            daemon: daemon.clone(),
            client_project: Some(other_project_name.to_string()),
        };

        let result = call_tool_over_http(
            other_proxy,
            "tachi_memory",
            serde_json::Map::from_iter([
                ("action".to_string(), serde_json::json!("save")),
                (
                    "id".to_string(),
                    serde_json::json!("other-proxy-cross-read-e2e"),
                ),
                (
                    "text".to_string(),
                    serde_json::json!("other PROXYCROSSREAD row"),
                ),
                (
                    "summary".to_string(),
                    serde_json::json!("other PROXYCROSSREAD row"),
                ),
                (
                    "path".to_string(),
                    serde_json::json!("/notes/stdio-proxy-cross-read-e2e"),
                ),
                ("category".to_string(), serde_json::json!("fact")),
                ("scope".to_string(), serde_json::json!("project")),
                ("force".to_string(), serde_json::json!(true)),
            ]),
        )
        .await
        .expect("seed other project through proxy");
        assert_tool_ok(&result);

        let result = call_tool_over_http(
            bound_proxy,
            "tachi_memory",
            serde_json::Map::from_iter([
                ("action".to_string(), serde_json::json!("search")),
                ("project".to_string(), serde_json::json!(other_project_name)),
                ("query".to_string(), serde_json::json!("PROXYCROSSREAD")),
                ("scope".to_string(), serde_json::json!("memory")),
                ("top_k".to_string(), serde_json::json!(10)),
                ("format".to_string(), serde_json::json!("json")),
            ]),
        )
        .await
        .expect("explicit cross-project read should be forwarded");
        assert_tool_ok(&result);
        let parsed = first_text_json(&result);
        assert_search_section_rows_are_objects(&parsed);
        let text = parsed.to_string();
        assert!(
            text.contains("other-proxy-cross-read-e2e"),
            "proxied explicit project read lost other project row: {parsed:#}"
        );

        (ct, daemon_task)
    });

    ct.cancel();
    rt.block_on(daemon_task).expect("daemon task");
}

#[test]
fn http_client_archives_global_row_with_bound_project() {
    let _guard = crate::utils::global_test_lock()
        .lock()
        .unwrap_or_else(|e| e.into_inner());

    let temp = tempfile::tempdir().expect("tempdir");
    let tachi_home = temp.path().join("home");
    let global = tachi_home.join("global/memory.db");
    let project_name = "Sigil-proxy-archive-e2e";
    let project = tachi_home
        .join("projects")
        .join(project_name)
        .join("memory.db");
    std::fs::create_dir_all(global.parent().expect("global parent")).expect("global parent");
    let _tachi_home = EnvRestore::set_path("TACHI_HOME", &tachi_home);
    let _sigil_home = EnvRestore::remove("SIGIL_HOME");
    let _app_home = EnvRestore::remove("TACHI_APP_HOME");
    seed_project_db(&tachi_home, &project);

    let rt = test_runtime();
    let (ct, daemon_task) = rt.block_on(async {
        let server = crate::MemoryServer::new(global.clone(), None).expect("daemon server");
        server.set_tool_profile(Some(tachi_hub::ToolProfile::admin()));
        let (daemon, ct, daemon_task) = spawn_test_http_daemon(server, &global).await;
        let proxy = HttpTestClient {
            tool_profile: Some(tachi_hub::ToolProfile::admin()),
            daemon: daemon.clone(),
            client_project: Some(project_name.to_string()),
        };

        let result = call_tool_over_http(
            proxy.clone(),
            "tachi_memory",
            serde_json::Map::from_iter([
                ("action".to_string(), serde_json::json!("save")),
                (
                    "id".to_string(),
                    serde_json::json!("global-proxy-archive-e2e"),
                ),
                (
                    "text".to_string(),
                    serde_json::json!("global archive fallback row PROXYMUTATEGLOBAL"),
                ),
                (
                    "summary".to_string(),
                    serde_json::json!("global archive fallback row"),
                ),
                (
                    "path".to_string(),
                    serde_json::json!("/tests/stdio-proxy-mutate-global"),
                ),
                ("category".to_string(), serde_json::json!("fact")),
                ("scope".to_string(), serde_json::json!("global")),
                ("force".to_string(), serde_json::json!(true)),
            ]),
        )
        .await
        .expect("seed archive row");
        assert_tool_ok(&result);

        let archived = call_tool_over_http(
            proxy,
            "archive_memory",
            serde_json::Map::from_iter([(
                "id".to_string(),
                serde_json::json!("global-proxy-archive-e2e"),
            )]),
        )
        .await
        .expect("proxied archive should succeed");
        assert_tool_ok(&archived);
        let archived = first_text_json(&archived);
        assert_eq!(archived["archived"], serde_json::json!(true));
        assert_eq!(archived["db"], serde_json::json!("global"));

        (ct, daemon_task)
    });

    assert_eq!(
        memory_archived_value(&global, "global-proxy-archive-e2e"),
        1
    );
    assert_eq!(memory_id_count(&project, "global-proxy-archive-e2e"), 0);

    ct.cancel();
    rt.block_on(daemon_task).expect("daemon task");
}
