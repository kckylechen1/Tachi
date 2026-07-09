use super::*;

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
