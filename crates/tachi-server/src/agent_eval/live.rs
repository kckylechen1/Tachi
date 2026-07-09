use super::*;

fn task_type_from_str(value: Option<&str>) -> TaskType {
    value
        .and_then(|s| serde_json::from_value(serde_json::json!(s)).ok())
        .unwrap_or(TaskType::Other)
}

fn completion_status_from_outcome(value: Option<&str>) -> CompletionStatus {
    match value.unwrap_or("").to_ascii_lowercase().as_str() {
        "success" | "completed" => CompletionStatus::Completed,
        "partial" => CompletionStatus::Stalled,
        "aborted" => CompletionStatus::Blocked,
        "failure" | "failed" => CompletionStatus::Stalled,
        _ => CompletionStatus::Exploratory,
    }
}

fn eval_row_from_memory(entry: &memcore::MemoryEntry) -> Option<EvalRow> {
    let meta = entry.metadata.as_object()?;
    let agent = meta.get("agent")?.as_str()?.to_string();
    let outcome = meta.get("outcome").and_then(|v| v.as_str());
    let task_type = task_type_from_str(meta.get("task_type").and_then(|v| v.as_str()));
    let subagents: Vec<SubagentEvalRow> = meta
        .get("subagents")
        .cloned()
        .and_then(|v| serde_json::from_value(v).ok())
        .unwrap_or_default();

    Some(EvalRow {
        agent,
        profile: meta
            .get("profile")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string()),
        model: meta
            .get("model")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string()),
        mode: meta
            .get("mode")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string()),
        task_type,
        turns: meta.get("turns").and_then(|v| v.as_u64()).unwrap_or(0) as u32,
        tool_calls: meta.get("tool_calls").and_then(|v| v.as_u64()).unwrap_or(0) as u32,
        verification_present: meta
            .get("verification_present")
            .and_then(|v| v.as_bool())
            .unwrap_or(false),
        failure_mode: meta
            .get("failure_mode")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string()),
        completion_status: completion_status_from_outcome(outcome),
        cost_usd: meta.get("cost_usd").and_then(|v| v.as_f64()),
        cost_tokens: meta.get("cost_tokens").and_then(|v| v.as_u64()),
        quality_score: meta.get("quality_score").and_then(|v| v.as_f64()),
        latency_ms: meta.get("duration_ms").and_then(|v| v.as_u64()),
        subagents,
    })
}

pub(crate) fn load_live_eval_rows(
    server: &MemoryServer,
    limit: usize,
) -> Result<Vec<EvalRow>, String> {
    let mut entries = server.with_global_store_read(|store| {
        store
            .list_by_path("/eval", limit, false)
            .map_err(|e| format!("list global eval rows: {e}"))
    })?;
    if server.has_project_db() {
        let mut project_entries = server.with_project_store_read(|store| {
            store
                .list_by_path("/eval", limit, false)
                .map_err(|e| format!("list project eval rows: {e}"))
        })?;
        entries.append(&mut project_entries);
    }
    entries.sort_by(|a, b| b.timestamp.cmp(&a.timestamp));
    entries.truncate(limit);
    Ok(entries
        .iter()
        .filter(|entry| {
            !entry
                .metadata
                .get("auto_synthesized")
                .and_then(Value::as_bool)
                .unwrap_or(false)
        })
        .filter_map(eval_row_from_memory)
        .collect())
}
