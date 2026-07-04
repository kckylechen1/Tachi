use super::*;

pub(super) async fn handle_tachi_task_wait(
    server: &MemoryServer,
    params: &TachiTaskParams,
) -> Result<String, String> {
    let dispatch_id = params
        .dispatch_id
        .as_deref()
        .map(str::trim)
        .filter(|id| !id.is_empty())
        .ok_or_else(|| "dispatch_id is required when action='wait'".to_string())?
        .to_string();
    let timeout = StdDuration::from_secs(params.timeout_secs.unwrap_or(600).min(86_400));
    let deadline = Instant::now() + timeout;
    let mut last_task = None;
    let mut poll_delay = TASK_WAIT_INITIAL_POLL_DELAY;

    loop {
        let task = crate::dispatch_ops::collect_run_task_for_server(server, &dispatch_id);
        if let Some(task) = task {
            let state = task
                .get("state")
                .and_then(Value::as_str)
                .unwrap_or("unknown");
            let terminal = is_terminal_task_state(state);
            if terminal {
                return serde_json::to_string(&json!({
                    "status": "completed",
                    "dispatch_id": dispatch_id,
                    "terminal": true,
                    "state": state,
                    "task": task,
                }))
                .map_err(|e| format!("serialize wait response: {e}"));
            }
            last_task = Some(task);
        }

        if Instant::now() >= deadline {
            let state = last_task
                .as_ref()
                .and_then(|task| task.get("state"))
                .and_then(Value::as_str)
                .unwrap_or("not_found");
            return serde_json::to_string(&json!({
                "status": "timeout",
                "dispatch_id": dispatch_id,
                "terminal": false,
                "state": state,
                "task": last_task,
            }))
            .map_err(|e| format!("serialize wait timeout response: {e}"));
        }

        let remaining = deadline.saturating_duration_since(Instant::now());
        tokio::time::sleep(poll_delay.min(remaining)).await;
        poll_delay = next_task_wait_poll_delay(poll_delay);
    }
}

pub(super) fn read_dispatch_status_for_task(
    server: &MemoryServer,
    params: &TachiTaskParams,
    action: &str,
) -> Result<(String, Value, Value, PathBuf), String> {
    let dispatch_id = params
        .dispatch_id
        .as_deref()
        .map(str::trim)
        .filter(|id| !id.is_empty())
        .ok_or_else(|| format!("dispatch_id is required when action='{action}'"))?
        .to_string();
    let task = crate::dispatch_ops::collect_run_task_for_server(server, &dispatch_id)
        .ok_or_else(|| format!("dispatch '{dispatch_id}' was not found in the run ledger"))?;
    let run_dir = task
        .get("run_dir")
        .and_then(Value::as_str)
        .filter(|path| !path.is_empty())
        .map(PathBuf::from)
        .ok_or_else(|| format!("dispatch '{dispatch_id}' has no readable run_dir"))?;
    let status_path = run_dir.join("status.json");
    let status = crate::task_lifecycle::read_json_file(&status_path)?
        .ok_or_else(|| format!("dispatch '{dispatch_id}' has no status.json"))?;
    Ok((dispatch_id, task, status, run_dir))
}

pub(super) async fn handle_tachi_task_status(
    server: &MemoryServer,
    params: &TachiTaskParams,
) -> Result<String, String> {
    let (dispatch_id, task, status, run_dir) =
        read_dispatch_status_for_task(server, params, "status")?;
    let state = task
        .get("state")
        .and_then(Value::as_str)
        .unwrap_or("unknown");
    let mut response = json!({
        "status": "ok",
        "dispatch_id": dispatch_id,
        "terminal": is_terminal_task_state(state),
        "state": state,
        "task": task,
        "run_status": status,
    });
    if response["run_status"]
        .get("execution_backend")
        .and_then(Value::as_str)
        == Some("acpx")
    {
        let timeout = StdDuration::from_secs(params.timeout_secs.unwrap_or(30).min(300));
        match crate::dispatch_ops::run_acpx_control_from_status(
            &run_dir,
            &response["run_status"],
            "status",
            timeout,
        )
        .await
        {
            Ok(acpx_status) => response["acpx_status"] = acpx_status,
            Err(err) => response["acpx_status_error"] = json!(err),
        }
    }
    serde_json::to_string(&response).map_err(|e| format!("serialize status response: {e}"))
}

