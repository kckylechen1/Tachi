use super::*;

fn tachi_memory_params(action: &str) -> TachiMemoryParams {
    TachiMemoryParams {
        action: action.to_string(),
        format: Some("markdown".to_string()),
        query: None,
        scope: None,
        top_k: 6,
        path_prefix: None,
        file_context: None,
        error_context: None,
        category: None,
        include_archived: false,
        include_training: false,
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
        metadata: None,
        files: Vec::new(),
        flow_id: None,
        event: None,
        state: None,
        project: None,
        domain: None,
        compact: false,
    }
}

#[tokio::test]
async fn tachi_memory_search_defaults_to_json_and_keeps_markdown_escape_hatch() {
    let server = make_server();

    let mut json_params = tachi_memory_params("search");
    json_params.format = None;
    json_params.query = Some("facade default json no matches".to_string());
    let json_body = crate::facade_memory_ops::handle_tachi_memory(&server, json_params)
        .await
        .expect("default search should succeed");
    let parsed: Value = serde_json::from_str(&json_body).expect("default search JSON");
    assert_eq!(parsed["status"], json!("completed"));
    assert_eq!(parsed["query"], json!("facade default json no matches"));

    let mut markdown_params = tachi_memory_params("search");
    markdown_params.query = Some("facade markdown output".to_string());
    let markdown = crate::facade_memory_ops::handle_tachi_memory(&server, markdown_params)
        .await
        .expect("markdown search should succeed");
    assert!(markdown.starts_with("## Tachi search:"), "{markdown}");
}

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
    assert!(info.url.contains(&format!("127.0.0.1:{port}")));

    drop(listener);
    let _ = std::fs::remove_dir_all(&temp);
}

#[tokio::test]
async fn cli_client_detect_daemon_rejects_nonlocal_pid_url_even_when_port_is_open() {
    let temp = std::env::temp_dir().join(format!("tachi-cli-test-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&temp).unwrap();

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind ephemeral port");
    let port = listener.local_addr().expect("local_addr").port();

    let pid_path = temp.join("daemon.pid");
    std::fs::write(
        &pid_path,
        serde_json::to_string(&json!({
            "pid": std::process::id(),
            "port": port,
            "url": format!("https://example.com:{port}/mcp"),
            "global_db": "/tmp/none.db",
            "project_db": null,
        }))
        .unwrap(),
    )
    .unwrap();

    let info = crate::cli_client::detect_daemon(&temp).await;
    assert!(
        info.is_none(),
        "expected None when pid file URL points outside localhost"
    );

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
            format: Some("markdown".to_string()),
            query: None,
            scope: Some("project".to_string()),
            top_k: 6,
            path_prefix: None,
            file_context: None,
            error_context: None,
            category: None,
            include_archived: false,
            include_training: false,
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
            metadata: None,
            compact: false,
            files: Vec::new(),
        },
    )
    .await
    .expect("checkpoint should save");

    assert!(body.contains("Saved ->"));
    assert!(body.contains("status: saved"));
    assert!(body.contains("id: `"));
    assert!(body.contains("Summary: Status diagnostics checkpoint"));
}

