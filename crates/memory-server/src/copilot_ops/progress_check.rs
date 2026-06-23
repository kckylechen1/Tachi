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
    let wiki_rows = search_memory_rows(
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
        },
        false,
    )
    .await?;
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
            json!(["continue one narrow validation", "record the result", "call tachi_progress_check again after another failed attempt"])
        },
    }))
    .map_err(|e| format!("serialize progress_check: {e}"))
}

pub(super) fn record_progress_check_event(
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
    let run_dir = crate::shell_ops::shell_runs_root().join(flow_id);
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

pub(super) async fn build_route_recommendation(
    server: &MemoryServer,
    task: &str,
    project: Option<&str>,
) -> serde_json::Value {
    let eval_rows = match search_memory_rows(
        server,
        SearchMemoryParams {
            query: task.to_string(),
            query_vec: None,
            top_k: 20,
            path_prefix: Some("/eval/".to_string()),
            include_training: false,
            include_archived: false,
            candidates_per_channel: 40,
            mmr_threshold: None,
            graph_expand_hops: 0,
            graph_relation_filter: None,
            weights: None,
            context_symbols: Vec::new(),
            agent_role: None,
            project: project.map(|s| s.to_string()),
            domain: None,
            file_context: None,
            error_context: None,
            enable_rerank: false,
            as_of: None,
            include_metadata: false,
        },
        false,
    )
    .await
    {
        Ok(rows) => rows,
        Err(_) => return json!({"available": false}),
    };

    if eval_rows.is_empty() {
        return json!({"available": false, "reason": "no eval history"});
    }

    let mut agent_stats: std::collections::HashMap<String, (u32, u32)> =
        std::collections::HashMap::new();

    for row in &eval_rows {
        let meta = match row.get("metadata") {
            Some(m) => m,
            None => continue,
        };
        let agent = meta
            .get("agent")
            .and_then(|v| v.as_str())
            .unwrap_or("unknown")
            .to_string();
        let outcome = meta.get("outcome").and_then(|v| v.as_str()).unwrap_or("");
        let entry = agent_stats.entry(agent).or_insert((0, 0));
        entry.1 += 1;
        if outcome == "success" {
            entry.0 += 1;
        }
    }

    let mut rankings: Vec<serde_json::Value> = agent_stats
        .iter()
        .map(|(agent, (success, total))| {
            let rate = if *total > 0 {
                (*success as f64) / (*total as f64)
            } else {
                0.0
            };
            json!({
                "agent": agent,
                "success": success,
                "total": total,
                "rate": (rate * 100.0).round() / 100.0,
            })
        })
        .collect();
    rankings.sort_by(|a, b| {
        b.get("rate")
            .and_then(|v| v.as_f64())
            .unwrap_or(0.0)
            .partial_cmp(&a.get("rate").and_then(|v| v.as_f64()).unwrap_or(0.0))
            .unwrap_or(std::cmp::Ordering::Equal)
    });

    let recommended = rankings
        .first()
        .and_then(|r| r.get("agent"))
        .and_then(|v| v.as_str())
        .unwrap_or("claude");

    json!({
        "available": true,
        "eval_count": eval_rows.len(),
        "agent_rankings": rankings,
        "recommended_agent": recommended,
    })
}
