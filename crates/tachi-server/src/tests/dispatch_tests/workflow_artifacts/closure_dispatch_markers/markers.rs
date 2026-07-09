use super::*;

#[test]
#[allow(clippy::await_holding_lock)]
fn tachi_task_dispatch_marker_updates_flow_status_idempotently() {
    let _lock = crate::shell_ops::tachi_run_root_env_lock()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let temp_home = tempfile::tempdir().expect("temp tachi home");
    let _tachi_home = EnvVarGuard::set_path("TACHI_HOME", temp_home.path());
    let flow_id = "flow_20260608T000007Z_dispatch_marker_test";
    let dispatch_id = "20260608T000007Z-custom-marker";

    crate::task_lifecycle::mark_task_dispatch(
        flow_id,
        dispatch_id,
        json!({
            "agent": "custom",
            "profile": "deepseek_explore",
            "task": "read-only review",
        }),
    )
    .expect("mark dispatch");
    crate::task_lifecycle::mark_task_dispatch(
        flow_id,
        dispatch_id,
        json!({
            "agent": "custom",
            "profile": "deepseek_explore",
            "task": "read-only review",
        }),
    )
    .expect("mark dispatch idempotently");
    assert!(
        crate::task_lifecycle::mark_task_dispatch(flow_id, "../bad", json!({})).is_err(),
        "dispatch marker ids must stay filename-safe"
    );

    let run_dir = crate::shell_ops::run_dir_for_flow_id(flow_id).expect("flow run dir");
    let status: Value = serde_json::from_str(
        &std::fs::read_to_string(run_dir.join("status.json")).expect("status"),
    )
    .expect("status JSON");
    assert_eq!(status["dispatch_ids"], json!([dispatch_id]));
    assert_eq!(status["stage"], json!("dispatch"));
    assert_eq!(status["state"], json!("dispatched"));
    let card_path = status["artifacts"]["dispatches"][dispatch_id]
        .as_str()
        .expect("dispatch card path");
    assert!(
        std::path::Path::new(card_path).exists(),
        "dispatch card should exist: {status:#}"
    );
    assert_eq!(
        status["dispatch_cards"].as_array().map(Vec::len),
        Some(1),
        "dispatch card list should not duplicate entries: {status:#}"
    );
    let events = std::fs::read_to_string(run_dir.join("events.jsonl")).expect("events");
    assert!(events.contains("\"event\":\"dispatch_linked\""), "{events}");
}

#[test]
#[allow(clippy::await_holding_lock)]
fn tachi_task_dispatch_completion_marker_updates_card_and_status_idempotently() {
    let _lock = crate::shell_ops::tachi_run_root_env_lock()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let temp_home = tempfile::tempdir().expect("temp tachi home");
    let _tachi_home = EnvVarGuard::set_path("TACHI_HOME", temp_home.path());
    let flow_id = "flow_20260609T000001Z_dispatch_completion_marker_test";
    let dispatch_id = "20260609T000001Z-custom-complete";

    crate::task_lifecycle::mark_task_dispatch(
        flow_id,
        dispatch_id,
        json!({
            "agent": "custom",
            "profile": "glm_impl",
            "task": "implementation",
        }),
    )
    .expect("mark dispatch");
    crate::task_lifecycle::mark_task_dispatch_completion(
        flow_id,
        dispatch_id,
        json!({
            "task_id": "eval-link-001",
            "outcome": "success",
            "eval_memory_id": "memory-eval-001",
            "eval_path": "/eval/2026-06-09/eval-link-001",
            "verification_present": true,
            "tests_run": ["cargo test -p tachi-server dispatch_tests"],
        }),
    )
    .expect("mark completion");
    crate::task_lifecycle::mark_task_dispatch_completion(
        flow_id,
        dispatch_id,
        json!({
            "task_id": "eval-link-001",
            "outcome": "success",
            "eval_memory_id": "memory-eval-001",
            "eval_path": "/eval/2026-06-09/eval-link-001",
            "verification_present": true,
        }),
    )
    .expect("mark completion idempotently");

    let run_dir = crate::shell_ops::run_dir_for_flow_id(flow_id).expect("flow run dir");
    let status: Value = serde_json::from_str(
        &std::fs::read_to_string(run_dir.join("status.json")).expect("status"),
    )
    .expect("status JSON");
    assert_eq!(status["completed_dispatch_ids"], json!([dispatch_id]));
    assert_eq!(status["stage"], json!("eval"));
    assert_eq!(status["state"], json!("dispatch_completed"));
    assert_eq!(
        status["dispatch_eval"][dispatch_id]["eval_memory_id"],
        json!("memory-eval-001")
    );
    assert_eq!(
        status["artifacts"]["dispatch_completions"][dispatch_id]["outcome"],
        json!("success")
    );
    let card_path = status["artifacts"]["dispatches"][dispatch_id]
        .as_str()
        .expect("dispatch card path");
    let card: Value = serde_json::from_str(&std::fs::read_to_string(card_path).expect("card"))
        .expect("card JSON");
    assert_eq!(
        card["completion"]["eval_path"],
        json!("/eval/2026-06-09/eval-link-001")
    );
    assert_eq!(
        card["completion_history"].as_array().map(Vec::len),
        Some(1),
        "same eval should not duplicate completion history: {card:#}"
    );
    let events = std::fs::read_to_string(run_dir.join("events.jsonl")).expect("events");
    assert!(
        events.contains("\"event\":\"dispatch_completed\""),
        "{events}"
    );
}
