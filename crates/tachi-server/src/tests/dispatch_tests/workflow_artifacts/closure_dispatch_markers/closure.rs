use super::*;

#[tokio::test]
async fn tachi_gh_close_loop_dry_run_builds_references() {
    let server = make_server();
    let params: crate::tool_params::TachiGhParams = serde_json::from_value(json!({
        "action": "close_loop",
        "dry_run": true,
        "issue_ref": "kckylechen1/tachi#194",
        "doc_paths": ["docs/engineering/architecture/agent-flow.md"],
        "related_issues": ["#153", "kckylechen1/tachi#194"],
    }))
    .expect("GH close_loop preview params");

    let raw = server
        .tachi_gh(Parameters(params))
        .await
        .expect("GH close_loop preview should succeed");
    let parsed: Value = serde_json::from_str(&raw).expect("GH response JSON");
    assert_eq!(parsed["action"], json!("close_loop"));
    assert_eq!(parsed["dry_run"], json!(true));
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

// Hold the process-wide home lock across the body: this test uses the
// non-isolating make_server(), and its wiki-write path re-reads TACHI_HOME from
// the env. A parallel TempHomeGuard test can repoint TACHI_HOME mid-test and
// flake this. This is the same fix v2 applied to
// tachi_wiki_write_can_attach_projected_pattern_refs (references.rs). The
// current-thread tokio runtime makes holding a std MutexGuard across `.await`
// sound here.
#[allow(clippy::await_holding_lock)]
#[tokio::test]
async fn tachi_gh_close_loop_writes_wiki_with_references() {
    let _home_lock = crate::utils::global_test_lock()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let server = make_server();
    let params: crate::tool_params::TachiGhParams = serde_json::from_value(json!({
        "action": "close_loop",
        "issue_ref": "kckylechen1/tachi#194",
        "doc_paths": ["docs/engineering/architecture/agent-flow.md"],
        "related_issues": ["#153"],
        "wiki_title": "GH closure facade smoke",
        "wiki_text": "Closed loop lesson through tachi_gh facade.",
        "wiki_topic": "gh-closure-facade",
        "post_comment": false,
        "force": true,
    }))
    .expect("GH close_loop params");

    let raw = server
        .tachi_gh(Parameters(params))
        .await
        .expect("GH close_loop should succeed");
    let parsed: Value = serde_json::from_str(&raw).expect("GH response JSON");
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
    let refs = entry["metadata"]["evidence_refs_v1"]
        .as_array()
        .expect("typed evidence refs");
    assert_eq!(refs.len(), 3);
    assert_eq!(refs[0]["ref"], json!("kckylechen1/tachi#194"));
    assert_eq!(
        refs[1]["ref"],
        json!("docs/engineering/architecture/agent-flow.md")
    );
    assert_eq!(refs[2]["ref"], json!("#153"));
    assert!(entry["metadata"].get("source_refs").is_none());
    assert_eq!(
        entry["metadata"]["promotion"]["source_refs"],
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
async fn tachi_gh_close_loop_marks_flow_complete() {
    let _lock = crate::utils::global_test_lock()
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

    let close_params: crate::tool_params::TachiGhParams = serde_json::from_value(json!({
        "action": "close_loop",
        "flow_id": flow_id,
        "issue_ref": "kckylechen1/tachi#239",
        "wiki_title": "Close loop marker smoke",
        "wiki_text": "Close loop should mark the flow complete.",
        "wiki_topic": "close-loop-marker",
        "post_comment": false,
        "force": true,
    }))
    .expect("GH close_loop params");
    let raw = server
        .tachi_gh(Parameters(close_params))
        .await
        .expect("close_loop should succeed");
    let parsed: Value = serde_json::from_str(&raw).expect("close_loop response JSON");
    assert_eq!(parsed["ok"], json!(true));

    let run_dir = crate::task_lifecycle::run_dir_for_flow_id(flow_id).expect("run dir");
    assert!(run_dir.join("close_loop.json").exists());
    let status: Value = serde_json::from_str(
        &std::fs::read_to_string(run_dir.join("status.json")).expect("status"),
    )
    .expect("status json");
    assert_eq!(status["state"], json!("closed_loop"));
    assert!(status["artifacts"]["close_loop"]
        .as_str()
        .is_some_and(|path| path.ends_with("close_loop.json")));

    let mut status_params = task_params("status");
    status_params.flow_id = Some(flow_id.to_string());
    let status_raw = server
        .tachi_task(Parameters(status_params))
        .await
        .expect("Task lifecycle status should succeed");
    let lifecycle: Value = serde_json::from_str(&status_raw).expect("Task status JSON");
    assert_eq!(lifecycle["cycle"]["stage"], json!("closed"));
}

#[allow(clippy::await_holding_lock)]
#[tokio::test]
async fn tachi_gh_close_loop_marks_cycle_stage_closed_after_write() {
    let _lock = crate::utils::global_test_lock()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let temp_home = tempfile::tempdir().expect("temp tachi home");
    let temp_runs = tempfile::tempdir().expect("temp run root");
    let _home = EnvVarGuard::set_path("TACHI_HOME", temp_home.path());
    let _run_root = EnvVarGuard::set_path("TACHI_RUN_ROOT", temp_runs.path());
    let server = make_server();
    let flow_id = "flow_20260810T000001Z_gh_close_cycle_test";
    let issue = crate::task_lifecycle::IssueSnapshot {
        repo: "kckylechen1/tachi".to_string(),
        number: 1713,
        title: "GH close loop cycle stage".to_string(),
        body: Some("## Acceptance criteria\n- close_loop stage is closed.".to_string()),
        labels: Vec::new(),
        state: Some("OPEN".to_string()),
        url: "https://github.com/kckylechen1/tachi/issues/1713".to_string(),
        doc_paths: Vec::new(),
        spec_paths: Vec::new(),
    };
    let automation_plan = crate::task_lifecycle::build_issue_automation_plan(&issue, None);
    crate::task_lifecycle::write_intake_flow_artifacts(
        flow_id,
        "GH close loop cycle stage",
        &issue,
        &automation_plan,
    )
    .expect("write intake artifacts");

    let params: crate::tool_params::TachiGhParams = serde_json::from_value(json!({
        "action": "close_loop",
        "issue_ref": "kckylechen1/tachi#1713",
        "flow_id": flow_id,
        "wiki_title": "GH close loop cycle stage",
        "wiki_text": "The relocated GH closure writes the lifecycle marker.",
        "post_comment": false,
        "force": true,
    }))
    .expect("GH close_loop params");
    let raw = server
        .tachi_gh(Parameters(params))
        .await
        .expect("GH close_loop should succeed");
    let response: Value = serde_json::from_str(&raw).expect("GH close_loop response JSON");
    assert_eq!(response["ok"], json!(true));

    let mut status_params = task_params("status");
    status_params.flow_id = Some(flow_id.to_string());
    let status_raw = server
        .tachi_task(Parameters(status_params))
        .await
        .expect("Task lifecycle status should succeed");
    let status: Value = serde_json::from_str(&status_raw).expect("Task status JSON");
    assert_eq!(status["cycle"]["stage"], json!("closed"));
}

#[allow(clippy::await_holding_lock)]
#[tokio::test]
async fn tachi_gh_close_loop_dry_run_is_pure_preview() {
    let _lock = crate::utils::global_test_lock()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let temp_home = tempfile::tempdir().expect("temp tachi home");
    let temp_runs = tempfile::tempdir().expect("temp run root");
    let _home = EnvVarGuard::set_path("TACHI_HOME", temp_home.path());
    let _run_root = EnvVarGuard::set_path("TACHI_RUN_ROOT", temp_runs.path());
    let server = make_server();
    let flow_id = "flow_20260810T000002Z_gh_close_preview_test";
    let before_memories = server
        .with_global_store_read(|store| {
            store
                .connection()
                .query_row("SELECT COUNT(*) FROM memories", [], |row| {
                    row.get::<_, i64>(0)
                })
                .map_err(|error| error.to_string())
        })
        .expect("count memories before preview");
    let before_events = server
        .with_global_store_read(|store| {
            store
                .connection()
                .query_row("SELECT COUNT(*) FROM tachi_events", [], |row| {
                    row.get::<_, i64>(0)
                })
                .map_err(|error| error.to_string())
        })
        .expect("count events before preview");

    let params: crate::tool_params::TachiGhParams = serde_json::from_value(json!({
        "action": "close_loop",
        "dry_run": true,
        "issue_ref": "kckylechen1/tachi#1713",
        "flow_id": flow_id,
        "wiki_title": "Preview only",
        "wiki_text": "This must never be written.",
        "post_comment": true,
        "force": true,
    }))
    .expect("GH close_loop preview params");
    let raw = server
        .tachi_gh(Parameters(params))
        .await
        .expect("GH close_loop preview should succeed");
    let response: Value = serde_json::from_str(&raw).expect("GH close_loop preview JSON");
    assert_eq!(response["ok"], json!(true));
    assert_eq!(response["dry_run"], json!(true));
    assert!(response["references"].is_array());
    assert!(response["promotion_plan"].is_object());
    assert!(response.get("wiki").is_none());
    assert!(response.get("closure_actions").is_none());
    assert!(
        !crate::task_lifecycle::run_dir_for_flow_id(flow_id)
            .expect("preview run dir")
            .exists(),
        "dry_run must not create flow artifacts"
    );
    let after_memories = server
        .with_global_store_read(|store| {
            store
                .connection()
                .query_row("SELECT COUNT(*) FROM memories", [], |row| {
                    row.get::<_, i64>(0)
                })
                .map_err(|error| error.to_string())
        })
        .expect("count memories after preview");
    let after_events = server
        .with_global_store_read(|store| {
            store
                .connection()
                .query_row("SELECT COUNT(*) FROM tachi_events", [], |row| {
                    row.get::<_, i64>(0)
                })
                .map_err(|error| error.to_string())
        })
        .expect("count events after preview");
    assert_eq!(
        after_memories, before_memories,
        "preview must not write wiki"
    );
    assert_eq!(
        after_events, before_events,
        "preview must not emit feedback"
    );
}
