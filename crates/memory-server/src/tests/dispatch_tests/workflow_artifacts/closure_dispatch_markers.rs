use super::*;

#[tokio::test]
async fn tachi_task_build_references_reuses_workflow_closure() {
    let server = make_server();
    let mut params = task_params("build_references");
    params.issue_ref = Some("kckylechen1/tachi#194".to_string());
    params.doc_paths = vec!["docs/engineering/architecture/agent-flow.md".to_string()];
    params.related_issues = vec!["#153".to_string(), "kckylechen1/tachi#194".to_string()];

    let raw = server
        .tachi_task(Parameters(params))
        .await
        .expect("task build_references should succeed");
    let parsed: Value = serde_json::from_str(&raw).expect("task response JSON");
    assert_eq!(
        parsed["references"],
        json!([
            "kckylechen1/tachi#194",
            "docs/engineering/architecture/agent-flow.md",
            "#153"
        ])
    );
    assert_eq!(
        parsed["promotion_plan"]["requires_explicit_invocation"],
        json!(true)
    );
    assert_eq!(
        parsed["promotion_plan"]["automatic_double_write"],
        json!(false)
    );
    assert!(parsed["promotion_plan"]["destinations"]
        .as_array()
        .is_some_and(|destinations| destinations
            .iter()
            .any(|dest| dest["destination"] == json!("feedback_rule"))));
}

#[tokio::test]
async fn tachi_task_close_loop_writes_wiki_with_references() {
    let server = make_server();
    let mut params = task_params("close_loop");
    params.issue_ref = Some("kckylechen1/tachi#194".to_string());
    params.doc_paths = vec!["docs/engineering/architecture/agent-flow.md".to_string()];
    params.related_issues = vec!["#153".to_string()];
    params.wiki_title = Some("Task closure facade smoke".to_string());
    params.wiki_text = Some("Closed loop lesson through tachi_task facade.".to_string());
    params.wiki_topic = Some("task-closure-facade".to_string());
    params.force = true;

    let raw = server
        .tachi_task(Parameters(params))
        .await
        .expect("task close_loop should succeed");
    let parsed: Value = serde_json::from_str(&raw).expect("task response JSON");
    assert_eq!(parsed["ok"], json!(true));
    assert_eq!(parsed["action"], json!("close_loop"));
    assert_eq!(
        parsed["promotion_plan"]["automatic_double_write"],
        json!(false)
    );
    let wiki_id = parsed["wiki"]["id"].as_str().expect("wiki id");
    let fetched = server
        .get_memory(Parameters(GetMemoryParams {
            id: wiki_id.to_string(),
            include_archived: false,
            project: None,
        }))
        .await
        .expect("get wiki entry");
    let entry: Value = serde_json::from_str(&fetched).expect("entry JSON");
    assert_eq!(
        entry["metadata"]["source_refs"],
        json!([
            "kckylechen1/tachi#194",
            "docs/engineering/architecture/agent-flow.md",
            "#153"
        ])
    );
    assert_eq!(
        entry["metadata"]["promotion"]["decision_mode"],
        json!("explicit_invocation")
    );
    assert_eq!(
        entry["metadata"]["promotion"]["destination_layer"],
        json!("wiki")
    );
    assert_eq!(
        entry["metadata"]["promotion"]["automatic_double_write"],
        json!(false)
    );
}

