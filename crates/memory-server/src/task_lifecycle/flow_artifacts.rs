use super::*;

pub(crate) fn mark_task_close_loop(flow_id: &str, raw_result: &str) -> Result<(), String> {
    let run_dir = run_dir_for_flow_id(flow_id)?;
    let close_loop_path = run_dir.join("close_loop.json");
    let payload = serde_json::from_str::<Value>(raw_result).unwrap_or_else(|_| {
        json!({
            "action": "close_loop",
            "raw_result": raw_result,
        })
    });
    write_json_atomic(&close_loop_path, &payload)?;
    let path_string = close_loop_path.to_string_lossy().to_string();
    merge_flow_status(
        &run_dir,
        json!({
            "state": "closed_loop",
            "closed_at": Utc::now().to_rfc3339(),
            "close_loop_path": path_string,
            "artifacts": { "close_loop": path_string },
        }),
    )?;
    Ok(())
}

pub(crate) fn mark_task_dispatch(
    flow_id: &str,
    dispatch_id: &str,
    mut card: Value,
) -> Result<(), String> {
    if !is_safe_dispatch_marker_id(dispatch_id) {
        return Err(format!(
            "invalid dispatch_id for flow marker: {dispatch_id}"
        ));
    }
    let _guard = FLOW_MARKER_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let run_dir = run_dir_for_flow_id(flow_id)?;
    std::fs::create_dir_all(run_dir.join("artifacts"))
        .map_err(|e| format!("create dispatch artifact dir: {e}"))?;

    let recorded_at = Utc::now().to_rfc3339();
    if !card.is_object() {
        card = json!({ "details": card });
    }
    if let Some(obj) = card.as_object_mut() {
        obj.insert("flow_id".to_string(), json!(flow_id));
        obj.insert("dispatch_id".to_string(), json!(dispatch_id));
        obj.insert("recorded_at".to_string(), json!(recorded_at));
    }

    let card_path = run_dir
        .join("artifacts")
        .join(format!("dispatch-{dispatch_id}.json"));
    write_json_atomic(&card_path, &card)?;
    let card_path_string = card_path.to_string_lossy().to_string();

    let status_path = run_dir.join("status.json");
    let mut status = read_json_file(&status_path)?.unwrap_or_else(|| json!({}));
    if !status.is_object() {
        status = json!({});
    }
    let obj = status
        .as_object_mut()
        .ok_or_else(|| "flow status must be a JSON object".to_string())?;
    let dispatch_ids = obj
        .entry("dispatch_ids".to_string())
        .or_insert_with(|| json!([]));
    if !dispatch_ids.is_array() {
        *dispatch_ids = json!([]);
    }
    if let Some(ids) = dispatch_ids.as_array_mut() {
        let already_present = ids.iter().any(|value| value.as_str() == Some(dispatch_id));
        if !already_present {
            ids.push(json!(dispatch_id));
        }
    }
    let dispatch_cards = obj
        .entry("dispatch_cards".to_string())
        .or_insert_with(|| json!([]));
    if !dispatch_cards.is_array() {
        *dispatch_cards = json!([]);
    }
    if let Some(cards) = dispatch_cards.as_array_mut() {
        let already_present = cards
            .iter()
            .any(|value| value.as_str() == Some(card_path_string.as_str()));
        if !already_present {
            cards.push(json!(card_path_string));
        }
    }
    let artifacts = obj
        .entry("artifacts".to_string())
        .or_insert_with(|| json!({}));
    if !artifacts.is_object() {
        *artifacts = json!({});
    }
    if let Some(artifact_obj) = artifacts.as_object_mut() {
        let dispatch_artifacts = artifact_obj
            .entry("dispatches".to_string())
            .or_insert_with(|| json!({}));
        if !dispatch_artifacts.is_object() {
            *dispatch_artifacts = json!({});
        }
        if let Some(dispatch_obj) = dispatch_artifacts.as_object_mut() {
            dispatch_obj.insert(dispatch_id.to_string(), json!(card_path_string));
        }
    }
    obj.insert("stage".to_string(), json!("dispatch"));
    obj.insert("state".to_string(), json!("dispatched"));
    obj.insert("last_dispatch_id".to_string(), json!(dispatch_id));
    obj.insert("updated_at".to_string(), json!(recorded_at.clone()));
    if obj.get("created_at").is_none() {
        obj.insert("created_at".to_string(), json!(recorded_at.clone()));
    }
    write_json_atomic(&status_path, &status)?;

    append_flow_event(
        &run_dir,
        json!({
            "event": "dispatch_linked",
            "flow_id": flow_id,
            "dispatch_id": dispatch_id,
            "dispatch_card": card_path_string,
            "timestamp": recorded_at,
        }),
    )?;
    Ok(())
}

