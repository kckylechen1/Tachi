use super::*;

pub(super) fn wants_json_format(format: Option<&str>) -> bool {
    crate::facade_memory_ops::wants_json(format)
}

/// Historically an omitted `format` meant JSON everywhere. Keep that default
/// for the surviving Task actions; the operator-only `tachi card` command
/// owns its own rendering.
fn resolved_action_format(action: &str, format: Option<&str>) -> Option<String> {
    let explicit = format.map(str::trim).filter(|f| !f.is_empty());
    if explicit.is_some() {
        return explicit.map(str::to_string);
    }
    let _ = action;
    None
}

pub(crate) fn format_facade_response(
    title: &str,
    action: &str,
    raw: &str,
    format: Option<&str>,
    _verbose: bool,
) -> Result<String, String> {
    let resolved = resolved_action_format(action, format);
    let format = resolved.as_deref();
    // format=full is the agent-facing verbose JSON receipt (#527), not human
    // markdown.
    let wants_full = crate::facade_memory_ops::wants_full_format(format);
    if wants_json_format(format) || wants_full {
        return normalize_json_facade_response(action, raw);
    }
    let value = serde_json::from_str::<Value>(raw).map_err(|e| {
        format!("format {title} markdown response: expected JSON from action '{action}': {e}")
    })?;
    let mut lines = vec![format!("## {title}")];
    lines.push(format!("action: `{action}`"));
    append_known_field(&mut lines, &value, "flow_id");
    append_known_field(&mut lines, &value, "dispatch_id");
    append_known_field(&mut lines, &value, "task_id");
    append_known_field(&mut lines, &value, "overall");
    append_known_field(&mut lines, &value, "next_action");
    append_known_field(&mut lines, &value, "stage");
    append_known_field(&mut lines, &value, "state");
    append_known_field(&mut lines, &value, "outcome");
    append_known_field(&mut lines, &value, "path");
    append_known_field(&mut lines, &value, "eval_path");
    append_known_field(&mut lines, &value, "eval_memory_id");
    append_known_field(&mut lines, &value, "run_dir");
    append_known_field(&mut lines, &value, "instruction_path");
    append_known_field(&mut lines, &value, "prompt_file");
    append_known_field(&mut lines, &value, "trajectory_file");
    append_known_field(&mut lines, &value, "context_file");
    append_known_field(&mut lines, &value, "message");
    append_known_field(&mut lines, &value, "dispatch_error");
    if action == "board" {
        for field in [
            "incomplete",
            "limit_incomplete",
            "warning",
            "incomplete_reasons",
            "kanban_fetch_truncated",
            "flow_fetch_truncated",
            "run_fallback_incomplete",
            "run_scan_truncated",
            "run_scan_invalid_entries",
        ] {
            append_known_field(&mut lines, &value, field);
        }
    }
    append_host_admission_markdown(&mut lines, &value);

    if let Some(eval_entry) = value.get("eval_entry") {
        append_known_field(&mut lines, eval_entry, "id");
        append_known_field(&mut lines, eval_entry, "path");
        append_known_field(&mut lines, eval_entry, "status");
    }
    if let Some(pipeline) = value.get("pipeline") {
        append_known_field(&mut lines, pipeline, "post_complete_hooks");
        if let Some(link) = pipeline.get("dispatch_completion_link") {
            append_known_field(&mut lines, link, "recorded");
            append_known_field(&mut lines, link, "eval_memory_id");
            append_known_field(&mut lines, link, "eval_path");
        }
    }

    if let Some(matrix) = value.get("matrix").and_then(Value::as_array) {
        let passed = matrix
            .iter()
            .filter(|step| step.get("status").and_then(Value::as_str) == Some("passed"))
            .count();
        let pending = matrix
            .iter()
            .filter(|step| step.get("status").and_then(Value::as_str) == Some("pending"))
            .count();
        let ready = matrix
            .iter()
            .filter(|step| step.get("status").and_then(Value::as_str) == Some("ready"))
            .count();
        let blocked = matrix
            .iter()
            .filter(|step| step.get("status").and_then(Value::as_str) == Some("blocked"))
            .count();
        lines.push(format!(
            "matrix: passed={passed} ready={ready} pending={pending} blocked={blocked}"
        ));
        for step in matrix.iter().take(12) {
            let id = step.get("id").and_then(Value::as_str).unwrap_or("(step)");
            let status = step
                .get("status")
                .and_then(Value::as_str)
                .unwrap_or("unknown");
            let gap = step
                .get("gaps")
                .and_then(Value::as_array)
                .and_then(|gaps| gaps.first())
                .and_then(Value::as_str)
                .filter(|gap| !gap.is_empty())
                .map(|gap| format!(" gap={gap}"))
                .unwrap_or_default();
            lines.push(format!("- `{id}` {status}{gap}"));
        }
    }

    if let Some(tasks) = value.get("tasks").and_then(Value::as_array) {
        lines.push(format!("tasks: {}", tasks.len()));
        for task in tasks.iter().take(10) {
            let id = task
                .get("dispatch_id")
                .and_then(Value::as_str)
                .or_else(|| task.get("id").and_then(Value::as_str))
                .unwrap_or("(task)");
            let state = task
                .get("state")
                .and_then(Value::as_str)
                .unwrap_or("unknown");
            let agent = task
                .get("agent")
                .and_then(Value::as_str)
                .unwrap_or("unknown");
            let summary = task
                .get("task")
                .and_then(Value::as_str)
                .or_else(|| task.get("title").and_then(Value::as_str))
                .or_else(|| task.get("summary").and_then(Value::as_str))
                .filter(|text| !text.is_empty())
                .map(|text| format!(" - {text}"))
                .unwrap_or_default();
            lines.push(format!("- `{id}` {state} agent={agent}{summary}"));
        }
    }

    if let Some(flows) = value.get("flows").and_then(Value::as_array) {
        lines.push(format!("flows: {}", flows.len()));
        for flow in flows.iter().take(10) {
            let id = flow
                .get("flow_id")
                .and_then(Value::as_str)
                .unwrap_or("(flow)");
            let stage = flow.get("stage").and_then(Value::as_str).unwrap_or("?");
            let state = flow.get("state").and_then(Value::as_str).unwrap_or("?");
            let summary = flow
                .get("title")
                .and_then(Value::as_str)
                .or_else(|| flow.get("task").and_then(Value::as_str))
                .or_else(|| flow.get("summary").and_then(Value::as_str))
                .filter(|text| !text.is_empty())
                .map(|text| format!(" - {text}"))
                .unwrap_or_default();
            lines.push(format!("- `{id}` stage={stage} state={state}{summary}"));
        }
    }

    if lines.len() <= 2 || value.get("policies").is_some() {
        lines.push(format!("```json\n{}\n```", value));
    }
    Ok(lines.join("\n"))
}