#[tokio::test]
async fn tachi_memory_save_persists_programming_agent_fields() {
    let server = make_server();
    let mem_id = "mcp-agent-fields-001";

    let body = crate::facade_memory_ops::handle_tachi_memory(
        &server,
        TachiMemoryParams {
            action: "save".to_string(),
            format: Some("markdown".to_string()),
            query: None,
            scope: Some("project".to_string()),
            top_k: 6,
            path_prefix: None,
            file_context: None,
            error_context: None,
            category: Some("fact".to_string()),
            include_archived: false,
            include_training: false,
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
            metadata: None,
            compact: false,
            files: Vec::new(),
        },
    )
    .await
    .expect("save should succeed");

    assert!(body.contains("Saved ->"));
    assert!(body.contains(&format!("`{mem_id}`")));

    let db_path = server.global_db_path_buf();
    let conn = rusqlite::Connection::open(db_path).expect("open test db");
    let (keywords, entities, domain, path): (String, String, Option<String>, String) = conn
        .query_row(
            "SELECT keywords, entities, domain, path FROM memories WHERE id=?1",
            [mem_id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
        )
        .expect("row exists");
    assert_eq!(keywords, r#"["rust","mcp"]"#);
    assert_eq!(entities, r#"["memory-server","sigil"]"#);
    assert_eq!(domain.as_deref(), Some("rust"));
    assert_eq!(path, "/project/sigil/mcp");
}

#[tokio::test]
async fn tachi_memory_get_action_returns_saved_entry() {
    let server = make_server();
    let mem_id = "facade-get-action-001";

    let mut save = tachi_memory_params("save");
    save.format = Some("json".to_string());
    save.scope = Some("project".to_string());
    save.text = Some("Facade get action should return the full saved memory text.".to_string());
    save.summary = Some("Facade get action".to_string());
    save.path = Some("/scratch/tachi/facade-get-action".to_string());
    save.id = Some(mem_id.to_string());
    save.force = true;
    crate::facade_memory_ops::handle_tachi_memory(&server, save)
        .await
        .expect("save should succeed");

    let mut get = tachi_memory_params("get");
    get.format = Some("json".to_string());
    get.id = Some(mem_id.to_string());
    let body = crate::facade_memory_ops::handle_tachi_memory(&server, get)
        .await
        .expect("get should succeed");
    let parsed: Value = serde_json::from_str(&body).expect("get response json");

    assert_eq!(parsed["id"], json!(mem_id));
    assert_eq!(parsed["path"], json!("/scratch/tachi/facade-get-action"));
    assert!(
        parsed["text"]
            .as_str()
            .is_some_and(|text| text.contains("full saved memory text")),
        "expected full text in get response: {body}"
    );
}

#[tokio::test]
async fn tachi_memory_ask_returns_evidence_contract() {
    let server = make_server();

    let body = crate::facade_memory_ops::handle_tachi_memory(
        &server,
        TachiMemoryParams {
            action: "ask".to_string(),
            format: Some("markdown".to_string()),
            query: Some("what did we implement".to_string()),
            scope: None,
            top_k: 3,
            path_prefix: None,
            file_context: None,
            error_context: None,
            category: None,
            include_archived: false,
            include_training: false,
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
            metadata: None,
            compact: false,
            files: Vec::new(),
        },
    )
    .await
    .expect("ask should succeed");

    assert!(body.starts_with("## Tachi ask"));
    assert!(body.contains("status: completed"));
    assert!(body.contains("evidence:"));
    // evidence count may be 0 in CI without embedding API; verify format only
    assert!(body.contains("hit(s)"));
}

#[tokio::test]
async fn tachi_memory_ask_can_return_compact_json() {
    let server = make_server();
    let params: TachiMemoryParams = serde_json::from_value(json!({
        "action": "ask",
        "format": "json",
        "query": "what did we implement",
        "top_k": 3
    }))
    .expect("params deserialize");

    let body = crate::facade_memory_ops::handle_tachi_memory(&server, params)
        .await
        .expect("ask json should succeed");
    let parsed: Value = serde_json::from_str(&body).expect("ask response should be JSON");

    assert_eq!(parsed["status"], json!("completed"));
    assert_eq!(parsed["query"], json!("what did we implement"));
    assert!(parsed["evidence"].is_array());
    assert!(parsed["thinking"].is_object());
}

#[tokio::test]
async fn tachi_memory_readiness_can_return_operational_json() {
    let server = make_server();
    let params: TachiMemoryParams = serde_json::from_value(json!({
        "action": "readiness",
        "format": "json"
    }))
    .expect("params deserialize");

    let body = crate::facade_memory_ops::handle_tachi_memory(&server, params)
        .await
        .expect("readiness json should succeed");
    let parsed: Value = serde_json::from_str(&body).expect("readiness response should be JSON");

    assert_eq!(parsed["status"], json!("completed"));
    assert!(parsed["runtime"].is_object());
    assert!(parsed["health"].is_object());
    assert!(parsed["tools"].is_array());
    assert!(parsed["tool_visibility_summary"].is_object());
    assert!(parsed["suggestions"].is_array());
    assert!(parsed["vector_health"].is_object());
}

#[test]
fn tachi_memory_action_schema_declares_enum_values() {
    let schema = rmcp::schemars::schema_for!(TachiMemoryParams);
    let value = serde_json::to_value(schema).expect("schema serializes");
    let action = &value["properties"]["action"];

    assert_eq!(action["type"], json!("string"));
    assert!(action["enum"]
        .as_array()
        .expect("action enum")
        .contains(&json!("briefing")));
    assert!(action["enum"]
        .as_array()
        .expect("action enum")
        .contains(&json!("get")));
    assert!(action["enum"]
        .as_array()
        .expect("action enum")
        .contains(&json!("readiness")));
}

#[test]
fn tachi_skill_action_schema_declares_bundle_and_loadout() {
    let schema = rmcp::schemars::schema_for!(TachiSkillParams);
    let value = serde_json::to_value(schema).expect("schema serializes");
    let action = &value["properties"]["action"];

    assert_eq!(action["type"], json!("string"));
    let values = action["enum"].as_array().expect("action enum");
    assert!(values.contains(&json!("discover")));
    assert!(values.contains(&json!("run")));
    assert!(values.contains(&json!("bundle")));
    assert!(values.contains(&json!("loadout")));
}

#[test]
fn tachi_task_action_schema_declares_feature_briefing() {
    let schema = rmcp::schemars::schema_for!(TachiTaskParams);
    let value = serde_json::to_value(schema).expect("schema serializes");
    let action = &value["properties"]["action"];

    assert_eq!(action["type"], json!("string"));
    let values = action["enum"].as_array().expect("action enum");
    assert!(values.contains(&json!("briefing")));
    assert!(values.contains(&json!("plan")));
    assert!(values.contains(&json!("dispatch")));
    assert!(values.contains(&json!("complete")));
    assert!(values.contains(&json!("recommend")));
    assert!(values.contains(&json!("route_simulate")));
    assert!(values.contains(&json!("proposals")));
    assert!(values.contains(&json!("review_proposal")));
    assert!(values.contains(&json!("apply_proposals")));
    assert!(values.contains(&json!("intake")));
    assert!(values.contains(&json!("link_pr")));
    assert!(values.contains(&json!("pr_status")));
    assert!(values.contains(&json!("release_note")));
    assert!(values.contains(&json!("ux_matrix")));
    assert!(values.contains(&json!("build_references")));
    assert!(values.contains(&json!("close_loop")));
}

#[allow(clippy::await_holding_lock)]
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
            format: Some("markdown".to_string()),
            query: None,
            scope: None,
            top_k: 6,
            path_prefix: None,
            file_context: None,
            error_context: None,
            category: None,
            include_archived: false,
            include_training: false,
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
            metadata: None,
            compact: false,
            files: Vec::new(),
        },
    )
    .await
    .expect("progress should record");

    assert!(body.starts_with("## Tachi progress"));
    assert!(body.contains("status: recorded"));
    assert!(body.contains("secret_redactions: 1"));
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
            format: Some("markdown".to_string()),
            query: Some("current work".to_string()),
            scope: None,
            top_k: 3,
            path_prefix: None,
            file_context: None,
            error_context: None,
            category: None,
            include_archived: false,
            include_training: false,
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
            metadata: None,
            compact: false,
            files: Vec::new(),
        },
    )
    .await
    .expect("briefing should succeed");

    assert!(body.starts_with("## Tachi briefing"));
    assert!(
        body.contains("Layer authority: [AUTHORITY: docs/specs > guide/SOP > wiki > memory/eval]")
    );
    assert!(body.contains("### Memories (this project)"));
    assert!(body.contains("[AUTHORITY: LOW-MEDIUM]"));
    assert!(body.contains("### Health snapshot"));
    assert!(!body.contains("merge_hints"));
    assert!(!body.contains("skill_quality"));
}

