//! Receipt/board publication precedes worker completion. Retired planner
//! requests fail before artifacts, leases, routing evidence, or worker launch.

use super::super::make_server;
use super::{dispatch_params, wait_for_dispatch_result, wait_for_dispatch_status};
use crate::test_support::EnvRestore;
use serde_json::{json, Value};

fn side_effect_counts(server: &crate::MemoryServer) -> Vec<i64> {
    server
        .with_global_store_read(|store| {
            ["route_decisions", "exec_envs", "session_claims"]
                .iter()
                .map(|table| {
                    store
                        .connection()
                        .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |row| {
                            row.get(0)
                        })
                        .map_err(|error| error.to_string())
                })
                .collect()
        })
        .expect("read dispatch side-effect counts")
}

#[allow(clippy::await_holding_lock)]
#[tokio::test]
async fn retired_planning_requests_never_create_runs_or_launch_workers() {
    let _guard = crate::utils::global_test_lock()
        .lock()
        .unwrap_or_else(|p| p.into_inner());
    let temp_home = tempfile::tempdir().expect("temp home");
    let _home = EnvRestore::set_path("TACHI_HOME", temp_home.path());
    let _enabled = EnvRestore::set("DISPATCH_V2_ENABLED", "false");
    let _review = EnvRestore::set("DISPATCH_V2_PLAN_REVIEW", "false");
    let server = make_server();
    let before = side_effect_counts(&server);
    let marker = temp_home.path().join("worker-started");
    for (stage, setting) in [
        (Some("auto"), None),
        (Some("plan_execute"), None),
        (Some("execute"), Some("DISPATCH_V2_ENABLED")),
        (Some("execute"), Some("DISPATCH_V2_PLAN_REVIEW")),
    ] {
        let _setting = setting.map(|key| EnvRestore::set(key, "true"));
        let mut params = dispatch_params(Some("custom"), "do not silently bypass host review");
        params.stage = stage.map(str::to_string);
        params.command = vec![
            "python3".into(),
            "-c".into(),
            "import pathlib,sys; pathlib.Path(sys.argv[1]).write_text('started')".into(),
            marker.to_string_lossy().into_owned(),
        ];
        let error = crate::dispatch_ops::handle_tachi_dispatch(&server, params)
            .await
            .expect_err("retired planner request must be rejected");
        assert!(error.contains("retired"), "{error}");
        assert!(error.contains("No worker was started"), "{error}");
    }
    assert!(
        !marker.exists(),
        "a rejected planning request must not execute its command"
    );
    assert_eq!(side_effect_counts(&server), before);
    let runs = crate::dispatch_ops::dispatch_runs_root();
    assert_eq!(
        std::fs::read_dir(runs)
            .map(|entries| entries.count())
            .unwrap_or(0),
        0
    );
}

#[allow(clippy::await_holding_lock)]
#[tokio::test]
async fn staff_planner_refusal_precedes_environment_provisioning() {
    let _guard = crate::utils::global_test_lock()
        .lock()
        .unwrap_or_else(|p| p.into_inner());
    let temp_home = tempfile::tempdir().expect("temp home");
    let _home = EnvRestore::set_path("TACHI_HOME", temp_home.path());
    let _enabled = EnvRestore::set("DISPATCH_V2_ENABLED", "false");
    let _review = EnvRestore::set("DISPATCH_V2_PLAN_REVIEW", "false");
    let (server, _project) = crate::tests::make_server_with_project_fixture("planner-refusal");
    server.set_session_identity(None, Some("planner-refusal".into()), None);
    let before = side_effect_counts(&server);
    for (stage, setting) in [
        (Some("auto"), None),
        (Some("execute"), Some("DISPATCH_V2_PLAN_REVIEW")),
    ] {
        let _setting = setting.map(|key| EnvRestore::set(key, "true"));
        let mut request = tachi_params::StaffAssignmentRequest::new(
            tachi_params::TachiDispatchReason::ExplicitUserRequest,
            "host review is required before any worker starts",
        );
        request.profile = Some("dsh_executor".into());
        request.stage = stage.map(str::to_string);
        let error = crate::staffing_ops::staff_start(&server, request)
            .await
            .expect_err("retired planner must refuse before provision");
        assert!(error.contains("retired"), "{error}");
    }
    assert_eq!(side_effect_counts(&server), before);
    let runs = crate::dispatch_ops::dispatch_runs_root();
    assert_eq!(
        std::fs::read_dir(runs)
            .map(|entries| entries.count())
            .unwrap_or(0),
        0
    );
}

