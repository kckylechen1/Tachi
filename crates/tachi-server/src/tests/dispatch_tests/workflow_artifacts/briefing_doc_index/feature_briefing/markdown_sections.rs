use super::*;

#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn tachi_task_briefing_supports_markdown_layered_sections() {
    let _lock = crate::shell_ops::tachi_run_root_env_lock()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let temp_home = tempfile::tempdir().expect("temp tachi home");
    let temp_runs = tempfile::tempdir().expect("temp run root");
    let _home = EnvVarGuard::set_path("TACHI_HOME", temp_home.path());
    let _run_root = EnvVarGuard::set_path("TACHI_RUN_ROOT", temp_runs.path());
    let server = make_server();

    let mut params = task_params("briefing");
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
        "## Recommended Dispatch",
        "## Relevant Skills / Profiles",
        "## Wiki Decisions / Lessons",
        "## Memory Fragments / Checkpoints",
        "## Eval Evidence",
        "## Next Action",
    ] {
        assert!(body.contains(section), "missing {section}: {body}");
    }
    assert!(body.contains("Dispatch args:"), "{body}");
}