pub(crate) fn mark_task_dispatch_completion(
    flow_id: &str,
    dispatch_id: &str,
    mut completion: Value,
) -> Result<Value, String> {
    if !is_safe_dispatch_marker_id(dispatch_id) {
        return Err(format!(
            "invalid dispatch_id for flow completion marker: {dispatch_id}"
        ));
    }
    let _guard = FLOW_MARKER_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let run_dir = run_dir_for_flow_id(flow_id)?;
    std::fs::create_dir_all(run_dir.join("artifacts"))
        .map_err(|e| format!("create dispatch artifact dir: {e}"))?;

    let completed_at = Utc::now().to_rfc3339();
    if !completion.is_object() {
        completion = json!({ "details": completion });
    }
    if let Some(obj) = completion.as_object_mut() {
        obj.insert("flow_id".to_string(), json!(flow_id));
        obj.insert("dispatch_id".to_string(), json!(dispatch_id));
        obj.insert("completed_at".to_string(), json!(completed_at.clone()));
    }

    let status_path = run_dir.join("status.json");
    let mut status = read_json_file(&status_path)?.unwrap_or_else(|| json!({}));
    if !status.is_object() {
        status = json!({});
    }

    let card_path = status
        .get("artifacts")
        .and_then(|artifacts| artifacts.get("dispatches"))
        .and_then(|dispatches| dispatches.get(dispatch_id))
        .and_then(Value::as_str)
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            run_dir
                .join("artifacts")
                .join(format!("dispatch-{dispatch_id}.json"))
        });
    let mut card = read_json_file(&card_path)?.unwrap_or_else(|| {
        json!({
            "flow_id": flow_id,
            "dispatch_id": dispatch_id,
        })
    });
    if !card.is_object() {
        card = json!({ "details": card });
    }
    if let Some(obj) = card.as_object_mut() {
        obj.insert("flow_id".to_string(), json!(flow_id));
        obj.insert("dispatch_id".to_string(), json!(dispatch_id));
        obj.insert("completion".to_string(), completion.clone());
        let history = obj
            .entry("completion_history".to_string())
            .or_insert_with(|| json!([]));
        if !history.is_array() {
            *history = json!([]);
        }
        if let Some(items) = history.as_array_mut() {
            let incoming_eval_id = completion
                .get("eval_memory_id")
                .and_then(Value::as_str)
                .map(str::to_string);
            let incoming_task_id = completion
                .get("task_id")
                .and_then(Value::as_str)
                .map(str::to_string);
            let already_present = items.iter().any(|item| {
                let item_eval_id = item.get("eval_memory_id").and_then(Value::as_str);
                let item_task_id = item.get("task_id").and_then(Value::as_str);
                incoming_eval_id
                    .as_deref()
                    .is_some_and(|id| item_eval_id == Some(id))
                    || incoming_task_id
                        .as_deref()
                        .is_some_and(|id| item_task_id == Some(id))
            });
            if !already_present {
                items.push(completion.clone());
            }
        }
    }
    write_json_atomic(&card_path, &card)?;
    let card_path_string = card_path.to_string_lossy().to_string();

    if !status.is_object() {
        status = json!({});
    }
    let obj = status
        .as_object_mut()
        .ok_or_else(|| "flow status must be a JSON object".to_string())?;
    let dispatch_ids = obj
        .entry("dispatch_ids".to_string())
        .or_insert_with(|| json!([]));
    if !dispatch_ids.is_array() {
        *dispatch_ids = json!([]);
    }
    if let Some(ids) = dispatch_ids.as_array_mut() {
        let already_present = ids.iter().any(|value| value.as_str() == Some(dispatch_id));
        if !already_present {
            ids.push(json!(dispatch_id));
        }
    }
    let completed_ids = obj
        .entry("completed_dispatch_ids".to_string())
        .or_insert_with(|| json!([]));
    if !completed_ids.is_array() {
        *completed_ids = json!([]);
    }
    if let Some(ids) = completed_ids.as_array_mut() {
        let already_present = ids.iter().any(|value| value.as_str() == Some(dispatch_id));
        if !already_present {
            ids.push(json!(dispatch_id));
        }
    }
    let artifacts = obj
        .entry("artifacts".to_string())
        .or_insert_with(|| json!({}));
    if !artifacts.is_object() {
        *artifacts = json!({});
    }
    if let Some(artifact_obj) = artifacts.as_object_mut() {
        let dispatch_artifacts = artifact_obj
            .entry("dispatches".to_string())
            .or_insert_with(|| json!({}));
        if !dispatch_artifacts.is_object() {
            *dispatch_artifacts = json!({});
        }
        if let Some(dispatch_obj) = dispatch_artifacts.as_object_mut() {
            dispatch_obj.insert(dispatch_id.to_string(), json!(card_path_string.clone()));
        }
        let completion_artifacts = artifact_obj
            .entry("dispatch_completions".to_string())
            .or_insert_with(|| json!({}));
        if !completion_artifacts.is_object() {
            *completion_artifacts = json!({});
        }
        if let Some(completion_obj) = completion_artifacts.as_object_mut() {
            completion_obj.insert(dispatch_id.to_string(), completion.clone());
        }
    }
    let dispatch_eval = obj
        .entry("dispatch_eval".to_string())
        .or_insert_with(|| json!({}));
    if !dispatch_eval.is_object() {
        *dispatch_eval = json!({});
    }
    if let Some(eval_obj) = dispatch_eval.as_object_mut() {
        eval_obj.insert(dispatch_id.to_string(), completion.clone());
    }
    let current_stage = obj.get("stage").and_then(Value::as_str);
    if current_stage.is_none() || current_stage == Some("dispatch") {
        obj.insert("stage".to_string(), json!("eval"));
    }
    let current_state = obj.get("state").and_then(Value::as_str);
    if current_state.is_none() || current_state == Some("dispatched") {
        obj.insert("state".to_string(), json!("dispatch_completed"));
    }
    obj.insert("last_completed_dispatch_id".to_string(), json!(dispatch_id));
    obj.insert(
        "last_dispatch_completion_at".to_string(),
        json!(completed_at.clone()),
    );
    obj.insert("updated_at".to_string(), json!(completed_at.clone()));
    write_json_atomic(&status_path, &status)?;

    append_flow_event(
        &run_dir,
        json!({
            "event": "dispatch_completed",
            "flow_id": flow_id,
            "dispatch_id": dispatch_id,
            "dispatch_card": card_path_string,
            "completion": completion,
            "timestamp": completed_at,
        }),
    )?;

    Ok(json!({
        "recorded": true,
        "flow_id": flow_id,
        "dispatch_id": dispatch_id,
        "dispatch_card": card_path.to_string_lossy().to_string(),
    }))
}