#[allow(clippy::await_holding_lock)]
#[tokio::test]
async fn tachi_task_close_loop_marks_flow_complete_for_ux_matrix() {
    let _lock = crate::shell_ops::tachi_run_root_env_lock()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let temp_home = tempfile::tempdir().expect("temp tachi home");
    let temp_runs = tempfile::tempdir().expect("temp run root");
    let _home = EnvVarGuard::set_path("TACHI_HOME", temp_home.path());
    let _run_root = EnvVarGuard::set_path("TACHI_RUN_ROOT", temp_runs.path());
    let server = make_server();
    let flow_id = "flow_20260608T000006Z_close_loop_marker_test";
    let issue = crate::task_lifecycle::IssueSnapshot {
        repo: "kckylechen1/tachi".to_string(),
        number: 239,
        title: "Persist close_loop marker".to_string(),
        body: Some("## Acceptance criteria\n- close_loop marks the flow complete.".to_string()),
        labels: Vec::new(),
        state: Some("OPEN".to_string()),
        url: "https://github.com/kckylechen1/tachi/issues/239".to_string(),
        doc_paths: vec!["docs/engineering/architecture/credential-adapters-cleanup.md".to_string()],
        spec_paths: Vec::new(),
    };
    let automation_plan = crate::task_lifecycle::build_issue_automation_plan(&issue, None);
    crate::task_lifecycle::write_intake_flow_artifacts(
        flow_id,
        "Persist close_loop marker",
        &issue,
        &automation_plan,
    )
    .expect("write intake artifacts");

    let mut close_params = task_params("close_loop");
    close_params.flow_id = Some(flow_id.to_string());
    close_params.issue_ref = Some("kckylechen1/tachi#239".to_string());
    close_params.wiki_title = Some("Close loop marker smoke".to_string());
    close_params.wiki_text = Some("Close loop should mark the flow complete.".to_string());
    close_params.wiki_topic = Some("close-loop-marker".to_string());
    close_params.force = true;
    let raw = server
        .tachi_task(Parameters(close_params))
        .await
        .expect("close_loop should succeed");
    let parsed: Value = serde_json::from_str(&raw).expect("close_loop response JSON");
    assert_eq!(parsed["ok"], json!(true));

    let run_dir = crate::shell_ops::run_dir_for_flow_id(flow_id).expect("run dir");
    assert!(run_dir.join("close_loop.json").exists());
    let status: Value = serde_json::from_str(
        &std::fs::read_to_string(run_dir.join("status.json")).expect("status"),
    )
    .expect("status json");
    assert_eq!(status["state"], json!("closed_loop"));
    assert!(status["artifacts"]["close_loop"]
        .as_str()
        .is_some_and(|path| path.ends_with("close_loop.json")));

    let mut ux_params = task_params("ux_matrix");
    ux_params.flow_id = Some(flow_id.to_string());
    let raw = server
        .tachi_task(Parameters(ux_params))
        .await
        .expect("ux_matrix should succeed");
    let parsed: Value = serde_json::from_str(&raw).expect("ux_matrix response JSON");
    assert_eq!(parsed["overall"], json!("complete"));
    let matrix = parsed["matrix"].as_array().expect("matrix array");
    assert!(matrix
        .iter()
        .any(|step| { step["id"] == json!("close_loop") && step["status"] == json!("passed") }));
}

#[test]
#[allow(clippy::await_holding_lock)]
fn tachi_task_dispatch_marker_updates_flow_status_idempotently() {
    let _lock = crate::shell_ops::tachi_run_root_env_lock()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let temp_home = tempfile::tempdir().expect("temp tachi home");
    let _tachi_home = EnvVarGuard::set_path("TACHI_HOME", temp_home.path());
    let flow_id = "flow_20260608T000007Z_dispatch_marker_test";
    let dispatch_id = "20260608T000007Z-custom-marker";

    crate::task_lifecycle::mark_task_dispatch(
        flow_id,
        dispatch_id,
        json!({
            "agent": "custom",
            "profile": "deepseek_explore",
            "task": "read-only review",
        }),
    )
    .expect("mark dispatch");
    crate::task_lifecycle::mark_task_dispatch(
        flow_id,
        dispatch_id,
        json!({
            "agent": "custom",
            "profile": "deepseek_explore",
            "task": "read-only review",
        }),
    )
    .expect("mark dispatch idempotently");
    assert!(
        crate::task_lifecycle::mark_task_dispatch(flow_id, "../bad", json!({})).is_err(),
        "dispatch marker ids must stay filename-safe"
    );

    let run_dir = crate::shell_ops::run_dir_for_flow_id(flow_id).expect("flow run dir");
    let status: Value = serde_json::from_str(
        &std::fs::read_to_string(run_dir.join("status.json")).expect("status"),
    )
    .expect("status JSON");
    assert_eq!(status["dispatch_ids"], json!([dispatch_id]));
    assert_eq!(status["stage"], json!("dispatch"));
    assert_eq!(status["state"], json!("dispatched"));
    let card_path = status["artifacts"]["dispatches"][dispatch_id]
        .as_str()
        .expect("dispatch card path");
    assert!(
        std::path::Path::new(card_path).exists(),
        "dispatch card should exist: {status:#}"
    );
    assert_eq!(
        status["dispatch_cards"].as_array().map(Vec::len),
        Some(1),
        "dispatch card list should not duplicate entries: {status:#}"
    );
    let events = std::fs::read_to_string(run_dir.join("events.jsonl")).expect("events");
    assert!(events.contains("\"event\":\"dispatch_linked\""), "{events}");
}