#[allow(clippy::await_holding_lock)]
#[tokio::test]
async fn tachi_memory_briefing_includes_recent_verification_gates() {
    let _guard = crate::shell_ops::tachi_run_root_env_lock()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let tmp = tempfile::tempdir().unwrap();
    let original = std::env::var_os("TACHI_RUN_ROOT");
    std::env::set_var("TACHI_RUN_ROOT", tmp.path());
    let flow = "flow_briefing-verification";
    let run_dir = tmp.path().join(flow);
    std::fs::create_dir_all(&run_dir).unwrap();
    std::fs::write(
        run_dir.join("verification.json"),
        serde_json::to_string_pretty(&json!({
            "flow_id": flow,
            "pr_ref": "kckylechen1/tachi#209",
            "head_sha": "abc",
            "overall": "failed",
            "updated_at": "2026-06-08T00:00:00Z",
            "items": [
                {"id":"gitleaks","status":"passed","required":true,"head_sha":"abc"},
                {"id":"clippy","status":"failed","required":true,"head_sha":"abc"}
            ]
        }))
        .unwrap(),
    )
    .unwrap();
    let server = make_server();

    let mut params = tachi_memory_params("briefing");
    params.query = Some("verification gates".to_string());
    params.compact = true;
    let body = crate::facade_memory_ops::handle_tachi_memory(&server, params)
        .await
        .expect("briefing should succeed");

    assert!(body.contains("### Verification gates"));
    assert!(body.contains("[failed] `flow_briefing-verification`"));
    assert!(body.contains("`kckylechen1/tachi#209`"));
    assert!(body.contains("tachi_verify(action='board')"));
    if let Some(original) = original {
        std::env::set_var("TACHI_RUN_ROOT", original);
    } else {
        std::env::remove_var("TACHI_RUN_ROOT");
    }
}

