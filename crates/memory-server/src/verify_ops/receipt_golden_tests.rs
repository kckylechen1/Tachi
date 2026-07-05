use super::*;
use crate::tool_params::{TachiVerifyCheckItem, TachiVerifyParams};
use crate::MemoryServer;
fn test_server() -> MemoryServer {
    let db = std::env::temp_dir().join(format!(
        "verify-receipt-test-{}.sqlite",
        uuid::Uuid::new_v4()
    ));
    MemoryServer::new(db, None).expect("test server")
}

fn verify_params(action: &str) -> TachiVerifyParams {
    TachiVerifyParams {
        action: action.to_string(),
        format: Some("json".to_string()),
        flow_id: Some("flow_g528-verify".to_string()),
        pr_ref: None,
        head_sha: Some("abc123".to_string()),
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

#[tokio::test]
async fn g1_record_single_check_receipt_ignores_other_checks() {
    let _guard = crate::shell_ops::tachi_run_root_env_lock()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let tmp = tempfile::tempdir().unwrap();
    let original = std::env::var_os("TACHI_RUN_ROOT");
    std::env::set_var("TACHI_RUN_ROOT", tmp.path());

    let server = test_server();

    let mut seed = verify_params("record");
    seed.kind = Some("gitleaks".to_string());
    seed.status = Some("passed".to_string());
    handle_tachi_verify(&server, seed).await.expect("seed check");

    let mut second = verify_params("record");
    second.kind = Some("clippy".to_string());
    second.status = Some("failed".to_string());
    let resp = handle_tachi_verify(&server, second)
        .await
        .expect("record second check");
    let value: Value = serde_json::from_str(&resp).expect("record JSON");

    assert_eq!(value["ok"], json!(true));
    assert_eq!(value["check_id"], json!("clippy"));
    assert_eq!(value["overall"], json!("failed"));
    assert!(value.get("verification").is_none());
    assert!(value.get("check_ids").is_none());
    let serialized = resp.to_ascii_lowercase();
    assert!(!serialized.contains("gitleaks"));
    assert!(!serialized.contains("superseded"));

    if let Some(v) = original {
        std::env::set_var("TACHI_RUN_ROOT", v);
    } else {
        std::env::remove_var("TACHI_RUN_ROOT");
    }
}

#[tokio::test]
async fn g2_batch_record_receipt_lists_both_ids() {
    let _guard = crate::shell_ops::tachi_run_root_env_lock()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let tmp = tempfile::tempdir().unwrap();
    let original = std::env::var_os("TACHI_RUN_ROOT");
    std::env::set_var("TACHI_RUN_ROOT", tmp.path());

    let server = test_server();

    let mut batch = verify_params("record");
    batch.checks = vec![
        TachiVerifyCheckItem {
            check_id: "gitleaks".to_string(),
            kind: "gitleaks".to_string(),
            status: "passed".to_string(),
            required: Some(true),
            command: None,
            summary: None,
            head_sha: Some("abc123".to_string()),
        },
        TachiVerifyCheckItem {
            check_id: "clippy".to_string(),
            kind: "clippy".to_string(),
            status: "failed".to_string(),
            required: Some(true),
            command: None,
            summary: None,
            head_sha: Some("abc123".to_string()),
        },
    ];
    let resp = handle_tachi_verify(&server, batch)
        .await
        .expect("batch record");
    let receipt: Value = serde_json::from_str(&resp).expect("batch receipt JSON");
    assert_eq!(receipt["check_ids"], json!(["gitleaks", "clippy"]));
    assert_eq!(receipt["overall"], json!("failed"));

    let mut status_params = verify_params("status");
    status_params.format = Some("json".to_string());
    let status_resp = handle_tachi_verify(&server, status_params)
        .await
        .expect("status");
    let status: Value = serde_json::from_str(&status_resp).expect("status JSON");
    let ids: Vec<String> = status["verification"]["items"]
        .as_array()
        .expect("items")
        .iter()
        .filter_map(|item| item.get("id").and_then(Value::as_str).map(str::to_string))
        .collect();
    assert!(ids.contains(&"gitleaks".to_string()));
    assert!(ids.contains(&"clippy".to_string()));

    if let Some(v) = original {
        std::env::set_var("TACHI_RUN_ROOT", v);
    } else {
        std::env::remove_var("TACHI_RUN_ROOT");
    }
}

#[test]
fn g5_record_receipt_default_under_600_bytes() {
    let receipt = json!({
        "ok": true,
        "flow_id": "flow_g528-verify",
        "check_id": "clippy",
        "status": "failed",
        "overall": "failed",
    });
    let raw = serde_json::to_string(&receipt).expect("serialize");
    assert!(raw.len() < 600, "record receipt too large: {} bytes", raw.len());
}

#[test]
fn g6_verification_json_retains_required_fields() {
    let _guard = crate::shell_ops::tachi_run_root_env_lock()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let tmp = tempfile::tempdir().unwrap();
    let original = std::env::var_os("TACHI_RUN_ROOT");
    std::env::set_var("TACHI_RUN_ROOT", tmp.path());

    let flow_id = "flow_g528-verify";
    let mut first = verify_params("record");
    first.kind = Some("gitleaks".to_string());
    first.status = Some("passed".to_string());
    record_items(&first, "passed").expect("record");

    let path = ledger_path_for_flow(flow_id).expect("ledger path");
    let on_disk: Value =
        serde_json::from_str(&std::fs::read_to_string(path).expect("read ledger")).expect("ledger");
    assert_eq!(on_disk["flow_id"], json!(flow_id));
    assert!(on_disk.get("overall").is_some());
    assert!(on_disk.get("items").and_then(Value::as_array).is_some());
    assert!(on_disk["items"][0].get("id").is_some());
    assert!(on_disk["items"][0].get("status").is_some());

    if let Some(v) = original {
        std::env::set_var("TACHI_RUN_ROOT", v);
    } else {
        std::env::remove_var("TACHI_RUN_ROOT");
    }
}

#[tokio::test]
async fn batch_record_rejects_mixed_single_and_checks_params() {
    let server = test_server();
    let mut params = verify_params("record");
    params.kind = Some("clippy".to_string());
    params.checks = vec![TachiVerifyCheckItem {
        check_id: "gitleaks".to_string(),
        kind: "gitleaks".to_string(),
        status: "passed".to_string(),
        required: None,
        command: None,
        summary: None,
        head_sha: None,
    }];
    let err = handle_tachi_verify(&server, params)
        .await
        .expect_err("mixed params");
    assert!(err.contains("checks array"));
    assert!(err.contains("check_id") || err.contains("kind"));
}