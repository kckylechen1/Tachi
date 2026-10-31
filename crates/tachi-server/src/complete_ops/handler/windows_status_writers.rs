use super::*;

#[test]
fn completion_receipt_writers_refuse_an_invalid_private_fence_without_mutation() {
    let temp = tempfile::tempdir().unwrap();
    let run = temp.path();
    let dispatch_id = "20261030T000001Z-windows-receipt";
    std::fs::write(
        run.join("status.json"),
        serde_json::to_vec(&json!({
            "dispatch_id": dispatch_id,
            "state": "TASK_STATE_WORKING",
            "status_revision": 0,
            "project": "preserved",
        }))
        .unwrap(),
    )
    .unwrap();
    persist_resolved_completion_receipt_at(
        run,
        dispatch_id,
        "TASK_STATE_COMPLETED",
        "eval-first",
        false,
    )
    .unwrap();
    let before = std::fs::read(run.join("status.json")).unwrap();
    let status: Value = serde_json::from_slice(&before).unwrap();
    assert_eq!(
        status["resolved_completion"]["eval_ledger_id"],
        "eval-first"
    );
    assert_eq!(status["project"], "preserved");
    assert_eq!(status["status_revision"], 1);
    std::fs::hard_link(
        run.join(".status-mutation.lock"),
        run.join("invalid-fence-link"),
    )
    .unwrap();
    assert!(persist_resolved_completion_receipt_at(
        run,
        dispatch_id,
        "TASK_STATE_COMPLETED",
        "eval-second",
        true,
    )
    .is_err());
    assert!(persist_pending_completion_recovery_receipt_for_test(
        run,
        dispatch_id,
        "TASK_STATE_COMPLETED",
        "eval-recovery",
        true,
        &json!({"outcome":"success"}),
    )
    .is_err());
    assert_eq!(std::fs::read(run.join("status.json")).unwrap(), before);
}
