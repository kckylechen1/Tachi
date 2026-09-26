use super::*;

pub(crate) async fn handle_tachi_progress_check(
    server: &MemoryServer,
    params: ProgressCheckParams,
) -> Result<String, String> {
    let attempt_count = params.attempts.len();
    let repeated_layer = params
        .attempts
        .iter()
        .filter(|attempt| {
            let lower = attempt.to_ascii_lowercase();
            lower.contains("transport")
                || lower.contains("proxy")
                || lower.contains("http")
                || lower.contains("传输")
        })
        .count()
        >= 2;
    let has_error = params
        .latest_error
        .as_ref()
        .map(|value| !value.trim().is_empty())
        .unwrap_or(false);
    let stuck = attempt_count >= 3 || (attempt_count >= 2 && has_error) || repeated_layer;
    let query = format!(
        "{} {} {}",
        params.task,
        params.latest_error.clone().unwrap_or_default(),
        params.attempts.join(" ")
    );
    let top_k = crate::clamp_facade_top_k(params.top_k);
    let wiki_plan = WikiReadPlan::from_project(params.project.as_deref())?;
    let wiki_rows = crate::wiki_ops::search_wiki_rows_for_plan(
        server,
        SearchMemoryParams {
            query: query.clone(),
            query_vec: None,
            top_k,
            path_prefix: Some("/wiki".to_string()),
            include_training: false,
            include_archived: false,
            candidates_per_channel: top_k.max(20),
            mmr_threshold: Some(0.85),
            graph_expand_hops: 1,
            graph_relation_filter: None,
            weights: None,
            context_symbols: Vec::new(),
            agent_role: params.agent_id.clone(),
            project: params.project.clone(),
            domain: params.domain.clone(),
            file_context: None,
            error_context: None,
            enable_rerank: false,
            as_of: None,
            include_metadata: false,
            format: None,
        },
        &wiki_plan,
        None,
        false,
    )
    .await?
    .rows;
    let debug_checklist = build_debug_checklist(&wiki_rows);

    let ask_codex_prompt = format!(
        "Review this stuck debugging task and identify the most likely wrong assumption.\n\nTask: {}\n\nAttempts:\n{}\n\nLatest error:\n{}\n\nPlease reason from the observed error backward across boundaries before proposing code changes.",
        params.task,
        params
            .attempts
            .iter()
            .enumerate()
            .map(|(idx, attempt)| format!("{}. {}", idx + 1, attempt))
            .collect::<Vec<_>>()
            .join("\n"),
        params.latest_error.as_deref().unwrap_or("(none provided)")
    );
    let progress_log = if let Some(flow_id) = params.flow_id.as_deref() {
        record_progress_check_event(flow_id, &params, stuck)?
    } else {
        None
    };

    serde_json::to_string(&json!({
        "status": "ok",
        "stuck": stuck,
        "attempt_count": attempt_count,
        "signals": {
            "has_latest_error": has_error,
            "repeated_same_layer": repeated_layer,
        },
        "reason": if stuck {
            "The task shows repeated attempts or continued errors; stop patching and reframe."
        } else {
            "No strong stuck signal yet; keep validating the next narrow hypothesis."
        },
        "suggested_reframe": "Trace where the invariant first fails. For MCP parameter bugs, check schema -> client serialization -> server deserialization -> handler -> transport before changing transport code.",
        "wiki_hits": compact_rows(wiki_rows, top_k),
        "debug_checklist": debug_checklist,
        "should_ask_codex": stuck,
        "ask_codex_prompt": ask_codex_prompt,
        "progress_log": progress_log,
        "next_actions": if stuck {
            json!(["search wiki hits", "write a failing boundary test", "ask another agent with ask_codex_prompt", "only then edit code"])
        } else {
            json!(["continue one narrow validation", "record the result", "call tachi_memory(action='alerts') after another failed attempt"])
        },
    }))
    .map_err(|e| format!("serialize progress_check: {e}"))
}

fn record_progress_check_event(
    flow_id: &str,
    params: &ProgressCheckParams,
    stuck: bool,
) -> Result<Option<String>, String> {
    use std::io::Write;

    if flow_id.is_empty()
        || flow_id.contains('/')
        || flow_id.contains('\\')
        || flow_id.contains("..")
        || !flow_id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
    {
        return Err(format!("Invalid flow_id: '{flow_id}'"));
    }
    let run_dir = crate::task_lifecycle::flow_runs_root().join(flow_id);
    std::fs::create_dir_all(&run_dir).map_err(|e| format!("create progress run dir: {e}"))?;
    let path = run_dir.join("progress.jsonl");
    let line = serde_json::to_string(&json!({
        "timestamp": Utc::now().to_rfc3339(),
        "flow_id": flow_id,
        "event": "progress_check",
        "task": params.task,
        "attempt_count": params.attempts.len(),
        "latest_error": params.latest_error,
        "stuck": stuck,
        "project": params.project,
        "domain": params.domain,
    }))
    .map_err(|e| format!("serialize progress check: {e}"))?;
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .map_err(|e| format!("open {}: {e}", path.display()))?;
    writeln!(file, "{line}").map_err(|e| format!("write {}: {e}", path.display()))?;
    Ok(Some(path.display().to_string()))
}
