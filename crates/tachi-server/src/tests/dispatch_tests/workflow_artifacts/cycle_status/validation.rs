use super::*;

#[tokio::test]
async fn tachi_task_status_cycle_requires_flow_issue_or_pr_target() {
    let server = make_server();
    let params = task_params("status");
    let err = server
        .tachi_task(Parameters(params))
        .await
        .expect_err("missing target should fail before GitHub access");
    assert_eq!(err, "dispatch_id is required when action='status'");
}

#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn tachi_task_status_dispatch_id_wins_over_cycle_selector_errors() {
    let _lock = crate::utils::global_test_lock()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let temp_runs = tempfile::tempdir().expect("temp run root");
    let _run_root = EnvVarGuard::set_path("TACHI_RUN_ROOT", temp_runs.path());
    let server = make_server();
    std::fs::create_dir_all(crate::dispatch_ops::runs_dir_for_server(&server))
        .expect("create run root");
    let mut params = task_params("status");
    params.dispatch_id = Some("missing-mixed-dispatch".to_string());
    params.flow_id = Some("flow_must_not_fall_through".to_string());
    params.issue_ref = Some("owner/repo#1712".to_string());
    params.pr_ref = Some("owner/repo#2712".to_string());

    let err = server
        .tachi_task(Parameters(params))
        .await
        .expect_err("dispatch_id must remain the status discriminator");
    assert!(
        err.contains("dispatch 'missing-mixed-dispatch' was not found"),
        "mixed keys must preserve the old dispatch lookup error: {err}"
    );
    assert!(
        !err.contains("status lifecycle view requires"),
        "mixed keys must not fall through to the nested cycle view: {err}"
    );
}