pub(super) fn is_safe_dispatch_marker_id(dispatch_id: &str) -> bool {
    !dispatch_id.trim().is_empty()
        && dispatch_id
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '.'))
}

pub(crate) fn write_intake_flow_artifacts(
    flow_id: &str,
    objective: &str,
    issue: &IssueSnapshot,
    automation_plan: &Value,
) -> Result<(), String> {
    let run_dir = run_dir_for_flow_id(flow_id)?;
    std::fs::create_dir_all(run_dir.join("artifacts"))
        .map_err(|e| format!("create intake run dir: {e}"))?;
    let now = Utc::now().to_rfc3339();
    let pr_handoff_path = run_dir.join("pr_handoff.md");
    let pr_handoff_path_string = pr_handoff_path.to_string_lossy().to_string();
    let pr_handoff = build_pr_handoff_body(
        flow_id,
        objective,
        Some(&format!("{}#{}", issue.repo, issue.number)),
        &json!({
            "task": objective,
            "issue_ref": format!("{}#{}", issue.repo, issue.number),
            "automation_plan": automation_plan,
        }),
        None,
        &string_array_field(automation_plan, "leader_gate_reasons"),
    );
    write_text_atomic(&pr_handoff_path, &pr_handoff)?;
    merge_flow_status(
        &run_dir,
        json!({
            "flow_id": flow_id,
            "stage": "intake",
            "state": "flow_bound",
            "task": objective,
            "issue_ref": format!("{}#{}", issue.repo, issue.number),
            "doc_paths": issue.doc_paths,
            "spec_paths": issue.spec_paths,
            "automation_plan": automation_plan,
            "branch": automation_plan.get("branch").and_then(Value::as_str),
            "pr_title": automation_plan.get("pr_title").and_then(Value::as_str),
            "pr_handoff_path": pr_handoff_path_string.clone(),
            "artifacts": { "pr_handoff": pr_handoff_path_string },
            "dispatch_ids": [],
            "updated_at": now,
        }),
    )?;
    merge_github_status(
        &run_dir,
        json!({
            "repo": issue.repo,
            "issue_number": issue.number,
            "issue_url": issue.url,
            "issue_ref": format!("{}#{}", issue.repo, issue.number),
            "issue_title": issue.title,
            "issue_state": issue.state,
            "doc_paths": issue.doc_paths,
            "spec_paths": issue.spec_paths,
            "labels": issue.labels,
            "automation_plan": automation_plan,
        }),
    )?;
    write_intake_instruction(&run_dir, flow_id, objective, issue, automation_plan)?;
    append_github_event(
        &run_dir,
        flow_id,
        "github_issue_linked",
        json!({
            "repo": issue.repo,
            "issue_number": issue.number,
            "issue_url": issue.url,
            "title": issue.title,
            "state": issue.state,
            "doc_paths": issue.doc_paths,
            "spec_paths": issue.spec_paths,
            "automation_plan": automation_plan,
        }),
    )?;
    Ok(())
}

