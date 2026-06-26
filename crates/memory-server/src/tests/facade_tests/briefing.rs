use super::*;

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
            emit_continuity: false,
            compact: false,
            files: Vec::new(),
            proposal_id: None,
            review_status: None,
            notes: None,
            confirm: false,
            state_filter: None,
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
            emit_continuity: false,
            compact: false,
            files: Vec::new(),
            proposal_id: None,
            review_status: None,
            notes: None,
            confirm: false,
            state_filter: None,
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
async fn tachi_memory_briefing_uses_bound_project_db_when_cwd_project_is_unknown() {
    let (server, temp_home) = make_server_with_temp_home();
    let root = temp_home
        .temp_home
        .join("Bound Project Repo")
        .canonicalize()
        .unwrap_or_else(|_| temp_home.temp_home.join("Bound Project Repo"));
    std::fs::create_dir_all(root.join(".git")).expect("create fake git root");

    server
        .tachi_init_project_db(Parameters(InitProjectDbParams {
            project_root: Some(root.display().to_string()),
            db_relpath: ".tachi/memory.db".to_string(),
        }))
        .await
        .expect("project DB init should succeed");
    server
        .with_project_store(|store| {
            let mut entry = make_entry("bound-project-briefing-hit");
            entry.path = "/scratch/tachi/bound-project-briefing".to_string();
            entry.summary = "Bound project briefing hit".to_string();
            entry.text = "BoundProjectBriefingNeedle should surface from the hot-bound project DB."
                .to_string();
            entry.entities = vec!["BoundProjectBriefingNeedle".to_string()];
            store.upsert(&entry).map_err(|e| e.to_string())
        })
        .expect("seed bound project memory");

    let mut params = tachi_memory_params("briefing");
    params.format = Some("json".to_string());
    params.query = Some("BoundProjectBriefingNeedle".to_string());
    params.compact = true;
    let body = crate::facade_memory_ops::handle_tachi_memory(&server, params)
        .await
        .expect("briefing should succeed");
    let parsed: Value = serde_json::from_str(&body).expect("briefing JSON");

    // The project alias name now carries a stable-hash suffix; it still starts
    // with the sanitized repo basename.
    let expected_project = crate::path_utils::plan_c_dir_name_from_root(&root).expect("alias name");
    assert!(
        expected_project.starts_with("Bound_Project_Repo-"),
        "{expected_project}"
    );
    assert_eq!(parsed["project"], json!(expected_project));
    assert!(
        parsed["memories"]
            .as_array()
            .is_some_and(|rows| rows
                .iter()
                .any(|row| row["id"] == json!("bound-project-briefing-hit"))),
        "briefing should search the bound project DB even when cwd has no matching named DB: {parsed}"
    );
}

#[tokio::test]
async fn tachi_memory_briefing_filters_recent_checkpoints_to_bound_project() {
    let (server, temp_home) = make_server_with_temp_home();
    let root = temp_home
        .temp_home
        .join("Checkpoint Project Repo")
        .canonicalize()
        .unwrap_or_else(|_| temp_home.temp_home.join("Checkpoint Project Repo"));
    std::fs::create_dir_all(root.join(".git")).expect("create fake git root");

    server
        .tachi_init_project_db(Parameters(InitProjectDbParams {
            project_root: Some(root.display().to_string()),
            db_relpath: ".tachi/memory.db".to_string(),
        }))
        .await
        .expect("project DB init should succeed");

    server
        .with_global_store(|store| {
            let mut entry = make_entry("global-router-checkpoint-noise");
            entry.path = "/agent/checkpoints/2026-06-26".to_string();
            entry.summary = "Unrelated iStoreOS router audit checkpoint".to_string();
            entry.text = "This global checkpoint belongs to another workspace.".to_string();
            entry.timestamp = "2026-06-26T11:00:00Z".to_string();
            store.upsert(&entry).map_err(|e| e.to_string())
        })
        .expect("seed global checkpoint noise");
    server
        .with_project_store(|store| {
            let mut entry = make_entry("project-checkpoint-briefing-hit");
            entry.path = "/agent/checkpoints/2026-06-26".to_string();
            entry.summary = "Current project checkpoint should remain visible".to_string();
            entry.text = "This checkpoint belongs to the bound project.".to_string();
            entry.timestamp = "2026-06-26T10:00:00Z".to_string();
            store.upsert(&entry).map_err(|e| e.to_string())
        })
        .expect("seed project checkpoint");

    let mut params = tachi_memory_params("briefing");
    params.format = Some("json".to_string());
    params.query = Some("checkpoint".to_string());
    params.compact = true;
    let body = crate::facade_memory_ops::handle_tachi_memory(&server, params)
        .await
        .expect("briefing should succeed");
    let parsed: Value = serde_json::from_str(&body).expect("briefing JSON");

    let checkpoint_ids = parsed["recent_checkpoints"]
        .as_array()
        .expect("checkpoint rows")
        .iter()
        .filter_map(|row| row.get("id").and_then(Value::as_str))
        .collect::<Vec<_>>();
    assert!(
        checkpoint_ids.contains(&"project-checkpoint-briefing-hit"),
        "expected project checkpoint in briefing: {parsed}"
    );
    assert!(
        !checkpoint_ids.contains(&"global-router-checkpoint-noise"),
        "briefing should not mix global checkpoints into a project briefing: {parsed}"
    );
}