pub(super) async fn handle_tachi_task_cancel(
    server: &MemoryServer,
    params: &TachiTaskParams,
) -> Result<String, String> {
    let (dispatch_id, task, status, run_dir) =
        read_dispatch_status_for_task(server, params, "cancel")?;
    let state = task
        .get("state")
        .and_then(Value::as_str)
        .unwrap_or("unknown");
    if is_terminal_task_state(state) {
        return serde_json::to_string(&json!({
            "status": "already_terminal",
            "dispatch_id": dispatch_id,
            "terminal": true,
            "state": state,
            "task": task,
        }))
        .map_err(|e| format!("serialize cancel response: {e}"));
    }
    let timeout = StdDuration::from_secs(params.timeout_secs.unwrap_or(30).min(300));
    let acpx_cancel =
        crate::dispatch_ops::run_acpx_control_from_status(&run_dir, &status, "cancel", timeout)
            .await?;
    let success = acpx_cancel
        .get("success")
        .and_then(Value::as_bool)
        .unwrap_or(false);

    let mut updated_status = status.clone();
    if let Some(obj) = updated_status.as_object_mut() {
        obj.insert("updated_at".to_string(), json!(Utc::now().to_rfc3339()));
        obj.insert("acpx_cancel".to_string(), acpx_cancel.clone());
        if success {
            obj.insert("cancel_requested".to_string(), json!(true));
            obj.insert(
                "cancel_requested_at".to_string(),
                json!(Utc::now().to_rfc3339()),
            );
        }
    }
    let status_path = run_dir.join("status.json");
    let body = serde_json::to_vec_pretty(&updated_status)
        .map_err(|e| format!("serialize updated dispatch status: {e}"))?;
    crate::utils::write_owner_only_file_atomic(&status_path, &body)
        .map_err(|e| format!("write updated dispatch status: {e}"))?;

    serde_json::to_string(&json!({
        "status": if success { "cancel_requested" } else { "cancel_failed" },
        "dispatch_id": dispatch_id,
        "terminal": false,
        "state": state,
        "task": task,
        "acpx_cancel": acpx_cancel,
    }))
    .map_err(|e| format!("serialize cancel response: {e}"))
}

pub(super) fn next_task_wait_poll_delay(current: StdDuration) -> StdDuration {
    current.saturating_mul(2).min(TASK_WAIT_MAX_POLL_DELAY)
}

pub(super) fn is_terminal_task_state(state: &str) -> bool {
    matches!(
        state,
        "TASK_STATE_COMPLETED" | "TASK_STATE_FAILED" | "TASK_STATE_CANCELED"
    )
}

pub(crate) fn resolve_task_pr_status_target(
    params: &TachiTaskParams,
) -> Result<(String, u64), String> {
    crate::task_lifecycle::resolve_task_pr_target(params)
        .map(|target| (target.repo, target.number))
        .map_err(|_| {
            "pr_status requires either repo+number or pr_ref='owner/repo#123' / GitHub PR URL"
                .to_string()
        })
}

pub(crate) fn build_task_pr_status_gh_params(
    params: &TachiTaskParams,
) -> Result<TachiGhParams, String> {
    let (repo, number) = resolve_task_pr_status_target(params)?;
    Ok(TachiGhParams {
        action: "safe_merge".to_string(),
        repo: Some(repo),
        number: Some(number),
        dry_run: Some(true),
        confirm: false,
        flow_id: params.flow_id.clone(),
        merge_policy: params.merge_policy.clone(),
        allow_umbrella_close: params.allow_umbrella_close,
        ..Default::default()
    })
}
