use super::*;

// ─── cli_client: daemon detection + in-process fallback ─────────────────────

#[tokio::test]
async fn cli_client_detect_daemon_returns_none_when_pid_file_missing() {
    let temp = std::env::temp_dir().join(format!("tachi-cli-test-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&temp).unwrap();

    let info = crate::cli_client::detect_daemon(&temp).await;
    assert!(
        info.is_none(),
        "expected None when ~/.tachi/daemon.pid is missing"
    );

    let _ = std::fs::remove_dir_all(&temp);
}

#[tokio::test]
async fn cli_client_detect_daemon_returns_none_for_stale_pid_file() {
    let temp = std::env::temp_dir().join(format!("tachi-cli-test-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&temp).unwrap();

    // Write a pid file pointing at a port nobody is listening on. Pick a high
    // port that is extremely unlikely to be in use during the test.
    let pid_path = temp.join("daemon.pid");
    std::fs::write(
        &pid_path,
        serde_json::to_string(&json!({
            "pid": 99999,
            "port": 1u16,           // privileged port we won't be bound to
            "url": "http://127.0.0.1:1/mcp",
            "global_db": "/tmp/none.db",
            "project_db": null,
        }))
        .unwrap(),
    )
    .unwrap();

    let info = crate::cli_client::detect_daemon(&temp).await;
    assert!(
        info.is_none(),
        "expected None when port in pid file is not listening"
    );

    let _ = std::fs::remove_dir_all(&temp);
}

#[tokio::test]
async fn cli_client_detect_daemon_succeeds_when_port_is_open() {
    let temp = std::env::temp_dir().join(format!("tachi-cli-test-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&temp).unwrap();

    // Bind a real listener on an OS-assigned port so the TCP probe succeeds.
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind ephemeral port");
    let addr = listener.local_addr().expect("local_addr");
    let port = addr.port();

    let pid_path = temp.join("daemon.pid");
    std::fs::write(
        &pid_path,
        serde_json::to_string(&json!({
            "pid": std::process::id(),
            "port": port,
            "url": format!("http://127.0.0.1:{port}/mcp"),
            "global_db": "/tmp/none.db",
            "project_db": null,
        }))
        .unwrap(),
    )
    .unwrap();

    let info = crate::cli_client::detect_daemon(&temp)
        .await
        .expect("expected Some(DaemonInfo) when port is listening");
    assert_eq!(info.port, port);
    assert!(info.url.contains(&format!("127.0.0.1:{port}")));

    drop(listener);
    let _ = std::fs::remove_dir_all(&temp);
}

#[tokio::test]
async fn cli_client_in_process_remember_round_trips_through_handler() {
    // Verifies the in-process fallback path: build a transient MemoryServer
    // and call the same `handle_remember` the MCP tool uses. This is the
    // critical guarantee that `tachi remember` from the shell behaves
    // identically to the `remember` MCP tool when no daemon is running.
    ensure_test_env();

    let db_path = std::env::temp_dir().join(format!(
        "tachi-cli-remember-{}.sqlite",
        uuid::Uuid::new_v4()
    ));
    let server = crate::cli_client::build_in_process_server(&db_path, None)
        .expect("build in-process server");

    let body = crate::memory_search_ops::handle_remember(
        &server,
        crate::tool_params::RememberParams {
            text: "cli round-trip note about a single concrete fact".to_string(),
            summary: String::new(),
            tags: vec!["cli-test".to_string()],
            topic: String::new(),
            importance: Some(0.6),
            scope: Some("project".to_string()),
            project: None,
            path: Some("/notes/cli-roundtrip".to_string()),
            category: None,
            domain: None,
            retention_policy: None,
            valid_from: None,
            valid_until: None,
            force: true, // bypass noise filter for the deterministic test string
        },
    )
    .await
    .expect("remember should succeed in-process");

    let parsed: Value = serde_json::from_str(&body).expect("remember body is JSON");
    // handle_remember delegates to handle_save_memory which returns either
    // {"saved": true, ...} or {"id": "...", ...} depending on the path; we
    // only need to assert the call landed without an error key.
    assert!(
        parsed.get("error").is_none(),
        "remember returned error: {body}"
    );

    let _ = std::fs::remove_file(&db_path);
}

#[tokio::test]
async fn tachi_memory_checkpoint_saves_agent_checkpoint() {
    let server = make_server();

    let body = crate::facade_memory_ops::handle_tachi_memory(
        &server,
        TachiMemoryParams {
            action: "checkpoint".to_string(),
            query: None,
            scope: Some("project".to_string()),
            top_k: 6,
            path_prefix: None,
            file_context: None,
            error_context: None,
            category: None,
            include_archived: false,
            enable_rerank: false,
            as_of: None,
            synthesize: false,
            model: None,
            text: Some("Implemented status diagnostics; next run cargo test.".to_string()),
            title: Some("Status diagnostics".to_string()),
            summary: Some("Status diagnostics checkpoint".to_string()),
            topic: None,
            keywords: Vec::new(),
            entities: Vec::new(),
            importance: None,
            retention_policy: None,
            kind: None,
            path: None,
            id: None,
            force: false,
            source: None,
            valid_from: None,
            valid_until: None,
            flow_id: None,
            event: None,
            state: None,
            project: None,
            domain: Some("engineering".to_string()),
        },
    )
    .await
    .expect("checkpoint should save");

    let parsed: Value = serde_json::from_str(&body).expect("checkpoint JSON");
    assert!(parsed["id"].as_str().is_some());
    assert_eq!(parsed["status"], json!("saved (enrichment pending)"));
}

#[tokio::test]
async fn tachi_memory_save_persists_programming_agent_fields() {
    let server = make_server();
    let mem_id = "mcp-agent-fields-001";

    let body = crate::facade_memory_ops::handle_tachi_memory(
        &server,
        TachiMemoryParams {
            action: "save".to_string(),
            query: None,
            scope: Some("project".to_string()),
            top_k: 6,
            path_prefix: None,
            file_context: None,
            error_context: None,
            category: Some("fact".to_string()),
            include_archived: false,
            enable_rerank: false,
            as_of: None,
            synthesize: false,
            model: None,
            text: Some("Refactored MCP save path for coding agents.".to_string()),
            title: None,
            summary: Some("MCP agent fields".to_string()),
            topic: Some("mcp".to_string()),
            keywords: vec!["rust".to_string(), "mcp".to_string()],
            entities: vec!["memory-server".to_string(), "sigil".to_string()],
            importance: Some(0.75),
            retention_policy: None,
            kind: Some("memory".to_string()),
            path: Some("/project/sigil/mcp".to_string()),
            id: Some(mem_id.to_string()),
            force: true,
            source: None,
            valid_from: None,
            valid_until: None,
            flow_id: None,
            event: None,
            state: None,
            project: None,
            domain: Some("rust".to_string()),
        },
    )
    .await
    .expect("save should succeed");

    let parsed: Value = serde_json::from_str(&body).expect("save JSON");
    assert!(parsed.get("error").is_none(), "save error: {body}");

    let db_path = server.global_db_path_buf();
    let conn = rusqlite::Connection::open(db_path).expect("open test db");
    let (keywords, entities, persons, domain, path): (
        String,
        String,
        String,
        Option<String>,
        String,
    ) = conn
        .query_row(
            "SELECT keywords, entities, persons, domain, path FROM memories WHERE id=?1",
            [mem_id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
        )
        .expect("row exists");
    assert_eq!(keywords, r#"["rust","mcp"]"#);
    assert_eq!(entities, r#"["memory-server","sigil"]"#);
    assert_eq!(persons, "[]");
    assert_eq!(domain.as_deref(), Some("rust"));
    assert_eq!(path, "/project/sigil/mcp");
}

#[tokio::test]
async fn tachi_memory_ask_returns_evidence_contract() {
    let server = make_server();

    let body = crate::facade_memory_ops::handle_tachi_memory(
        &server,
        TachiMemoryParams {
            action: "ask".to_string(),
            query: Some("what did we implement".to_string()),
            scope: None,
            top_k: 3,
            path_prefix: None,
            file_context: None,
            error_context: None,
            category: None,
            include_archived: false,
            enable_rerank: false,
            as_of: None,
            synthesize: false,
            model: None,
            text: None,
            title: None,
            summary: None,
            topic: None,
            keywords: Vec::new(),
            entities: Vec::new(),
            importance: None,
            retention_policy: None,
            kind: None,
            path: None,
            id: None,
            force: false,
            source: None,
            valid_from: None,
            valid_until: None,
            flow_id: None,
            event: None,
            state: None,
            project: None,
            domain: None,
        },
    )
    .await
    .expect("ask should succeed");

    let parsed: Value = serde_json::from_str(&body).expect("ask JSON");
    assert_eq!(parsed["mode"], json!("ask"));
    assert!(parsed["evidence"].is_array());
    assert_eq!(parsed["thinking"]["mode"], json!("ask"));
    assert!(parsed["thinking"]["evidence_count"].as_u64().is_some());
    assert!(parsed["thinking"]["key_evidence"].is_array());
}

#[tokio::test]
async fn tachi_memory_progress_writes_append_only_jsonl() {
    let _lock = crate::shell_ops::tachi_run_root_env_lock()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let (server, temp_home) = make_server_with_temp_home();
    let run_root = temp_home.temp_home.join("runs");
    let original_run_root = std::env::var_os("TACHI_RUN_ROOT");
    std::env::set_var("TACHI_RUN_ROOT", &run_root);

    let body = crate::facade_memory_ops::handle_tachi_memory(
        &server,
        TachiMemoryParams {
            action: "progress".to_string(),
            query: None,
            scope: None,
            top_k: 6,
            path_prefix: None,
            file_context: None,
            error_context: None,
            category: None,
            include_archived: false,
            enable_rerank: false,
            as_of: None,
            synthesize: false,
            model: None,
            text: Some("step completed with api_key=test-secret-value-1234567890".to_string()),
            title: Some("Progress step".to_string()),
            summary: Some("one step done".to_string()),
            topic: None,
            keywords: Vec::new(),
            entities: Vec::new(),
            importance: None,
            retention_policy: None,
            kind: None,
            path: None,
            id: None,
            force: false,
            source: None,
            valid_from: None,
            valid_until: None,
            flow_id: Some("flow_progress_test".to_string()),
            event: Some("validation".to_string()),
            state: Some("running".to_string()),
            project: None,
            domain: None,
        },
    )
    .await
    .expect("progress should record");

    let parsed: Value = serde_json::from_str(&body).expect("progress JSON");
    assert_eq!(parsed["status"], json!("recorded"));
    assert_eq!(parsed["secret_redactions"], json!(1));
    let log = std::fs::read_to_string(run_root.join("flow_progress_test/progress.jsonl"))
        .expect("progress jsonl");
    assert!(log.contains("validation"));
    assert!(!log.contains("test-secret-value-1234567890"));
    if let Some(original) = original_run_root {
        std::env::set_var("TACHI_RUN_ROOT", original);
    } else {
        std::env::remove_var("TACHI_RUN_ROOT");
    }
}

#[tokio::test]
async fn tachi_memory_briefing_includes_health_wiki_and_kanban_sections() {
    let server = make_server();

    let body = crate::facade_memory_ops::handle_tachi_memory(
        &server,
        TachiMemoryParams {
            action: "briefing".to_string(),
            query: Some("current work".to_string()),
            scope: None,
            top_k: 3,
            path_prefix: None,
            file_context: None,
            error_context: None,
            category: None,
            include_archived: false,
            enable_rerank: false,
            as_of: None,
            synthesize: false,
            model: None,
            text: None,
            title: None,
            summary: None,
            topic: None,
            keywords: Vec::new(),
            entities: Vec::new(),
            importance: None,
            retention_policy: None,
            kind: None,
            path: None,
            id: None,
            force: false,
            source: None,
            valid_from: None,
            valid_until: None,
            flow_id: None,
            event: None,
            state: None,
            project: None,
            domain: None,
        },
    )
    .await
    .expect("briefing should succeed");

    assert!(body.starts_with("## Tachi briefing"));
    assert!(body.contains("### Database context"));
    assert!(body.contains("### Memories"));
    assert!(body.contains("tachi_status"));
    assert!(!body.contains("merge_hints"));
    assert!(!body.contains("skill_quality"));
}

#[tokio::test]
async fn tachi_status_reports_failed_jobs_and_vector_backfill_hint() {
    let (server, temp_home) = make_server_with_temp_home();
    let manifest_path = temp_home.temp_home.join(".tachi/manifest.json");
    let marker_path = temp_home
        .temp_home
        .join(".tachi/foundry-runs/.last_distill_run");
    std::fs::create_dir_all(marker_path.parent().unwrap()).expect("marker parent");
    std::fs::write(&marker_path, Utc::now().to_rfc3339()).expect("write marker");

    server
        .with_global_store(|store| {
            store
                .connection()
                .execute(
                    "INSERT INTO memories
                     (id, path, summary, text, importance, timestamp, category, topic, keywords, persons, entities, location, source, scope, archived, created_at, updated_at, access_count, revision, metadata)
                     VALUES (?1, '/facts/status', 'status', 'status diagnostic memory', 0.8, ?2, 'fact', 'status', '[]', '[]', '[]', '', 'manual', 'project', 0, ?2, ?2, 0, 1, '{}')",
                    rusqlite::params!["status-memory", Utc::now().to_rfc3339()],
                )
                .map_err(|e| e.to_string())?;
            store
                .connection()
                .execute(
                    "INSERT INTO foundry_jobs
                     (id, kind, lane, status, target_db, path_prefix, memory_ids, metadata, created_at, updated_at)
                     VALUES ('job-1', 'memory_distill', 'distill', 'failed', 'project', '/', '[]', ?1, ?2, ?2)",
                    rusqlite::params![
                        json!({"terminal_reason":{"reason":"403 Forbidden"}}).to_string(),
                        Utc::now().to_rfc3339()
                    ],
                )
                .map_err(|e| e.to_string())?;
            Ok(())
        })
        .expect("seed status db");

    let manifest = crate::manifest::Manifest {
        schema_version: 1,
        generated_at: Utc::now().to_rfc3339(),
        comment: String::new(),
        dbs: vec![crate::manifest::DbEntry {
            path: server.global_db_path_buf().display().to_string(),
            role: crate::manifest::DbRole::Global,
            owner: "tachi".to_string(),
            schema_kind: "tachi".to_string(),
            vec_enabled: true,
            allow_write: true,
            last_doctor_at: Utc::now().to_rfc3339(),
            last_classification: "healthy".to_string(),
            scope_hint: "global".to_string(),
            notes: String::new(),
        }],
    };
    manifest.save(&manifest_path).expect("save manifest");

    let body = crate::status_ops::handle_tachi_status_full(&server)
        .await
        .expect("status should serialize");
    let parsed: Value = serde_json::from_str(&body).expect("status JSON");
    assert_eq!(parsed["databases"]["failed_jobs"], json!(1));
    assert_eq!(parsed["distill"]["is_stale"], json!(false));
    assert!(
        parsed["databases"]["low_vector_coverage"][0]["backfill_command"]
            .as_str()
            .unwrap_or_default()
            .contains("tachi backfill-vectors")
    );
    assert!(parsed["databases"]["provider_auth_failures"]
        .as_array()
        .is_some_and(|rows| !rows.is_empty()));
    assert_eq!(
        parsed["databases"]["provider_auth_failures"][0]["inferred_invalid_provider"],
        json!("SILICONFLOW")
    );
    assert_eq!(parsed["models"]["embedding"]["model"], json!("voyage-4"));
}

#[tokio::test]
async fn tachi_status_marks_config_env_only_key_as_configured() {
    let (server, temp_home) = make_server_with_temp_home();
    let config_env = temp_home.temp_home.join(".tachi/config.env");
    std::fs::create_dir_all(config_env.parent().expect("config env parent"))
        .expect("create config env dir");
    std::fs::write(&config_env, "VOYAGE_API_KEY=config-only-value\n").expect("write config env");

    let original_voyage = std::env::var_os("VOYAGE_API_KEY");
    std::env::remove_var("VOYAGE_API_KEY");

    let body = crate::status_ops::handle_tachi_status_full(&server)
        .await
        .expect("status should serialize");
    let parsed: Value = serde_json::from_str(&body).expect("status JSON");
    let voyage = parsed["api_keys"]
        .as_array()
        .expect("api_keys array")
        .iter()
        .find(|row| row["name"] == json!("VOYAGE_API_KEY"))
        .expect("voyage key row");
    assert_eq!(voyage["status"], json!("configured"));
    assert_eq!(voyage["source"], json!("config.env"));

    if let Some(value) = original_voyage {
        std::env::set_var("VOYAGE_API_KEY", value);
    } else {
        std::env::remove_var("VOYAGE_API_KEY");
    }
}

#[tokio::test]
async fn tachi_status_marks_config_env_alias_key_as_configured() {
    let (server, temp_home) = make_server_with_temp_home();
    let config_env = temp_home.temp_home.join(".tachi/config.env");
    std::fs::create_dir_all(config_env.parent().expect("config env parent"))
        .expect("create config env dir");
    std::fs::write(&config_env, "BIGMODEL_API_KEY=alias-config-value\n").expect("write config env");

    let original_reasoning = std::env::var_os("REASONING_API_KEY");
    let original_zai = std::env::var_os("ZAI_API_KEY");
    let original_bigmodel = std::env::var_os("BIGMODEL_API_KEY");
    std::env::remove_var("REASONING_API_KEY");
    std::env::remove_var("ZAI_API_KEY");
    std::env::remove_var("BIGMODEL_API_KEY");

    let body = crate::status_ops::handle_tachi_status_full(&server)
        .await
        .expect("status should serialize");
    let parsed: Value = serde_json::from_str(&body).expect("status JSON");
    let reasoning = parsed["api_keys"]
        .as_array()
        .expect("api_keys array")
        .iter()
        .find(|row| row["name"] == json!("REASONING_API_KEY"))
        .expect("reasoning key row");
    assert_eq!(reasoning["status"], json!("configured"));
    assert_eq!(reasoning["source"], json!("alias"));

    if let Some(value) = original_reasoning {
        std::env::set_var("REASONING_API_KEY", value);
    } else {
        std::env::remove_var("REASONING_API_KEY");
    }
    if let Some(value) = original_zai {
        std::env::set_var("ZAI_API_KEY", value);
    } else {
        std::env::remove_var("ZAI_API_KEY");
    }
    if let Some(value) = original_bigmodel {
        std::env::set_var("BIGMODEL_API_KEY", value);
    } else {
        std::env::remove_var("BIGMODEL_API_KEY");
    }
}
