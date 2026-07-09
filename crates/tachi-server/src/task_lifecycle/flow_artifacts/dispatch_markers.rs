use super::*;

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

fn is_safe_dispatch_marker_id(dispatch_id: &str) -> bool {
    !dispatch_id.trim().is_empty()
        && dispatch_id
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '.'))
}
