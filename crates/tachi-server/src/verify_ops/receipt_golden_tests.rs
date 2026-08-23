use super::*;
use crate::tool_params::{TachiVerifyCheckItem, TachiVerifyParams};
use crate::MemoryServer;
fn test_server() -> MemoryServer {
    let db = crate::utils::test_fixture_path(format!(
        "verify-receipt-test-{}.sqlite",
        uuid::Uuid::new_v4()
    ));
    MemoryServer::new(db, None).expect("test server")
}

fn verify_params(action: &str) -> TachiVerifyParams {
    TachiVerifyParams {
        action: action.parse().expect("valid tachi_verify action"),
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
        check_kind: None,
        timeout_secs: None,
    }
}

#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn g1_record_single_check_receipt_ignores_other_checks() {
    let _guard = crate::utils::global_test_lock()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let tmp = tempfile::tempdir().unwrap();
    let original = std::env::var_os("TACHI_RUN_ROOT");
    std::env::set_var("TACHI_RUN_ROOT", tmp.path());

    let server = test_server();

    let mut seed = verify_params("record");
    seed.kind = Some("gitleaks".to_string());
    seed.status = Some("passed".to_string());
    handle_tachi_verify(&server, seed)
        .await
        .expect("seed check");

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
#[allow(clippy::await_holding_lock)]
async fn g2_batch_record_receipt_lists_both_ids() {
    let _guard = crate::utils::global_test_lock()
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

    // F1: default status is compact (problems only); full board via format=full.
    let mut status_params = verify_params("status");
    status_params.format = Some("json".to_string());
    let status_resp = handle_tachi_verify(&server, status_params)
        .await
        .expect("status");
    let status: Value = serde_json::from_str(&status_resp).expect("status JSON");
    assert!(status.get("verification").is_none());
    // #1454 F6: the readiness verdict is the authority-aware gate result.
    // Caller-asserted records (no server-run receipts, no server-known head)
    // display fail-closed as `unverified` — never the ledger's "failed".
    assert_eq!(status["overall"], json!("unverified"));
    assert_eq!(status["ledger_overall"], json!("failed"));
    assert!(status["problems"]
        .as_array()
        .unwrap()
        .iter()
        .any(|p| { p.get("id").and_then(Value::as_str) == Some("clippy") }));

    let mut full = verify_params("status");
    full.format = Some("full".to_string());
    let full_resp = handle_tachi_verify(&server, full)
        .await
        .expect("full status");
    let full_status: Value = serde_json::from_str(&full_resp).expect("full JSON");
    let ids: Vec<String> = full_status["verification"]["items"]
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

#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn g2b_legacy_commands_receipt_lists_both_derived_ids() {
    let _guard = crate::utils::global_test_lock()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let tmp = tempfile::tempdir().unwrap();
    let original = std::env::var_os("TACHI_RUN_ROOT");
    std::env::set_var("TACHI_RUN_ROOT", tmp.path());

    let server = test_server();
    let mut params = verify_params("record");
    params.commands = vec!["cargo test".to_string(), "cargo clippy".to_string()];
    params.status = Some("passed".to_string());
    let resp = handle_tachi_verify(&server, params)
        .await
        .expect("legacy commands record");
    let receipt: Value = serde_json::from_str(&resp).expect("receipt JSON");
    assert_eq!(receipt["check_ids"], json!(["cargo-test", "cargo-clippy"]));
    assert_eq!(receipt["overall"], json!("passed"));

    if let Some(v) = original {
        std::env::set_var("TACHI_RUN_ROOT", v);
    } else {
        std::env::remove_var("TACHI_RUN_ROOT");
    }
}