#[allow(clippy::await_holding_lock)]
#[tokio::test]
async fn host_plan_reaches_worker_after_receipt_and_board_publication() {
    let _guard = crate::utils::global_test_lock()
        .lock()
        .unwrap_or_else(|p| p.into_inner());
    let temp_home = tempfile::tempdir().expect("temp home");
    let _home = EnvRestore::set_path("TACHI_HOME", temp_home.path());
    let _enabled = EnvRestore::set("DISPATCH_V2_ENABLED", "false");
    let _review = EnvRestore::set("DISPATCH_V2_PLAN_REVIEW", "false");
    let release = temp_home.path().join("release-worker");
    let server = make_server();
    let host_plan =
        "Host-approved plan: inspect only the supplied fixture and report HOST_PLAN_PRESERVED.";
    let mut params = dispatch_params(Some("custom"), host_plan);
    params.stage = Some("execute".into());
    params.timeout_secs = 30;
    params.command = vec![
        "python3".into(), "-c".into(),
        "import pathlib,sys,time; release=pathlib.Path(sys.argv[1]); deadline=time.monotonic()+20\nwhile not release.exists() and time.monotonic()<deadline: time.sleep(0.02)\nassert release.exists(), 'worker release timed out'\nprint(sys.argv[-1])".into(),
        release.to_string_lossy().into_owned(),
    ];
    let raw = crate::dispatch_ops::handle_tachi_dispatch(&server, params)
        .await
        .expect("start worker");
    let response: Value = serde_json::from_str(&raw).expect("dispatch response");
    assert_eq!(response["v2"], false);
    let run_dir = std::path::Path::new(response["run_dir"].as_str().expect("run dir"));
    let dispatch_id = response["dispatch_id"].as_str().expect("dispatch id");
    let status: Value =
        serde_json::from_slice(&std::fs::read(run_dir.join("status.json")).expect("receipt"))
            .expect("status JSON");
    assert_eq!(status["task"], host_plan);
    assert_eq!(status["plan_review_status"], "n/a");
    assert!(status["exit_code"].is_null());
    assert!(status.get("model_plan").is_none());
    assert_eq!(
        crate::dispatch_ops::get_kanban_state(&server, dispatch_id)
            .await
            .as_deref(),
        Some("TASK_STATE_WORKING")
    );
    let trajectory = std::fs::read_to_string(run_dir.join("trajectory.jsonl")).expect("trajectory");
    let events: Vec<Value> = trajectory
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert_eq!(events[0]["event"], json!("dispatch_received"));
    assert!(events
        .iter()
        .any(|event| event["event"] == "dispatch_started"));
    assert!(!events
        .iter()
        .any(|event| event["event"] == "plan_generated"));
    assert!(std::fs::read_to_string(run_dir.join("prompt.md"))
        .unwrap()
        .contains(host_plan));
    std::fs::write(&release, b"go").expect("release worker");
    let result = wait_for_dispatch_result(run_dir).await;
    assert!(result.contains("HOST_PLAN_PRESERVED"), "{result}");
    let completed = wait_for_dispatch_status(run_dir).await;
    assert_eq!(completed["v2"], false);
    assert!(completed.get("model_plan").is_none());
}
