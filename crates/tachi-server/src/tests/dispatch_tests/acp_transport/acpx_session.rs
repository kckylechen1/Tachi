use super::*;

#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn dispatch_acpx_session_mode_derives_raven_for_a_review_lane() {
    let _lock = crate::shell_ops::tachi_run_root_env_lock()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let temp_home = tempfile::tempdir().expect("temp tachi home");
    let temp_python = tempfile::tempdir().expect("temp python module");
    std::fs::write(
        temp_python.path().join("fake_acpx_session.py"),
        r#"import json
print(json.dumps({"type": "message", "message": "reviewing"}))
print(json.dumps({"event": "end_turn", "final_response": "raven done"}))
"#,
    )
    .expect("fake acpx session module");

    let _home = EnvVarGuard::set_path("TACHI_HOME", temp_home.path());
    let _pythonpath = EnvVarGuard::set_path("PYTHONPATH", temp_python.path());
    let _acpx_command = EnvVarGuard::set_value("TACHI_ACPX_COMMAND", "python3");
    let _acpx_args = EnvVarGuard::set_value("TACHI_ACPX_ARGS", "-m fake_acpx_session");
    let _acpx_agent = EnvVarGuard::set_value("TACHI_ACPX_AGENT", "codex");
    let _acpx_mode = EnvVarGuard::set_value("TACHI_ACPX_RUN_MODE", "session");
    let _acpx_session = EnvVarGuard::set_value("TACHI_ACPX_SESSION", "");
    let _watchdog_polls = EnvVarGuard::set_value("TACHI_DISPATCH_WATCHDOG_POLLS", "1");
    let _watchdog_poll_ms = EnvVarGuard::set_value("TACHI_DISPATCH_WATCHDOG_POLL_MS", "10");

    let server = make_server();
    let mut params = dispatch_params(Some("codex"), "Review through fake acpx session");
    params.harness_transport = Some("acpx".to_string());
    params.cwd = Some(temp_home.path().to_string_lossy().to_string());
    params.unmanaged_cwd = Some(true);
    // The review lane is declared by `stage`, not by `profile`, and that is
    // load-bearing rather than cosmetic.
    //
    // This test's subject is acpx session plumbing: a review lane must derive the
    // `raven` session, reach the acpx process as `-s raven`, and be echoed back in
    // the response and in status.json. `derive_acpx_session` builds that name from
    // `profile` OR `stage` (`acpx::spec`), so either input exercises the subject.
    //
    // It cannot use `profile: codex_55_review` any more: since #894 S2d a read-only
    // profile cannot be dispatched over acpx *at all* — acpx has no sandbox
    // primitive, codex runs shell unattended, and a read-only claim nobody enforces
    // is refused pre-spawn (authority invariant 4). Certifying codex/cli does not
    // change that: the receipt certifies the codex **CLI** sandbox, and over acpx
    // the `--sandbox` flag never reaches the child. That refusal is pinned in
    // `authority::tests::derived_read_only_on_an_uncertified_provider_is_refused_not_downgraded`,
    // and the profile→raven derivation is pinned in
    // `dispatch_ops::acpx::tests::acpx_session_derives_raven_from_a_review_dispatch_profile`
    // (added with this change, so that path keeps its coverage). Neither of those
    // is this test's job — but the raven assertions below are, and they are
    // unchanged.
    params.stage = Some("review".to_string());
    params.timeout_secs = 5;

    let response = crate::dispatch_ops::handle_tachi_dispatch(&server, params)
        .await
        .expect("acpx session dispatch should start");
    let parsed: Value = serde_json::from_str(&response).expect("dispatch JSON");
    assert_eq!(parsed["execution_backend"], json!("acpx"));
    assert_eq!(parsed["acpx"]["mode"], json!("session"));
    assert_eq!(parsed["acpx"]["session"], json!("raven"));
    assert_eq!(
        parsed["acpx"]["controls"]["status"]["supported"],
        json!(true)
    );

    let dispatch_id = parsed["dispatch_id"].as_str().expect("dispatch id");
    let run_dir = temp_home.path().join("runs").join(dispatch_id);
    let result = wait_for_dispatch_result(&run_dir).await;
    assert_eq!(result, "raven done");
    let status = wait_for_dispatch_status(&run_dir).await;
    assert_eq!(status["acpx"]["mode"], json!("session"));
    assert_eq!(status["acpx"]["session"], json!("raven"));
    assert_eq!(
        status["acpx"]["controls"]["cancel"]["supported"],
        json!(true)
    );
}