#[test]
#[allow(clippy::await_holding_lock)]
fn tachi_task_dispatch_completion_marker_updates_card_and_status_idempotently() {
    let _lock = crate::shell_ops::tachi_run_root_env_lock()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let temp_home = tempfile::tempdir().expect("temp tachi home");
    let _tachi_home = EnvVarGuard::set_path("TACHI_HOME", temp_home.path());
    let flow_id = "flow_20260609T000001Z_dispatch_completion_marker_test";
    let dispatch_id = "20260609T000001Z-custom-complete";

    crate::task_lifecycle::mark_task_dispatch(
        flow_id,
        dispatch_id,
        json!({
            "agent": "custom",
            "profile": "glm_51_impl",
            "task": "implementation",
        }),
    )
    .expect("mark dispatch");
    crate::task_lifecycle::mark_task_dispatch_completion(
        flow_id,
        dispatch_id,
        json!({
            "task_id": "eval-link-001",
            "outcome": "success",
            "eval_memory_id": "memory-eval-001",
            "eval_path": "/eval/2026-06-09/eval-link-001",
            "verification_present": true,
            "tests_run": ["cargo test -p memory-server dispatch_tests"],
        }),
    )
    .expect("mark completion");
    crate::task_lifecycle::mark_task_dispatch_completion(
        flow_id,
        dispatch_id,
        json!({
            "task_id": "eval-link-001",
            "outcome": "success",
            "eval_memory_id": "memory-eval-001",
            "eval_path": "/eval/2026-06-09/eval-link-001",
            "verification_present": true,
        }),
    )
    .expect("mark completion idempotently");

    let run_dir = crate::shell_ops::run_dir_for_flow_id(flow_id).expect("flow run dir");
    let status: Value = serde_json::from_str(
        &std::fs::read_to_string(run_dir.join("status.json")).expect("status"),
    )
    .expect("status JSON");
    assert_eq!(status["completed_dispatch_ids"], json!([dispatch_id]));
    assert_eq!(status["stage"], json!("eval"));
    assert_eq!(status["state"], json!("dispatch_completed"));
    assert_eq!(
        status["dispatch_eval"][dispatch_id]["eval_memory_id"],
        json!("memory-eval-001")
    );
    assert_eq!(
        status["artifacts"]["dispatch_completions"][dispatch_id]["outcome"],
        json!("success")
    );
    let card_path = status["artifacts"]["dispatches"][dispatch_id]
        .as_str()
        .expect("dispatch card path");
    let card: Value = serde_json::from_str(&std::fs::read_to_string(card_path).expect("card"))
        .expect("card JSON");
    assert_eq!(
        card["completion"]["eval_path"],
        json!("/eval/2026-06-09/eval-link-001")
    );
    assert_eq!(
        card["completion_history"].as_array().map(Vec::len),
        Some(1),
        "same eval should not duplicate completion history: {card:#}"
    );
    let events = std::fs::read_to_string(run_dir.join("events.jsonl")).expect("events");
    assert!(
        events.contains("\"event\":\"dispatch_completed\""),
        "{events}"
    );
}

