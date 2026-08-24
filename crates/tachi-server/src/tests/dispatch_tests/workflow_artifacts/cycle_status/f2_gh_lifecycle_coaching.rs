//! F2 (#495/#913) discrimination: agent-facing cycle_status coaching for
//! GitHub PR lifecycle must point at `tachi_gh`, never re-advertise the
//! deprecated `tachi_task(action=link_pr|pr_handoff|pr_status|release_note)`.

use super::*;

const DEPRECATED_LIFECYCLE_COACHING: &[&str] = &[
    "tachi_task(action='link_pr'",
    "tachi_task(action='pr_status'",
    "tachi_task(action='pr_handoff'",
    "tachi_task(action='release_note'",
    "tachi_task(action=\"link_pr\"",
    "tachi_task(action=\"pr_status\"",
    "tachi_task(action=\"pr_handoff\"",
    "tachi_task(action=\"release_note\"",
];

fn assert_no_deprecated_lifecycle_coaching(surface: &str, text: &str) {
    for needle in DEPRECATED_LIFECYCLE_COACHING {
        assert!(
            !text.contains(needle),
            "{surface} must not coach agents onto deprecated {needle}; use tachi_gh. got: {text}"
        );
    }
}

#[allow(clippy::await_holding_lock)]
#[tokio::test]
async fn f2_cycle_status_next_action_coaches_tachi_gh_for_release_note() {
    // Pre-fix RED: next_action for ready PR + passed verify without release_note
    // used to say tachi_task(action='release_note'...).
    let _lock = crate::utils::global_test_lock()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let temp_home = tempfile::tempdir().expect("temp tachi home");
    let temp_runs = tempfile::tempdir().expect("temp run root");
    let _home = EnvVarGuard::set_path("TACHI_HOME", temp_home.path());
    let _run_root = EnvVarGuard::set_path("TACHI_RUN_ROOT", temp_runs.path());
    let server = make_server();
    let flow_id = "flow_20260709T000001Z_f2_cycle_status_gh";
    let issue = issue_snapshot(
        918,
        vec!["docs/engineering/architecture/project-cycle-memory-spine.md"],
        vec!["docs/engineering/specs/project-cycle-read-model.md"],
    );
    write_intake_flow(flow_id, &issue);
    let pr = pr_snapshot(919);
    crate::task_lifecycle::write_link_pr_artifacts(flow_id, &pr, None)
        .expect("write link_pr artifacts");
    let run_dir = crate::task_lifecycle::run_dir_for_flow_id(flow_id).expect("run dir");
    crate::task_lifecycle::merge_github_status(
        &run_dir,
        json!({
            "merge_state": "ready",
            "head_sha": "abc123",
            "policy": "standard",
            "requested_mode": "preview"
        }),
    )
    .expect("merge github status");
    write_passed_verification(&server, flow_id, "abc123");

    let mut params = task_params("status");
    params.flow_id = Some(flow_id.to_string());
    let raw = server
        .tachi_task(Parameters(params))
        .await
        .expect("status cycle should succeed");
    let parsed = cycle_view(&raw);

    let next = parsed["next_action"].as_str().expect("next_action string");
    assert!(
        next.contains("tachi_gh(action='release_note'"),
        "ready PR without release_note must coach tachi_gh release_note, got: {next}"
    );
    assert_no_deprecated_lifecycle_coaching("status.cycle.next_action", next);

    let drift = serde_json::to_string(&parsed["spec_drift"]).expect("serialize drift");
    assert_no_deprecated_lifecycle_coaching("status.cycle.spec_drift", &drift);
    let full = serde_json::to_string(&parsed).expect("serialize full");
    assert_no_deprecated_lifecycle_coaching("status cycle full payload", &full);
}

#[allow(clippy::await_holding_lock)]
#[tokio::test]
async fn f2_cycle_status_next_action_coaches_tachi_gh_to_link_pr() {
    let _lock = crate::utils::global_test_lock()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let temp_home = tempfile::tempdir().expect("temp tachi home");
    let temp_runs = tempfile::tempdir().expect("temp run root");
    let _home = EnvVarGuard::set_path("TACHI_HOME", temp_home.path());
    let _run_root = EnvVarGuard::set_path("TACHI_RUN_ROOT", temp_runs.path());
    let server = make_server();
    let flow_id = "flow_20260709T000002Z_f2_cycle_status_link";
    let issue = issue_snapshot(
        918,
        vec!["docs/engineering/architecture/project-cycle-memory-spine.md"],
        vec!["docs/engineering/specs/project-cycle-read-model.md"],
    );
    write_intake_flow(flow_id, &issue);

    let mut params = task_params("status");
    params.flow_id = Some(flow_id.to_string());
    let raw = server
        .tachi_task(Parameters(params))
        .await
        .expect("status cycle should succeed");
    let parsed = cycle_view(&raw);
    let next = parsed["next_action"].as_str().expect("next_action string");
    assert!(
        next.contains("tachi_gh(action='pr_handoff'") && next.contains("tachi_gh(action='link_pr'"),
        "missing PR must coach tachi_gh pr_handoff+link_pr, got: {next}"
    );
    assert_no_deprecated_lifecycle_coaching("status.cycle.next_action(link)", next);
}
