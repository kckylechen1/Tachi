use super::*;

#[tokio::test]
async fn dispatch_prompt_does_not_require_self_complete_without_tachi_mcp() {
    let server = make_server();
    let mut params = dispatch_params(Some("codex"), "read-only worker");
    params.inject_tachi_mcp = Some(false);
    params.mcp_access = Some(DispatchMcpAccessParams {
        inject_tachi_mcp: Some(false),
        inject_hub_mcps: Some(false),
        allowed_facades: vec!["tachi_memory".to_string()],
        allowed_mcp_servers: Vec::new(),
        github_read: Some(false),
        write_actions: Some(false),
        issue_refs: Vec::new(),
        pr_refs: Vec::new(),
        fallback: None,
    });

    let prompt = crate::dispatch_ops::assemble_prompt_with_trace(&server, &params)
        .await
        .prompt;
    assert!(
        prompt.contains("leader will call `tachi_task(action=\"complete\")`"),
        "{prompt}"
    );
    assert!(
        !prompt.contains("- Call `tachi_task(action=\"complete\")` when done"),
        "{prompt}"
    );

    params.inject_tachi_mcp = Some(true);
    let prompt = crate::dispatch_ops::assemble_prompt_with_trace(&server, &params)
        .await
        .prompt;
    assert!(
        prompt.contains("- Call `tachi_task(action=\"complete\")` when done"),
        "{prompt}"
    );
}

#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn dispatch_rejects_unsupported_mcp_injection_before_run_dir() {
    let _lock = crate::utils::global_test_lock()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let temp_home = tempfile::tempdir().expect("temp tachi home");
    let _home = EnvVarGuard::set_path("TACHI_HOME", temp_home.path());
    let server = make_server();
    let mut params = dispatch_params(Some("codex"), "unsupported mcp injection");
    params.inject_tachi_mcp = Some(true);

    let err = crate::dispatch_ops::handle_tachi_dispatch(&server, params)
        .await
        .expect_err("codex mcp injection should be rejected before run creation");
    assert!(err.contains("not supported for the codex backend"), "{err}");

    let runs_dir = temp_home.path().join("runs");
    assert!(
        !runs_dir.exists()
            || std::fs::read_dir(&runs_dir)
                .expect("read runs dir")
                .next()
                .is_none(),
        "unsupported dispatch validation must not leave orphaned run dirs"
    );
}