pub(crate) fn write_link_pr_artifacts(
    flow_id: &str,
    pr: &PrSnapshot,
    issue_ref: Option<&str>,
) -> Result<(), String> {
    let run_dir = run_dir_for_flow_id(flow_id)?;
    std::fs::create_dir_all(run_dir.join("artifacts"))
        .map_err(|e| format!("create link_pr run dir: {e}"))?;
    let patch = json!({
        "repo": pr.repo,
        "pr_number": pr.number,
        "pr_url": pr.url,
        "pr_ref": format!("{}#{}", pr.repo, pr.number),
        "pr_title": pr.title,
        "pr_state": pr.state,
        "head_ref": pr.head_ref,
        "base_ref": pr.base_ref,
        "review": { "state": pr.review_decision, "updated_at": Utc::now().to_rfc3339() },
        "mergeable": pr.mergeable,
        "merge_state": initial_merge_state_for_pr(pr.state.as_deref()),
    });
    merge_github_status(&run_dir, patch.clone())?;
    let mut flow_patch = json!({
        "flow_id": flow_id,
        "stage": "review",
        "state": "pr_linked",
        "pr_ref": format!("{}#{}", pr.repo, pr.number),
        "updated_at": Utc::now().to_rfc3339(),
    });
    if let (Some(obj), Some(issue_ref)) = (flow_patch.as_object_mut(), issue_ref) {
        obj.insert("issue_ref".to_string(), json!(issue_ref));
    }
    merge_flow_status(&run_dir, flow_patch)?;
    append_github_event(
        &run_dir,
        flow_id,
        "github_pr_updated",
        json!({
            "repo": pr.repo,
            "pr_number": pr.number,
            "pr_url": pr.url,
            "issue_ref": issue_ref,
            "state": pr.state,
            "review_decision": pr.review_decision,
            "mergeable": pr.mergeable,
        }),
    )?;
    Ok(())
}