#[tokio::test]
async fn tachi_memory_briefing_excludes_stale_run_board_noise() {
    let (server, temp_home) = make_server_with_temp_home();
    let dispatch_id = format!(
        "20260626T000000Z-arena-stale-{}",
        uuid::Uuid::new_v4().as_simple()
    );
    let run_dir = temp_home.temp_home.join(".tachi/runs").join(&dispatch_id);
    std::fs::create_dir_all(&run_dir).expect("create stale run fixture");
    let stale_updated_at = (Utc::now() - chrono::Duration::seconds(120)).to_rfc3339();
    std::fs::write(
        run_dir.join("status.json"),
        serde_json::to_string(&json!({
            "dispatch_id": dispatch_id,
            "agent": "arena",
            "task": "Arena mission stale briefing noise",
            "state": "TASK_STATE_WORKING",
            "updated_at": stale_updated_at,
            "exit_code": null,
            "result_written": false,
            "timeout_secs": 5,
        }))
        .expect("serialize stale status fixture"),
    )
    .expect("write status fixture");

    let mut params = tachi_memory_params("briefing");
    params.format = Some("json".to_string());
    params.query = Some("current task".to_string());
    params.compact = true;
    let body = crate::facade_memory_ops::handle_tachi_memory(&server, params)
        .await
        .expect("briefing should succeed");
    let parsed: Value = serde_json::from_str(&body).expect("briefing JSON");

    assert!(
        !parsed["kanban"]["tasks"]
            .as_array()
            .unwrap()
            .iter()
            .any(|task| task["summary"] == json!("Arena mission stale briefing noise")),
        "briefing should not surface stale run board noise: {parsed}"
    );

    let _ = std::fs::remove_dir_all(&run_dir);
}

#[tokio::test]
async fn tachi_memory_briefing_excludes_stale_kanban_card_noise() {
    let server = make_server();
    let dispatch_id = format!(
        "20260626T000003Z-arena-stale-kanban-{}",
        uuid::Uuid::new_v4().as_simple()
    );
    let stale_updated_at = (Utc::now() - chrono::Duration::seconds(120)).to_rfc3339();
    server
        .with_global_store(|store| {
            let mut entry = make_entry("briefing-stale-kanban-card");
            entry.path = format!("/kanban/tasks/{dispatch_id}");
            entry.summary = "Arena persisted kanban briefing noise".to_string();
            entry.text = "Dispatch Task\nAgent: arena\nTask: Arena persisted kanban briefing noise"
                .to_string();
            entry.timestamp = stale_updated_at;
            entry.topic = "kanban".to_string();
            entry.keywords = vec!["kanban".to_string(), "dispatch".to_string()];
            entry.metadata = json!({
                "type": "a2a_task",
                "dispatch_id": dispatch_id,
                "a2a_state": "TASK_STATE_WORKING",
                "agent": "arena",
                "timeout_secs": 5,
            });
            store.upsert(&entry).map_err(|e| e.to_string())
        })
        .expect("seed stale kanban card");

    let mut params = tachi_memory_params("briefing");
    params.format = Some("json".to_string());
    params.query = Some("current task".to_string());
    params.compact = true;
    let body = crate::facade_memory_ops::handle_tachi_memory(&server, params)
        .await
        .expect("briefing should succeed");
    let parsed: Value = serde_json::from_str(&body).expect("briefing JSON");

    assert!(
        !parsed["kanban"]["tasks"]
            .as_array()
            .unwrap()
            .iter()
            .any(|task| task["summary"] == json!("Arena persisted kanban briefing noise")),
        "briefing should not surface stale persisted kanban noise: {parsed}"
    );
}