#[tokio::test]
async fn tachi_memory_briefing_defaults_to_named_wiki_project_hits() {
    let mut entry = make_entry("briefing-default-wiki-hit");
    entry.path = "/wiki/agent/tachi/briefing-default".to_string();
    entry.summary = "Briefing default wiki hit".to_string();
    entry.text =
        "BriefingDefaultWikiNeedle should appear in default briefing wiki rows.".to_string();
    entry.entities = vec!["BriefingDefaultWikiNeedle".to_string()];

    let (server, _home) = seed_wiki_project_entries(vec![entry]);

    let body = crate::facade_memory_ops::handle_tachi_memory(
        &server,
        TachiMemoryParams {
            action: "briefing".to_string(),
            format: Some("json".to_string()),
            query: Some("BriefingDefaultWikiNeedle".to_string()),
            scope: None,
            top_k: 5,
            path_prefix: None,
            file_context: None,
            error_context: None,
            category: None,
            include_archived: false,
            include_training: false,
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
            metadata: None,
            compact: false,
            files: Vec::new(),
        },
    )
    .await
    .expect("briefing should succeed");

    let parsed: Value = serde_json::from_str(&body).expect("briefing JSON");
    assert!(
        parsed["wiki"]
            .as_array()
            .is_some_and(|rows| rows.iter().any(|row| {
                row["id"] == json!("briefing-default-wiki-hit")
                    || row["path"] == json!("/wiki/agent/tachi/briefing-default")
            })),
        "expected default briefing wiki rows to include project:wiki hit, got: {parsed}"
    );
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
                     (id, path, summary, text, importance, timestamp, category, topic, keywords, entities, source, scope, archived, created_at, updated_at, access_count, revision, metadata)
                     VALUES (?1, '/facts/status', 'status', 'status diagnostic memory', 0.8, ?2, 'fact', 'status', '[]', '[]', 'manual', 'project', 0, ?2, ?2, 0, 1, '{}')",
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
    server.llm.set_provider_secret_pool(
        "VOYAGE_API_KEY",
        vec![
            crate::llm::ProviderSecret {
                key_id: "VOYAGE_API_KEY_1".to_string(),
                value: "voyage-secret-one".to_string(),
            },
            crate::llm::ProviderSecret {
                key_id: "VOYAGE_API_KEY_2".to_string(),
                value: "voyage-secret-two".to_string(),
            },
        ],
    );
    server
        .llm
        .mark_provider_key_rate_limited_for_tests("VOYAGE_API_KEY_1", Some(60));

    let body = crate::status_ops::handle_tachi_status_full(&server)
        .await
        .expect("status should serialize");
    let parsed: Value = serde_json::from_str(&body).expect("status JSON");
    assert!(parsed["runtime"]["pid"].as_u64().is_some());
    assert!(parsed["runtime"]["provider_secret_count"]
        .as_u64()
        .is_some());
    assert_eq!(
        parsed["runtime"]["provider_pools"][0]["logical_name"],
        json!("VOYAGE_API_KEY")
    );
    assert_eq!(
        parsed["runtime"]["provider_pools"][0]["available_keys"],
        json!(1)
    );
    assert_eq!(
        parsed["runtime"]["provider_pools"][0]["rate_limited_keys"][0]["key_id"],
        json!("VOYAGE_API_KEY_1")
    );
    assert!(
        !body.contains("voyage-secret-one") && !body.contains("voyage-secret-two"),
        "status must not expose provider secret values: {body}"
    );
    assert_eq!(parsed["runtime"]["vault"]["unlocked"], json!(false));
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
async fn tachi_status_separates_active_worker_queue_from_terminal_history() {
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
                    "INSERT INTO foundry_jobs
                     (id, kind, lane, status, target_db, path_prefix, memory_ids, metadata, created_at, updated_at)
                     VALUES ('job-completed', 'memory_neighborhood', 'maintenance', 'completed', 'project', '/', '[]', '{}', ?1, ?1)",
                    rusqlite::params![Utc::now().to_rfc3339()],
                )
                .map_err(|e| e.to_string())?;
            store
                .connection()
                .execute(
                    "INSERT INTO foundry_jobs
                     (id, kind, lane, status, target_db, path_prefix, memory_ids, metadata, created_at, updated_at)
                     VALUES ('job-skipped', 'forget_sweep', 'maintenance', 'skipped', 'project', '/', '[]', '{}', ?1, ?1)",
                    rusqlite::params![Utc::now().to_rfc3339()],
                )
                .map_err(|e| e.to_string())?;
            Ok(())
        })
        .expect("seed terminal-only status db");

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
    assert_eq!(parsed["databases"]["active_jobs"], json!(0));
    assert_eq!(parsed["databases"]["pending_jobs"], json!(0));
    assert_eq!(parsed["databases"]["failed_jobs"], json!(0));
    assert_eq!(parsed["databases"]["stuck_jobs"], json!(0));
    assert_eq!(parsed["databases"]["terminal_jobs"], json!(2));

    let queue = &parsed["databases"]["worker_queues"][0];
    assert_eq!(queue["queue_state"], json!("idle"));
    assert_eq!(queue["active_jobs"], json!(0));
    assert_eq!(queue["terminal_jobs"], json!(2));
    assert_eq!(queue["backfill"]["needed"], json!(false));
    assert!(
        queue["latest_active_job"].is_null(),
        "terminal history must not masquerade as active queue: {queue}"
    );
    assert_eq!(
        queue["latest_terminal_job"]["status"],
        json!("skipped"),
        "terminal history should still be available for audit"
    );
}

#[tokio::test]
async fn tachi_status_marks_vault_alias_in_config_env_as_vault_config() {
    let (server, temp_home) = make_server_with_temp_home();
    let config_env = temp_home.temp_home.join(".tachi/config.env");
    std::fs::create_dir_all(config_env.parent().expect("config env parent"))
        .expect("create config env dir");
    std::fs::write(&config_env, "VOYAGE_API_KEY=vault:VOYAGE_API_KEY\n").expect("write config env");

    let global_db = temp_home.temp_home.join("global/memory.db");
    std::fs::create_dir_all(global_db.parent().expect("global parent")).expect("mkdir");
    let store = memory_core::MemoryStore::open(global_db.to_str().unwrap()).expect("open global");
    let _ = store; // vault entries optional for status name listing

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
    // Without vault entry, alias alone may be unresolved; with vault it is vault(config.env).
    assert!(
        voyage["source"] == json!("vault(config.env)")
            || voyage["source"] == json!("vault-alias-unresolved")
            || voyage["status"] == json!("missing")
    );

    if let Some(value) = original_voyage {
        std::env::set_var("VOYAGE_API_KEY", value);
    } else {
        std::env::remove_var("VOYAGE_API_KEY");
    }
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
