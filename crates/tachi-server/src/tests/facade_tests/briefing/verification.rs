use super::*;

#[allow(clippy::await_holding_lock)]
#[tokio::test]
async fn tachi_memory_briefing_includes_recent_verification_gates() {
    let _guard = crate::utils::global_test_lock()
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
    // #1454 F6-adjudication: the board row leads with the gate verdict
    // (`unverified` — no server-known receipt-store head) and appends the
    // caller-asserted marker so a seeded `failed` ledger stays visibly
    // failed. Asserting both preserves this test's original guardian intent
    // (a failed ledger is visible on the briefing) under the F6 authority
    // contract (verdict from the gate, never caller prose).
    // #1454 O2 re-anchor: the caller-authored flow_id renders through the
    // shared `markup_text` helper, so its underscore appears escaped.
    assert!(body.contains("[unverified (caller-asserted: failed)] `flow\\_briefing-verification`"));
    assert!(body.contains("`kckylechen1/tachi#209`"));
    assert!(body.contains("tachi_verify(action='board')"));
    if let Some(original) = original {
        std::env::set_var("TACHI_RUN_ROOT", original);
    } else {
        std::env::remove_var("TACHI_RUN_ROOT");
    }
}