#[allow(clippy::await_holding_lock)]
#[tokio::test]
async fn tachi_dispatch_with_flow_id_records_dispatch_card() {
    let (server, _temp_home) = make_server_with_temp_home();
    let tmp = tempfile::tempdir().expect("temp dispatch cwd");
    let flow_id = "flow_20260608T000008Z_dispatch_card_test";
    let run_dir = crate::shell_ops::run_dir_for_flow_id(flow_id).expect("flow run dir");
    std::fs::create_dir_all(&run_dir).expect("create flow run dir");
    std::fs::write(
        run_dir.join("status.json"),
        serde_json::to_string_pretty(&json!({
            "flow_id": flow_id,
            "dispatch_ids": [],
        }))
        .expect("serialize status"),
    )
    .expect("seed status");

    let mut params = dispatch_params(Some("custom"), "smoke flow dispatch marker");
    params.command = vec![
        "python3".to_string(),
        "-c".to_string(),
        "print('ok')".to_string(),
    ];
    params.cwd = Some(tmp.path().to_string_lossy().to_string());
    params.profile = Some("glm_51_impl".to_string());
    params.flow_id = Some(flow_id.to_string());
    params.issue_ref = Some("kckylechen1/tachi#194".to_string());

    let raw = crate::dispatch_ops::handle_tachi_dispatch(&server, params)
        .await
        .expect("custom dispatch should start");
    let response: Value = serde_json::from_str(&raw).expect("dispatch JSON");
    let dispatch_id = response["dispatch_id"].as_str().expect("dispatch id");

    let status: Value = serde_json::from_str(
        &std::fs::read_to_string(run_dir.join("status.json")).expect("status"),
    )
    .expect("status JSON");
    assert!(
        status["dispatch_ids"]
            .as_array()
            .is_some_and(|ids| ids.iter().any(|id| id.as_str() == Some(dispatch_id))),
        "flow status should include dispatch id {dispatch_id}: {status:#}"
    );
    assert!(
        status["artifacts"]["dispatches"][dispatch_id]
            .as_str()
            .is_some_and(|path| path.ends_with(".json")),
        "flow status should link compact dispatch card: {status:#}"
    );
    let card_path = status["artifacts"]["dispatches"][dispatch_id]
        .as_str()
        .expect("dispatch card path");
    let card: Value = serde_json::from_str(&std::fs::read_to_string(card_path).expect("card"))
        .expect("card JSON");
    assert_eq!(card["suggested_complete"]["tool"], json!("tachi_task"));
    assert_eq!(
        card["suggested_complete"]["arguments"]["action"],
        json!("complete")
    );
    assert_eq!(
        card["suggested_complete"]["arguments"]["dispatch_id"],
        json!(dispatch_id)
    );
    assert_eq!(
        card["suggested_complete"]["arguments"]["flow_id"],
        json!(flow_id)
    );
    let events = std::fs::read_to_string(run_dir.join("events.jsonl")).expect("events");
    assert!(
        events.contains(dispatch_id) && events.contains("\"event\":\"dispatch_linked\""),
        "{events}"
    );
}

#[allow(clippy::await_holding_lock)]
#[tokio::test]
async fn tachi_task_board_filters_to_flow_dispatch_ids() {
    let _lock = crate::shell_ops::tachi_run_root_env_lock()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let server = make_server();
    let temp_home = tempfile::tempdir().expect("temp tachi home");
    let _tachi_home = EnvVarGuard::set_path("TACHI_HOME", temp_home.path());
    let flow_id = "flow_20260609T000005Z_board_flow_filter";
    let dispatch_id = "20260609T000005Z-codex-flow";
    let other_dispatch_id = "20260609T000006Z-codex-other";

    crate::task_lifecycle::mark_task_dispatch(
        flow_id,
        dispatch_id,
        json!({
            "agent": "codex",
            "profile": "codex_53_fast",
            "task": "flow task",
        }),
    )
    .expect("mark dispatch");
    for (id, task) in [
        (dispatch_id, "flow task"),
        (other_dispatch_id, "other task"),
    ] {
        let run_dir = temp_home.path().join("runs").join(id);
        std::fs::create_dir_all(&run_dir).expect("create run dir");
        if id == dispatch_id {
            std::fs::write(run_dir.join("result.md"), "worker completed").expect("result");
        }
        std::fs::write(
            run_dir.join("status.json"),
            serde_json::to_string_pretty(&json!({
                "dispatch_id": id,
                "agent": "codex",
                "task": task,
                "state": "TASK_STATE_COMPLETED",
                "exit_code": 0,
                "updated_at": Utc::now().to_rfc3339(),
            }))
            .expect("status json"),
        )
        .expect("write status");
    }

    let mut params = task_params("board");
    params.flow_id = Some(flow_id.to_string());
    params.limit = Some(20);
    let raw = server
        .tachi_task(Parameters(params))
        .await
        .expect("board should succeed");
    let board: Value = serde_json::from_str(&raw).expect("board JSON");
    assert_eq!(board["flow_id"], json!(flow_id), "{board:#}");
    assert_eq!(board["run_count"], json!(1), "{board:#}");
    let tasks = board["tasks"].as_array().expect("tasks");
    assert_eq!(tasks.len(), 1, "{board:#}");
    assert_eq!(tasks[0]["dispatch_id"], json!(dispatch_id), "{board:#}");
    assert_eq!(
        tasks[0]["state"],
        json!("TASK_STATE_COMPLETED"),
        "{board:#}"
    );
    assert_eq!(tasks[0]["exit_code"], json!(0), "{board:#}");
    assert_eq!(tasks[0]["result_written"], json!(true), "{board:#}");
    assert_eq!(tasks[0]["state_source"], json!("run"), "{board:#}");
    assert!(
        tasks[0]["run_dir"]
            .as_str()
            .is_some_and(|path| path.ends_with(dispatch_id)),
        "{board:#}"
    );
}
