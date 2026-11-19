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
