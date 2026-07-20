//! End-to-end discrimination tests for the retained `tachi_shell` surface.

use super::{make_server, shell_params};
use crate::shell_ops::{handle_tachi_shell, shell_runs_root};
use serde_json::Value;

fn unique_flow_id(tag: &str) -> String {
    format!("flow_test_{}_{}", tag, uuid::Uuid::new_v4().simple())
}

fn read_events(run_dir: &std::path::Path) -> Vec<Value> {
    std::fs::read_to_string(run_dir.join("events.jsonl"))
        .unwrap_or_default()
        .lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| serde_json::from_str(line).expect("valid event JSON"))
        .collect()
}

struct Cleanup(std::path::PathBuf);

impl Drop for Cleanup {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn removed_shell_actions_reject_without_artifacts() {
    let _lock = crate::shell_ops::tachi_run_root_env_lock()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let server = make_server();

    for action in ["brainstorm", "plan", "review", "ship"] {
        let flow_id = unique_flow_id(action);
        let run_dir = shell_runs_root().join(&flow_id);
        let _cleanup = Cleanup(run_dir.clone());
        let mut params = shell_params(action);
        params.flow_id = Some(flow_id);
        params.task = Some(format!("removed action {action}"));

        let error = handle_tachi_shell(&server, params)
            .await
            .expect_err("removed action must reject");
        assert_eq!(
            error,
            format!("Invalid action '{action}'. Use 'dispatch' or 'status'.")
        );
        assert!(!run_dir.exists(), "{action} must create zero artifacts");
    }
}

#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn non_async_dispatch_writes_packet_status_and_event_without_worker() {
    let _lock = crate::shell_ops::tachi_run_root_env_lock()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let server = make_server();
    let flow_id = unique_flow_id("dispatch");
    let run_dir = shell_runs_root().join(&flow_id);
    let _cleanup = Cleanup(run_dir.clone());
    let mut params = shell_params("dispatch");
    params.flow_id = Some(flow_id.clone());
    params.task = Some("prepare bounded packet".to_string());
    params.notes = Some("acceptance evidence".to_string());
    params.validation = vec!["cargo test -p tachi-server".to_string()];
    params.allowed_scope = vec!["crates/tachi-server/**".to_string()];

    let raw = handle_tachi_shell(&server, params)
        .await
        .expect("non-async dispatch succeeds");
    let response: Value = serde_json::from_str(&raw).expect("dispatch response JSON");
    assert_eq!(response["flow_id"], flow_id);
    assert_eq!(response["stage"], "dispatch");
    assert_eq!(response["async"], false);
    assert!(response["dispatch_id"].is_null());
    let injection = &response["injected_skill"];
    assert_eq!(injection["required"], true);
    assert_eq!(
        injection["rel_path"],
        "skill/superpowers/skills/executing-plans/SKILL.md"
    );
    if injection["loaded"] == true {
        assert!(injection["source_path"].is_string());
        assert!(injection["injected_path"].is_string());
        assert!(injection["warning"].is_null());
        assert!(injection["failure_class"].is_null());
    } else {
        assert!(injection["warning"].is_string());
        assert!(injection["failure_class"].is_string());
    }
    assert!(run_dir.join("instruction.md").exists());

    let status: Value = serde_json::from_str(
        &std::fs::read_to_string(run_dir.join("status.json")).expect("status artifact"),
    )
    .expect("status JSON");
    assert_eq!(status["stage"], "dispatch");
    assert_eq!(status["state"], "dispatch_ready");
    assert_eq!(status["dispatch_ids"], serde_json::json!([]));
    let events = read_events(&run_dir);
    assert_eq!(events.len(), 1);
    assert_eq!(events[0]["event"], "flow_created");
    assert_eq!(events[0]["stage"], "dispatch");
}

#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn status_is_read_only_for_existing_and_missing_flows() {
    let _lock = crate::shell_ops::tachi_run_root_env_lock()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let server = make_server();
    let flow_id = unique_flow_id("status");
    let run_dir = shell_runs_root().join(&flow_id);
    let _cleanup = Cleanup(run_dir.clone());

    let mut dispatch = shell_params("dispatch");
    dispatch.flow_id = Some(flow_id.clone());
    dispatch.task = Some("status readback".to_string());
    handle_tachi_shell(&server, dispatch)
        .await
        .expect("seed dispatch packet");
    let events_before = read_events(&run_dir);

    let mut status = shell_params("status");
    status.flow_id = Some(flow_id.clone());
    let raw = handle_tachi_shell(&server, status)
        .await
        .expect("existing status lookup");
    let response: Value = serde_json::from_str(&raw).expect("status response JSON");
    assert_eq!(response["found"], true);
    assert_eq!(response["status"]["state"], "dispatch_ready");
    assert_eq!(read_events(&run_dir), events_before);

    let missing_id = unique_flow_id("missing");
    let missing_dir = shell_runs_root().join(&missing_id);
    let mut missing = shell_params("status");
    missing.flow_id = Some(missing_id);
    let raw = handle_tachi_shell(&server, missing)
        .await
        .expect("missing status lookup");
    let response: Value = serde_json::from_str(&raw).expect("missing response JSON");
    assert_eq!(response["found"], false);
    assert!(!missing_dir.exists(), "missing lookup must create nothing");
}
