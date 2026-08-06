//! #1683 / PR #1697 cold-review discriminator: cycle_status returns
//! `next_action` (not the retired `next_step` from cycle_plan), and intake /
//! agent-rules coaching that mentions cycle_status must teach that field.

use super::*;

#[allow(clippy::await_holding_lock)]
#[tokio::test]
async fn cycle_status_json_exposes_next_action_not_next_step() {
    let _lock = crate::utils::global_test_lock()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let temp_home = tempfile::tempdir().expect("temp tachi home");
    let temp_runs = tempfile::tempdir().expect("temp run root");
    let _home = EnvVarGuard::set_path("TACHI_HOME", temp_home.path());
    let _run_root = EnvVarGuard::set_path("TACHI_RUN_ROOT", temp_runs.path());
    let server = make_server();
    let flow_id = "flow_20260806T000001Z_cycle_status_next_action_field";
    let issue = issue_snapshot(
        1683,
        vec!["docs/engineering/architecture/project-cycle-memory-spine.md"],
        vec!["docs/engineering/specs/project-cycle-read-model.md"],
    );
    write_intake_flow(flow_id, &issue);

    let mut params = task_params("cycle_status");
    params.flow_id = Some(flow_id.to_string());
    let raw = server
        .tachi_task(Parameters(params))
        .await
        .expect("cycle_status should succeed");
    let parsed: Value = serde_json::from_str(&raw).expect("cycle_status JSON");

    let next = parsed["next_action"]
        .as_str()
        .expect("cycle_status must return next_action as a string");
    assert!(
        !next.is_empty(),
        "next_action must be a non-empty coaching string, got: {next:?}"
    );
    assert!(
        parsed.get("next_step").is_none(),
        "cycle_status must not expose retired next_step; got: {parsed:#}"
    );
}

#[test]
fn intake_and_agent_rules_coach_cycle_status_next_action_not_next_step() {
    let issue = issue_snapshot(
        1683,
        vec!["docs/engineering/architecture/project-cycle-memory-spine.md"],
        vec!["docs/engineering/specs/project-cycle-read-model.md"],
    );
    let plan = crate::task_lifecycle::build_issue_automation_plan(&issue, None);
    let recommended = plan["recommended_next_action"]
        .as_str()
        .expect("recommended_next_action string");
    assert!(
        recommended.contains("tachi_task(action='cycle_status'"),
        "intake plan must route through cycle_status: {recommended}"
    );
    assert!(
        recommended.contains("follow its next_action"),
        "intake plan must teach cycle_status.next_action, got: {recommended}"
    );
    assert!(
        !recommended.contains("next_step"),
        "intake plan must not teach retired next_step for cycle_status, got: {recommended}"
    );

    let rules = crate::bootstrap::setup_wizard::agent_rules::agent_memory_rules_block();
    assert!(
        rules.contains("cycle_status") && rules.contains("`next_action`"),
        "agent rules must teach cycle_status next_action, got: {rules}"
    );
    assert!(
        !rules.contains("`next_step`"),
        "agent rules must not teach retired next_step for cycle_status, got: {rules}"
    );
}
