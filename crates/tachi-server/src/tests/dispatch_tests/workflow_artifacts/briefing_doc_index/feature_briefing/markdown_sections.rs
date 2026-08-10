use super::*;

#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn tachi_task_briefing_supports_markdown_layered_sections() {
    let _lock = crate::utils::global_test_lock()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let temp_home = tempfile::tempdir().expect("temp tachi home");
    let temp_runs = tempfile::tempdir().expect("temp run root");
    let _home = EnvVarGuard::set_path("TACHI_HOME", temp_home.path());
    let _run_root = EnvVarGuard::set_path("TACHI_RUN_ROOT", temp_runs.path());
    let server = make_server();

    let mut params = task_params("brief");
    params.format = Some("markdown".to_string());
    params.task = Some("Prepare feature handoff".to_string());
    params.doc_paths = vec!["docs/engineering/architecture/subagent-eval-system.md".to_string()];

    let body = server
        .tachi_task(Parameters(params))
        .await
        .expect("markdown briefing should succeed");

    assert!(body.starts_with("# Feature Briefing"), "{body}");
    for section in [
        "## Project Work Record",
        "## Canonical Docs / Specs",
        "## Run Artifacts",
        "## Board State",
        "## Guide / SOP",
        "## Feedback Rules",
        "## Native Subagent Handoff",
        "## Relevant Skills / Profiles",
        "## Wiki Decisions / Lessons",
        "## Memory Fragments / Checkpoints",
        "## Eval Evidence",
        "## Next Action",
    ] {
        assert!(body.contains(section), "missing {section}: {body}");
    }
    assert!(body.contains("Handoff context:"), "{body}");
    assert!(body.contains("host harness's native subagent"), "{body}");
    assert!(!body.contains("tachi_task(action=\"dispatch\")"), "{body}");
}

/// #1001 round 2 item 4: the task-facing `feature_briefing` markdown carried
/// `value["presence"]` in its JSON payload but never rendered it — every
/// other section (Board State, Guide/SOP, etc.) had a markdown section,
/// presence did not. This primes a live claim (mirroring what the dispatch
/// hook does) before requesting the SAME `issue_ref` in markdown format and
/// asserts the presence section actually appears with the claim's identity
/// and target — proving the markdown renderer, not just the JSON payload,
/// carries presence.
#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn tachi_task_briefing_markdown_renders_presence_section() {
    let _lock = crate::utils::global_test_lock()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let temp_home = tempfile::tempdir().expect("temp tachi home");
    let temp_runs = tempfile::tempdir().expect("temp run root");
    let _home = EnvVarGuard::set_path("TACHI_HOME", temp_home.path());
    let _run_root = EnvVarGuard::set_path("TACHI_RUN_ROOT", temp_runs.path());
    let server = make_server();

    server.set_session_identity(Some("seat-presence-md".to_string()), None, None);
    crate::claims_ops::auto_register_or_heartbeat_claim(
        &server,
        &crate::claims_ops::ClaimHookInput {
            issue_ref: Some("org/repo#1004".to_string()),
            flow_id: None,
            dispatch_id: None,
            branch: Some("feat/presence-md".to_string()),
            declared_file_scope: None,
        },
    );

    let mut params = task_params("brief");
    params.format = Some("markdown".to_string());
    params.task = Some("Prepare feature handoff".to_string());
    params.issue_ref = Some("org/repo#1004".to_string());

    let body = server
        .tachi_task(Parameters(params))
        .await
        .expect("markdown briefing should succeed");

    assert!(
        body.contains("## Presence 工位表"),
        "markdown must render a presence section: {body}"
    );
    assert!(
        body.contains("seat-presence-md"),
        "presence section must show the live claim's session_client: {body}"
    );
    assert!(
        body.contains("org/repo#1004"),
        "presence section must show the claim's issue_ref target: {body}"
    );
}
