use super::*;

pub(super) fn wants_json_format(format: Option<&str>) -> bool {
    crate::facade_memory_ops::wants_json(format)
}

/// tachi#1201: `recommend`/`profiles`/`profile`/`card` are read-heavy
/// discovery endpoints. Historically an omitted `format` meant JSON
/// everywhere (`wants_json`'s global `None => true` default, shared by every
/// OTHER facade action — save/checkpoint/briefing/wiki/... ). Scope the
/// default-format flip to just these four actions instead of touching that
/// global default: when the caller hasn't set `format` at all (or set it to
/// an empty string), resolve it to `"markdown"` for these actions only.
fn resolved_action_format(action: &str, format: Option<&str>) -> Option<String> {
    let explicit = format.map(str::trim).filter(|f| !f.is_empty());
    if explicit.is_some() {
        return explicit.map(str::to_string);
    }
    if matches!(action, "recommend" | "profiles" | "profile" | "card") {
        Some("markdown".to_string())
    } else {
        None
    }
}

/// Per-row keys kept in the `recommend` JSON candidate shape (tachi#1201 item
/// 2). The dropped fields (agent/model/live_samples/useful_rate/
/// failure_count/performance_samples/human_override_rate/avg_retry_count/
/// avg_latency_ms/avg_cost_usd) duplicate what the top-level `mbit_card` (or
/// the live-eval evidence a caller can fetch with format='full') already
/// carries — the per-row fat is what item 2 asks to de-duplicate. `reasons`
/// is kept whole (not truncated): it is typically a handful of short
/// strings, and several existing callers key routing assertions off finding
/// a specific reason string anywhere in that list.
const RECOMMEND_CANDIDATE_ROW_KEYS: &[&str] = &["profile", "role", "score"];

fn slim_recommend_candidate(candidate: &Value) -> Value {
    let mut row = serde_json::Map::new();
    for key in RECOMMEND_CANDIDATE_ROW_KEYS {
        if let Some(field) = candidate.get(*key) {
            row.insert((*key).to_string(), field.clone());
        }
    }
    row.insert(
        "reasons".to_string(),
        candidate
            .get("reasons")
            .cloned()
            .unwrap_or_else(|| Value::Array(Vec::new())),
    );
    Value::Object(row)
}

/// JSON-mode shaping for `action='recommend'` (tachi#1201 item 2): slims
/// every `candidates` row unconditionally, and gates the two large
/// unconditionally-embedded fields the pre-#1201 shape always carried:
/// `identity_receipt` (kept only when `verbose=true`) and the single
/// top-level `mbit_card` (kept only when `include_card=true`). Response
/// shapes that don't carry these keys at all (e.g. the host-admission
/// decline receipt, which returns before `candidates`/`mbit_card` are ever
/// built) pass through unchanged aside from the usual status/action fill.
fn normalize_recommend_json_response(
    raw: &str,
    verbose: bool,
    include_card: bool,
) -> Result<String, String> {
    let Ok(mut value) = serde_json::from_str::<Value>(raw) else {
        return Ok(raw.to_string());
    };
    if let Some(obj) = value.as_object_mut() {
        obj.entry("status".to_string())
            .or_insert_with(|| Value::String("completed".to_string()));
        obj.entry("action".to_string())
            .or_insert_with(|| Value::String("recommend".to_string()));
        if !verbose {
            obj.remove("identity_receipt");
        }
        if !include_card {
            obj.remove("mbit_card");
        }
        if let Some(candidates) = obj.get("candidates").and_then(Value::as_array) {
            let slim = candidates
                .iter()
                .map(slim_recommend_candidate)
                .collect::<Vec<_>>();
            obj.insert("candidates".to_string(), Value::Array(slim));
        }
    }
    serde_json::to_string(&value).map_err(|e| format!("serialize normalized recommend JSON: {e}"))
}

pub(crate) fn format_facade_response(
    title: &str,
    action: &str,
    raw: &str,
    format: Option<&str>,
    verbose: bool,
    include_card: bool,
) -> Result<String, String> {
    let resolved = resolved_action_format(action, format);
    let format = resolved.as_deref();
    // format=full is the agent-facing verbose JSON receipt (#527), not human
    // markdown, and (tachi#1201) always bypasses recommend's JSON slimming —
    // "full" already means "give me everything", the same intent as
    // verbose=true + include_card=true together.
    let wants_full = crate::facade_memory_ops::wants_full_format(format);
    if wants_json_format(format) || wants_full {
        if action == "recommend" && !wants_full {
            return normalize_recommend_json_response(raw, verbose, include_card);
        }
        return normalize_json_facade_response(action, raw);
    }
    let value = serde_json::from_str::<Value>(raw).map_err(|e| {
        format!("format {title} markdown response: expected JSON from action '{action}': {e}")
    })?;
    if action == "recommend" {
        return Ok(render_recommend_markdown(title, action, &value));
    }
    // tachi#1201: `profile`/`card` are pre-existing on-demand-full-card
    // aliases of the same `dispatch_profiles_json_for_server` listing
    // `profiles` renders — same response shape (`dispatch_profiles` +
    // `verbose`), so they share the same table renderer now that all three
    // default to markdown.
    if matches!(action, "profiles" | "profile" | "card") {
        return Ok(render_profiles_markdown(title, action, &value));
    }
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
    append_host_admission_markdown(&mut lines, value);
    if let Some(profile) = value.get("recommended_profile").and_then(Value::as_str) {
        lines.push(format!("recommended_profile: `{profile}`"));
    }
    if let Some(transport) = value.get("recommended_transport").and_then(Value::as_str) {
        lines.push(format!("recommended_transport: `{transport}`"));
    }
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

    if let Some(candidates) = value.get("candidates").and_then(Value::as_array) {
        lines.push(String::new());
        lines.push("| profile | role | score | useful_rate | top reason |".to_string());
        lines.push("| --- | --- | --- | --- | --- |".to_string());
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
///
/// #1182 checkpoint 4 (codex review round 2): tachi#1173 item 2 slimmed the
/// JSON default to `name`/`backend`/`model`/`role` rows with no `mbit_card`,
/// but this renderer used to unconditionally read `stage`/`mbit_card.stats`/
/// `strong_against` — on the new slim default those columns silently
/// rendered `-` for every row and dropped `model` from view entirely, a
/// human-facing UX regression no test exercised end-to-end. Mirror the same
/// verbose/slim split the JSON response uses: `value["verbose"]` (echoed by
/// `dispatch_profiles_json_for_server`) selects which table shape to render.
pub(super) fn render_profiles_markdown(title: &str, action: &str, value: &Value) -> String {
    let mut lines = vec![format!("## {title}"), format!("action: `{action}`")];
    lines.push(String::new());

    let verbose = value
        .get("verbose")
        .and_then(Value::as_bool)
        .unwrap_or(false);

    if !verbose {
        lines.push("| name | backend | model | role |".to_string());
        lines.push("| --- | --- | --- | --- |".to_string());
        if let Some(profiles) = value.get("dispatch_profiles").and_then(Value::as_array) {
            for profile in profiles {
                let name = md_opt_str(profile, "name");
                let backend = md_opt_str(profile, "backend");
                let model = md_opt_str(profile, "model");
                let role = md_opt_str(profile, "role");
                lines.push(format!("| {name} | {backend} | {model} | {role} |"));
            }
        }
        return lines.join("\n");
    }

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
