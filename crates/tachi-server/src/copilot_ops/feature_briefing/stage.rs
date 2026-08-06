use super::super::*;

pub(super) fn infer_feature_stage(run_artifacts: &[Value], board: &Value) -> String {
    if let Some(task) = board
        .get("tasks")
        .and_then(Value::as_array)
        .and_then(|tasks| tasks.first())
    {
        if let Some(state) = task.get("state").and_then(Value::as_str) {
            return state.to_string();
        }
    }
    let has_result = run_artifacts.iter().any(|artifact| {
        artifact
            .get("path")
            .and_then(Value::as_str)
            .is_some_and(|path| path.ends_with("result.md"))
            && artifact.get("exists").and_then(Value::as_bool) == Some(true)
    });
    if has_result {
        "result_available".to_string()
    } else if !run_artifacts.is_empty() {
        "flow_started".to_string()
    } else {
        "intake".to_string()
    }
}

pub(super) fn feature_next_action(
    params: &TachiTaskParams,
    canonical_docs: &[Value],
    run_artifacts: &[Value],
    board: &Value,
    memory_rows: &[Value],
) -> String {
    if canonical_docs.is_empty() {
        return "Attach or create a canonical docs/spec reference before treating memory as feature truth.".to_string();
    }
    if board
        .get("tasks")
        .and_then(Value::as_array)
        .is_some_and(|tasks| {
            tasks
                .iter()
                .any(|task| task.get("state").and_then(Value::as_str) == Some("TASK_STATE_WORKING"))
        })
    {
        return "Poll tachi_task(action='board') and collect the active worker result before dispatching more work.".to_string();
    }
    if let Some(command) = cycle_status_command(params) {
        return format!(
            "Run {command} to inspect current issue/PR/docs/verification lifecycle state."
        );
    }
    if run_artifacts.iter().any(|artifact| {
        artifact
            .get("path")
            .and_then(Value::as_str)
            .is_some_and(|path| path.ends_with("instruction.md"))
            && artifact.get("exists").and_then(Value::as_bool) == Some(true)
    }) && !run_artifacts.iter().any(|artifact| {
        artifact
            .get("path")
            .and_then(Value::as_str)
            .is_some_and(|path| path.ends_with("result.md"))
            && artifact.get("exists").and_then(Value::as_bool) == Some(true)
    }) {
        return "Use the flow instruction packet as the handoff source for a bounded harness-native subagent; use Tachi dispatch only for an explicit durable/remote exception.".to_string();
    }
    if memory_rows.is_empty() {
        return "Save a checkpoint after the next concrete decision.".to_string();
    }
    "Use a harness-native subagent or review with explicit verification.".to_string()
}

fn cycle_status_command(params: &TachiTaskParams) -> Option<String> {
    params
        .flow_id
        .as_deref()
        .filter(|flow_id| !flow_id.trim().is_empty())
        .map(|flow_id| format!("tachi_task(action='cycle_status', flow_id='{flow_id}')"))
        .or_else(|| {
            params
                .issue_ref
                .as_deref()
                .filter(|issue_ref| !issue_ref.trim().is_empty())
                .map(|issue_ref| {
                    format!("tachi_task(action='cycle_status', issue_ref='{issue_ref}')")
                })
        })
        .or_else(|| {
            params
                .pr_ref
                .as_deref()
                .filter(|pr_ref| !pr_ref.trim().is_empty())
                .map(|pr_ref| format!("tachi_task(action='cycle_status', pr_ref='{pr_ref}')"))
        })
}