pub(crate) fn flow_status_doc_refs(
    flow_id: Option<&str>,
) -> Result<(Vec<String>, Vec<String>), String> {
    let Some(flow_id) = flow_id.filter(|id| !id.trim().is_empty()) else {
        return Ok((Vec::new(), Vec::new()));
    };
    let run_dir = run_dir_for_flow_id(flow_id)?;
    let status = read_json_file(&run_dir.join("status.json"))?.unwrap_or_else(|| json!({}));
    let mut docs = string_array_field(&status, "doc_paths");
    let mut specs = string_array_field(&status, "spec_paths");
    if let Some(github) = status.get("github") {
        docs.extend(string_array_field(github, "doc_paths"));
        specs.extend(string_array_field(github, "spec_paths"));
    }
    dedupe_strings(&mut docs);
    dedupe_strings(&mut specs);
    Ok((docs, specs))
}

pub(super) fn intake_briefing_params(
    params: &TachiTaskParams,
    flow_id: &str,
    objective: &str,
    issue: &IssueSnapshot,
) -> TachiTaskParams {
    let mut briefing = params.clone();
    briefing.action = "briefing".to_string();
    briefing.format = Some("json".to_string());
    briefing.flow_id = Some(flow_id.to_string());
    briefing.issue_ref = Some(format!("{}#{}", issue.repo, issue.number));
    briefing.task = Some(objective.to_string());
    briefing.doc_paths = issue.doc_paths.clone();
    briefing.spec_paths = issue.spec_paths.clone();
    briefing
}

pub(crate) fn resolve_link_pr_issue_ref(
    flow_id: &str,
    supplied: Option<&str>,
) -> Result<Option<String>, String> {
    let existing = existing_flow_issue_ref(flow_id)?;
    let supplied = supplied
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(normalize_issue_ref)
        .transpose()?;
    if let (Some(existing), Some(supplied)) = (existing.as_deref(), supplied.as_deref()) {
        if existing != supplied {
            return Err(format!(
                "link_pr issue_ref mismatch: flow has '{existing}', request supplied '{supplied}'"
            ));
        }
    }
    Ok(supplied.or(existing))
}

pub(super) fn existing_flow_issue_ref(flow_id: &str) -> Result<Option<String>, String> {
    let run_dir = run_dir_for_flow_id(flow_id)?;
    let status = read_json_file(&run_dir.join("status.json"))?.unwrap_or_else(|| json!({}));
    let from_top = status
        .get("issue_ref")
        .and_then(Value::as_str)
        .map(str::to_string);
    if from_top.is_some() {
        return Ok(from_top);
    }
    Ok(status
        .get("github")
        .and_then(|github| github.get("issue_ref"))
        .and_then(Value::as_str)
        .map(str::to_string))
}

pub(super) fn normalize_issue_ref(raw: &str) -> Result<String, String> {
    parse_issue_ref(raw, None)
        .map(|target| format!("{}#{}", target.repo, target.number))
        .ok_or_else(|| {
            "issue_ref must be owner/repo#123 or a GitHub issue URL for link_pr".to_string()
        })
}

pub(super) fn initial_merge_state_for_pr(state: Option<&str>) -> &'static str {
    match state.unwrap_or("").to_ascii_uppercase().as_str() {
        "MERGED" => "merged",
        "CLOSED" => "blocked",
        _ => "pending",
    }
}