#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn f1_status_compact_omits_passed_rows_and_stays_small() {
    let _guard = crate::utils::global_test_lock()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let tmp = tempfile::tempdir().unwrap();
    let original = std::env::var_os("TACHI_RUN_ROOT");
    std::env::set_var("TACHI_RUN_ROOT", tmp.path());

    let server = test_server();
    // Seed a passed check then a failed one; compact status must not re-echo gitleaks.
    let mut seed = verify_params("record");
    seed.kind = Some("gitleaks".to_string());
    seed.status = Some("passed".to_string());
    seed.summary = Some("ok".to_string());
    handle_tachi_verify(&server, seed).await.expect("seed");

    let mut fail = verify_params("record");
    fail.kind = Some("clippy".to_string());
    fail.status = Some("failed".to_string());
    fail.summary = Some("lint".to_string());
    handle_tachi_verify(&server, fail).await.expect("fail");

    let mut status = verify_params("status");
    status.format = Some("json".to_string());
    let resp = handle_tachi_verify(&server, status)
        .await
        .expect("compact status");
    assert!(
        resp.len() < 600,
        "compact status too large: {} bytes: {resp}",
        resp.len()
    );
    let lower = resp.to_ascii_lowercase();
    assert!(
        !lower.contains("gitleaks"),
        "passed check must not be echoed: {resp}"
    );
    assert!(lower.contains("clippy"));
    assert!(lower.contains("problems"));

    if let Some(v) = original {
        std::env::set_var("TACHI_RUN_ROOT", v);
    } else {
        std::env::remove_var("TACHI_RUN_ROOT");
    }
}

#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn g5_record_receipt_default_under_400_bytes() {
    let _guard = crate::utils::global_test_lock()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let tmp = tempfile::tempdir().unwrap();
    let original = std::env::var_os("TACHI_RUN_ROOT");
    std::env::set_var("TACHI_RUN_ROOT", tmp.path());

    let server = test_server();
    let mut params = verify_params("record");
    params.kind = Some("clippy".to_string());
    params.status = Some("failed".to_string());
    let resp = handle_tachi_verify(&server, params)
        .await
        .expect("record receipt");
    assert!(
        resp.len() < 400,
        "record receipt too large: {} bytes: {resp}",
        resp.len()
    );

    if let Some(v) = original {
        std::env::set_var("TACHI_RUN_ROOT", v);
    } else {
        std::env::remove_var("TACHI_RUN_ROOT");
    }
}

fn strip_volatile_ledger_fields(mut ledger: Value) -> Value {
    if let Some(obj) = ledger.as_object_mut() {
        obj.remove("updated_at");
        if let Some(items) = obj.get_mut("items").and_then(Value::as_array_mut) {
            for item in items.iter_mut() {
                if let Some(item_obj) = item.as_object_mut() {
                    item_obj.remove("updated_at");
                }
            }
        }
    }
    ledger
}

#[test]
fn g6_verification_json_matches_expected_structure() {
    let _guard = crate::utils::global_test_lock()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let tmp = tempfile::tempdir().unwrap();
    let original = std::env::var_os("TACHI_RUN_ROOT");
    std::env::set_var("TACHI_RUN_ROOT", tmp.path());

    let flow_id = "flow_g528-verify";
    let mut first = verify_params("record");
    first.kind = Some("gitleaks".to_string());
    first.status = Some("passed".to_string());
    first.head_sha = Some("abc123".to_string());
    record_items(&first, "passed").expect("record");

    let path = ledger_path_for_flow(flow_id).expect("ledger path");
    let on_disk: Value =
        serde_json::from_str(&std::fs::read_to_string(path).expect("read ledger")).expect("ledger");
    let expected = json!({
        "flow_id": flow_id,
        "head_sha": "abc123",
        "overall": "passed",
        "items": [{
            "id": "gitleaks",
            "kind": "gitleaks",
            "status": "passed",
            "required": true,
            "head_sha": "abc123",
            "source": "caller_asserted",
        }],
    });
    assert_eq!(
        strip_volatile_ledger_fields(on_disk),
        strip_volatile_ledger_fields(expected)
    );

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