fn normalize_json_facade_response(action: &str, raw: &str) -> Result<String, String> {
    let Ok(mut value) = serde_json::from_str::<Value>(raw) else {
        return Ok(raw.to_string());
    };
    if let Some(obj) = value.as_object_mut() {
        obj.entry("status".to_string())
            .or_insert_with(|| Value::String("completed".to_string()));
        obj.entry("action".to_string())
            .or_insert_with(|| Value::String(action.to_string()));
    }
    serde_json::to_string(&value).map_err(|e| format!("serialize normalized facade JSON: {e}"))
}

/// Escape pipe characters so free-form text stays inside one markdown table cell.
pub(super) fn md_table_cell(text: &str) -> String {
    text.replace('|', "\\|").replace('\n', " ")
}

fn append_host_admission_markdown(lines: &mut Vec<String>, value: &Value) {
    let Some(admission) = value.get("host_admission").and_then(Value::as_object) else {
        return;
    };
    let receipt_value = |field: &str| match admission.get(field) {
        Some(Value::String(value)) => md_table_cell(value),
        Some(Value::Bool(value)) => value.to_string(),
        Some(Value::Null) | None => "-".to_string(),
        Some(value) => md_table_cell(&value.to_string()),
    };

    lines.push(String::new());
    lines.push("host_admission:".to_string());
    lines.push(
        "| requested_level | effective_level | max_execution_level | host_profile | profile_source | allowed | reason_code |"
            .to_string(),
    );
    lines.push("| --- | --- | --- | --- | --- | --- | --- |".to_string());
    lines.push(format!(
        "| {} | {} | {} | {} | {} | {} | {} |",
        receipt_value("requested_level"),
        receipt_value("effective_level"),
        receipt_value("max_execution_level"),
        receipt_value("host_profile"),
        receipt_value("profile_source"),
        receipt_value("allowed"),
        receipt_value("reason_code"),
    ));
}

pub(super) fn append_known_field(lines: &mut Vec<String>, value: &Value, field: &str) {
    let Some(raw) = value.get(field) else {
        return;
    };
    if raw.is_null() {
        return;
    }
    if let Some(text) = raw.as_str() {
        if !text.is_empty() {
            lines.push(format!("{field}: `{text}`"));
        }
    } else {
        lines.push(format!("{field}: `{raw}`"));
    }
}
