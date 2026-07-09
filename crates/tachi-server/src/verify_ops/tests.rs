use super::*;
use crate::tool_params::TachiVerifyParams;

fn params(action: &str) -> TachiVerifyParams {
    TachiVerifyParams {
        action: action.to_string(),
        format: Some("json".to_string()),
        flow_id: Some("flow_test-verify".to_string()),
        pr_ref: None,
        head_sha: None,
        check_id: None,
        kind: None,
        command: None,
        commands: vec![],
        status: None,
        exit_code: None,
        log_path: None,
        summary: None,
        cwd: None,
        required: None,
        limit: None,
        checks: vec![],
    }
}

#[test]
fn record_items_upserts_and_computes_overall() {
    let _guard = crate::shell_ops::tachi_run_root_env_lock()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let tmp = tempfile::tempdir().unwrap();
    let original = std::env::var_os("TACHI_RUN_ROOT");
    std::env::set_var("TACHI_RUN_ROOT", tmp.path());

    let mut first = params("record");
    first.kind = Some("gitleaks".to_string());
    first.head_sha = Some("abc".to_string());
    first.status = Some("passed".to_string());
    record_items(&first, "passed").unwrap();

    let mut second = params("record");
    second.kind = Some("clippy".to_string());
    second.head_sha = Some("abc".to_string());
    second.status = Some("failed".to_string());
    let out = record_items(&second, "failed").unwrap();

    assert_eq!(out["verification"]["overall"], "failed");
    assert_eq!(out["verification"]["items"].as_array().unwrap().len(), 2);
    if let Some(v) = original {
        std::env::set_var("TACHI_RUN_ROOT", v);
    } else {
        std::env::remove_var("TACHI_RUN_ROOT");
    }
}

#[test]
fn verification_gate_detects_failed_pending_and_stale_items() {
    let _guard = crate::shell_ops::tachi_run_root_env_lock()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let tmp = tempfile::tempdir().unwrap();
    let original = std::env::var_os("TACHI_RUN_ROOT");
    std::env::set_var("TACHI_RUN_ROOT", tmp.path());

    let flow_id = "flow_test-verify";
    let path = ledger_path_for_flow(flow_id).unwrap();
    write_json(
        &path,
        &json!({
            "flow_id": flow_id,
            "overall": "failed",
            "items": [
                {"id":"gitleaks","status":"passed","head_sha":"abc","required":true},
                {"id":"clippy","status":"failed","head_sha":"abc","required":true},
                {"id":"test","status":"running","head_sha":"abc","required":true},
                {"id":"check","status":"passed","head_sha":"old","required":true}
            ]
        }),
    )
    .unwrap();

    let gate = evaluate_verification_gate(Some(flow_id), "abc")
        .unwrap()
        .unwrap();
    assert_eq!(gate["overall"], "failed");
    assert!(gate["reasons"]
        .as_array()
        .unwrap()
        .iter()
        .any(|r| r == "verification:clippy:failed"));
    assert!(gate["waiting_on"]
        .as_array()
        .unwrap()
        .iter()
        .any(|r| r == "verification:test:pending"));
    assert!(gate["waiting_on"]
        .as_array()
        .unwrap()
        .iter()
        .any(|r| r == "verification:check:stale"));
    if let Some(v) = original {
        std::env::set_var("TACHI_RUN_ROOT", v);
    } else {
        std::env::remove_var("TACHI_RUN_ROOT");
    }
}

#[test]
fn verification_gate_treats_missing_head_sha_as_stale() {
    let _guard = crate::shell_ops::tachi_run_root_env_lock()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let tmp = tempfile::tempdir().unwrap();
    let original = std::env::var_os("TACHI_RUN_ROOT");
    std::env::set_var("TACHI_RUN_ROOT", tmp.path());

    let flow_id = "flow_test-verify";
    let path = ledger_path_for_flow(flow_id).unwrap();
    write_json(
        &path,
        &json!({
            "flow_id": flow_id,
            "overall": "passed",
            "items": [
                {"id":"gitleaks","status":"passed","required":true}
            ]
        }),
    )
    .unwrap();

    let gate = evaluate_verification_gate(Some(flow_id), "abc")
        .unwrap()
        .unwrap();
    assert_eq!(gate["overall"], "pending");
    assert!(gate["waiting_on"]
        .as_array()
        .unwrap()
        .iter()
        .any(|r| r == "verification:gitleaks:stale"));
    if let Some(v) = original {
        std::env::set_var("TACHI_RUN_ROOT", v);
    } else {
        std::env::remove_var("TACHI_RUN_ROOT");
    }
}

#[test]
fn verification_gate_treats_skipped_required_without_head_sha_as_stale() {
    let _guard = crate::shell_ops::tachi_run_root_env_lock()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let tmp = tempfile::tempdir().unwrap();
    let original = std::env::var_os("TACHI_RUN_ROOT");
    std::env::set_var("TACHI_RUN_ROOT", tmp.path());

    let flow_id = "flow_test-verify";
    let path = ledger_path_for_flow(flow_id).unwrap();
    write_json(
        &path,
        &json!({
            "flow_id": flow_id,
            "overall": "passed",
            "items": [
                {"id":"gitleaks","status":"skipped","required":true}
            ]
        }),
    )
    .unwrap();

    let gate = evaluate_verification_gate(Some(flow_id), "abc")
        .unwrap()
        .unwrap();
    assert_eq!(gate["overall"], "pending");
    assert!(gate["waiting_on"]
        .as_array()
        .unwrap()
        .iter()
        .any(|r| r == "verification:gitleaks:stale"));
    if let Some(v) = original {
        std::env::set_var("TACHI_RUN_ROOT", v);
    } else {
        std::env::remove_var("TACHI_RUN_ROOT");
    }
}
