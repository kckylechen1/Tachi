use super::super::*;

pub(super) fn feature_run_artifacts(flow_id: Option<&str>) -> Result<Vec<Value>, String> {
    let Some(flow_id) = flow_id.filter(|id| !id.trim().is_empty()) else {
        return Ok(Vec::new());
    };
    let run_dir = crate::shell_ops::run_dir_for_flow_id(flow_id)?;
    let mut out = vec![json!({
        "kind": "run_dir",
        "path": run_dir.to_string_lossy(),
        "exists": run_dir.exists(),
        "layer": "runtime_artifact",
        "authority": "runtime_state",
    })];
    for name in [
        "instruction.md",
        "plan.md",
        "result.md",
        "validation.md",
        "status.json",
        "close_loop.json",
        "events.jsonl",
        "progress.jsonl",
        "trajectory.jsonl",
    ] {
        let path = run_dir.join(name);
        out.push(json!({
            "kind": "run_artifact",
            "path": path.to_string_lossy(),
            "exists": path.exists(),
            "layer": "runtime_artifact",
            "authority": "runtime_state",
        }));
    }
    Ok(out)
}

pub(super) async fn feature_board(
    server: &MemoryServer,
    params: &TachiTaskParams,
    top_k: usize,
) -> serde_json::Value {
    let raw = feature_board_raw(server, params, params.flow_id.clone(), top_k).await;
    let mut used_fallback = false;
    let mut board: Value = match raw {
        Ok(raw) => serde_json::from_str(&raw).unwrap_or_else(|_| json!({})),
        Err(_) => return json!({"available": false}),
    };
    if params
        .flow_id
        .as_deref()
        .is_some_and(|id| !id.trim().is_empty())
        && board
            .get("tasks")
            .and_then(Value::as_array)
            .is_some_and(Vec::is_empty)
    {
        if let Ok(raw) = feature_board_raw(server, params, None, top_k).await {
            board = serde_json::from_str(&raw).unwrap_or_else(|_| json!({}));
            used_fallback = true;
        }
    }
    let Some(tasks) = board.get("tasks").and_then(Value::as_array).cloned() else {
        return board;
    };
    let needles = feature_needles(params);
    if needles.is_empty() {
        board["tasks"] = Value::Array(tasks.into_iter().take(top_k).collect());
        board["count"] = json!(board["tasks"].as_array().map(Vec::len).unwrap_or(0));
        return board;
    }
    let filtered = tasks
        .into_iter()
        .filter(|task| value_contains_any(task, &needles))
        .take(top_k)
        .collect::<Vec<_>>();
    board["tasks"] = Value::Array(filtered);
    board["count"] = json!(board["tasks"].as_array().map(Vec::len).unwrap_or(0));
    if used_fallback {
        board["flow_id"] = json!(params.flow_id);
        board["flow_filter_fallback"] = json!("needle_scan");
    }
    board
}

pub(super) async fn feature_board_raw(
    server: &MemoryServer,
    params: &TachiTaskParams,
    flow_id: Option<String>,
    top_k: usize,
) -> Result<String, String> {
    crate::dispatch_ops::handle_tachi_board(
        server,
        TachiBoardParams {
            state_filter: Some("all".to_string()),
            limit: Some(top_k.max(10)),
            project: params.project.clone(),
            flow_id,
            // tachi#1173 item 3: the default board view now folds terminal
            // rows into count rows on an unfiltered ("all") query. This
            // consumer needle-scans historical (including completed) rows
            // for flow_id/issue_ref/pr_ref matches (see `feature_needles` /
            // `value_contains_any` below) — folding those away would silently
            // drop matches, so this internal caller opts back into the full,
            // unfolded row set rather than the new agent-facing default.
            verbose: Some(true),
        },
    )
    .await
}

pub(super) fn feature_needles(params: &TachiTaskParams) -> Vec<String> {
    [
        params.flow_id.as_deref(),
        params.issue_ref.as_deref(),
        params.pr_ref.as_deref(),
    ]
    .into_iter()
    .flatten()
    .map(str::trim)
    .filter(|value| !value.is_empty())
    .map(str::to_ascii_lowercase)
    .collect()
}

pub(in crate::copilot_ops) fn value_contains_any(value: &Value, needles: &[String]) -> bool {
    if needles.is_empty() {
        return true;
    }
    [
        "dispatch_id",
        "summary",
        "run_dir",
        "eval_id",
        "agent",
        "state",
        "source",
    ]
    .into_iter()
    .filter_map(|field| value.get(field))
    .any(|field_value| field_value_contains_any(field_value, needles))
}

pub(super) fn field_value_contains_any(value: &Value, needles: &[String]) -> bool {
    match value {
        Value::String(text) => {
            let text = text.to_ascii_lowercase();
            needles.iter().any(|needle| text.contains(needle))
        }
        Value::Array(values) => values
            .iter()
            .any(|value| field_value_contains_any(value, needles)),
        Value::Object(map) => map
            .values()
            .any(|value| field_value_contains_any(value, needles)),
        _ => false,
    }
}
