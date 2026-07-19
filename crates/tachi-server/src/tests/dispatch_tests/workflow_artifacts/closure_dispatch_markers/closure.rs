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

// Hold the process-wide home lock across the body: this test uses the
// non-isolating make_server(), and its wiki-write path re-reads TACHI_HOME from
// the env. A parallel TempHomeGuard test can repoint TACHI_HOME mid-test and
// flake this. This is the same fix v2 applied to
// tachi_wiki_write_can_attach_projected_pattern_refs (references.rs). The
// current-thread tokio runtime makes holding a std MutexGuard across `.await`
// sound here.
#[allow(clippy::await_holding_lock)]
#[tokio::test]
async fn tachi_task_close_loop_writes_wiki_with_references() {
    let _home_lock = crate::utils::global_test_lock()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
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
    let refs = entry["metadata"]["evidence_refs_v1"]
        .as_array()
        .expect("typed evidence refs");
    assert_eq!(refs[0]["ref"], json!("kckylechen1/tachi#194"));
    assert_eq!(
        refs[1]["ref"],
        json!("docs/engineering/architecture/agent-flow.md")
    );
    assert_eq!(refs[2]["ref"], json!("#153"));
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
