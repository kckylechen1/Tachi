use super::*;

pub(super) fn wants_json_format(format: Option<&str>) -> bool {
    crate::facade_memory_ops::wants_json(format)
}

pub(super) fn format_facade_response(
    title: &str,
    action: &str,
    raw: &str,
    format: Option<&str>,
) -> Result<String, String> {
    // format=full is the agent-facing verbose JSON receipt (#527), not human markdown.
    if wants_json_format(format) || crate::facade_memory_ops::wants_full_format(format) {
        return normalize_json_facade_response(action, raw);
    }
    let value = serde_json::from_str::<Value>(raw).map_err(|e| {
        format!("format {title} markdown response: expected JSON from action '{action}': {e}")
    })?;
    if action == "recommend" {
        return Ok(render_recommend_markdown(title, action, &value));
    }
    if action == "profiles" {
        return Ok(render_profiles_markdown(title, action, &value));
    }
    let mut lines = vec![format!("## {title}")];
    lines.push(format!("action: `{action}`"));
    append_known_field(&mut lines, &value, "arena_id");
    append_known_field(&mut lines, &value, "mission_id");
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
    append_known_field(&mut lines, &value, "arena_dir");
    append_known_field(&mut lines, &value, "mission_dir");
    append_known_field(&mut lines, &value, "instruction_path");
    append_known_field(&mut lines, &value, "prompt_file");
    append_known_field(&mut lines, &value, "trajectory_file");
    append_known_field(&mut lines, &value, "context_file");
    append_known_field(&mut lines, &value, "message");
    append_known_field(&mut lines, &value, "dispatch_error");

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

    if lines.len() <= 2 {
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

pub(super) fn md_opt_str(value: &Value, field: &str) -> String {
    value
        .get(field)
        .and_then(Value::as_str)
        .filter(|text| !text.is_empty())
        .map(md_table_cell)
        .unwrap_or_else(|| "-".to_string())
}

pub(super) fn md_opt_num(value: &Value, field: &str) -> String {
    match value.get(field) {
        Some(Value::Number(n)) => {
            // Integers (incl. large ones that would lose precision as f64) render
            // exactly via as_i64; non-integers fall back to 2dp, and whole-number
            // floats (e.g. 3.0) still render without a trailing ".00".
            if let Some(i) = n.as_i64() {
                format!("{i}")
            } else if let Some(f) = n.as_f64() {
                if f.fract().abs() < f64::EPSILON {
                    format!("{}", f as i64)
                } else {
                    format!("{f:.2}")
                }
            } else {
                "-".to_string()
            }
        }
        _ => "-".to_string(),
    }
}

/// Render the `recommend` action as a candidates table plus the resolved routing summary.
pub(super) fn render_recommend_markdown(title: &str, action: &str, value: &Value) -> String {
    let mut lines = vec![format!("## {title}"), format!("action: `{action}`")];

    if let Some(task) = value
        .get("task")
        .and_then(Value::as_str)
        .filter(|t| !t.is_empty())
    {
        lines.push(format!("task: {}", md_table_cell(task)));
    }
    lines.push(format!(
        "recommended_profile: `{}`",
        value
            .get("recommended_profile")
            .and_then(Value::as_str)
            .unwrap_or("-")
    ));
    lines.push(format!(
        "recommended_transport: `{}`",
        value
            .get("recommended_transport")
            .and_then(Value::as_str)
            .unwrap_or("-")
    ));
    if let Some(chain) = value.get("fallback_chain").and_then(Value::as_array) {
        let chain = chain
            .iter()
            .filter_map(Value::as_str)
            .collect::<Vec<_>>()
            .join(" -> ");
        if !chain.is_empty() {
            lines.push(format!("fallback_chain: {chain}"));
        }
    }

    lines.push(String::new());
    lines.push("| profile | role | score | useful_rate | top reason |".to_string());
    lines.push("| --- | --- | --- | --- | --- |".to_string());
    if let Some(candidates) = value.get("candidates").and_then(Value::as_array) {
        for candidate in candidates {
            let profile = md_opt_str(candidate, "profile");
            let role = md_opt_str(candidate, "role");
            let score = md_opt_num(candidate, "score");
            let useful_rate = md_opt_num(candidate, "useful_rate");
            let top_reason = candidate
                .get("reasons")
                .and_then(Value::as_array)
                .and_then(|reasons| reasons.first())
                .and_then(Value::as_str)
                .filter(|text| !text.is_empty())
                .map(md_table_cell)
                .unwrap_or_else(|| "-".to_string());
            lines.push(format!(
                "| {profile} | {role} | {score} | {useful_rate} | {top_reason} |"
            ));
        }
    }

    lines.join("\n")
}

/// Render the `profiles` action as a table of configured dispatch profiles.
pub(super) fn render_profiles_markdown(title: &str, action: &str, value: &Value) -> String {
    let mut lines = vec![format!("## {title}"), format!("action: `{action}`")];
    lines.push(String::new());
    lines.push(
        "| name | role | stage | backend | cost | precision | speed | strong_against |".to_string(),
    );
    lines.push("| --- | --- | --- | --- | --- | --- | --- | --- |".to_string());
    if let Some(profiles) = value.get("dispatch_profiles").and_then(Value::as_array) {
        for profile in profiles {
            let name = md_opt_str(profile, "name");
            let role = md_opt_str(profile, "role");
            let stage = md_opt_str(profile, "stage");
            let backend = md_opt_str(profile, "backend");
            let null_stats = Value::Null;
            let stats = profile
                .get("mbit_card")
                .and_then(|card| card.get("stats"))
                .unwrap_or(&null_stats);
            let cost = md_opt_num(stats, "cost");
            let precision = md_opt_num(stats, "precision");
            let speed = md_opt_num(stats, "speed");
            let strong_against = profile
                .get("mbit_card")
                .and_then(|card| card.get("strong_against"))
                .and_then(Value::as_array)
                .map(|items| {
                    items
                        .iter()
                        .filter_map(Value::as_str)
                        .collect::<Vec<_>>()
                        .join(", ")
                })
                .filter(|text| !text.is_empty())
                .map(|text| md_table_cell(&text))
                .unwrap_or_else(|| "-".to_string());
            lines.push(format!(
                "| {name} | {role} | {stage} | {backend} | {cost} | {precision} | {speed} | {strong_against} |"
            ));
        }
    }

    lines.join("\n")
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
