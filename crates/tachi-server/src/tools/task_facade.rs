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
    let timeout = StdDuration::from_secs(
        params
            .timeout_secs
            .unwrap_or(TASK_WAIT_TIMEOUT_DEFAULT_SECS)
            .min(TASK_WAIT_TIMEOUT_CAP_SECS),
    );
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
            let terminal = is_terminal_task(&task);
            if terminal {
                // tachi#1173 item 7: on a terminal *failed* dispatch, attach
                // a bounded, ANSI-free failure_tail so the caller can
                // autopsy the failure from this response alone, without a
                // separate file read under ~/.tachi.
                let failure_tail = (state == "TASK_STATE_FAILED")
                    .then(|| task.get("run_dir").and_then(Value::as_str))
                    .flatten()
                    .and_then(|run_dir| {
                        crate::dispatch_ops::read_failure_tail(std::path::Path::new(run_dir))
                    });
                return serde_json::to_string(&json!({
                    "status": "completed",
                    "dispatch_id": dispatch_id,
                    "terminal": true,
                    "state": state,
                    "task": task,
                    "failure_tail": failure_tail,
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
        "terminal": is_terminal_task(&task),
        "state": state,
        "task": task,
        "run_status": status,
    });
    if response["run_status"]
        .get("execution_backend")
        .and_then(Value::as_str)
        == Some("acpx")
    {
        let timeout = StdDuration::from_secs(
            params
                .timeout_secs
                .unwrap_or(TASK_CONTROL_TIMEOUT_DEFAULT_SECS)
                .min(TASK_CONTROL_TIMEOUT_CAP_SECS),
        );
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
    // #878-C: when include_result is true, read the lane's result.md from the
    // run directory and include its size-capped content in the response. This
    // lets a leader whose FS access doesn't include ~/.tachi adjudicate the
    // lane report without local file access.
    if params.include_result {
        let result_path = run_dir.join("result.md");
        match std::fs::read_to_string(&result_path) {
            Ok(content) => {
                const MAX_RESULT_CHARS: usize = 8_000;
                let char_count = content.chars().count();
                let byte_count = content.len();
                let (body, truncated) = if char_count > MAX_RESULT_CHARS {
                    (
                        content.chars().take(MAX_RESULT_CHARS).collect::<String>(),
                        true,
                    )
                } else {
                    (content, false)
                };
                response["result"] = json!({
                    "body": body,
                    "truncated": truncated,
                    "full_size_chars": char_count,
                    "full_size_bytes": byte_count,
                });
            }
            Err(_) => {
                response["result"] = json!({
                    "body": null,
                    "note": "no result.md found in run directory (lane may not have produced a report)",
                });
            }
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

    // #1001 round 2 item 1: release the presence claim this dispatch
    // registered (auto_register_or_heartbeat_claim keys it on dispatch_id).
    // Fires unconditionally — including the already-terminal early-return
    // branch below — so a claim never stays `active` for a dispatch that is
    // being cancelled (or was already terminal but never released). Fail-safe
    // — degrades to a warn, never fails cancel.
    crate::claims_ops::release_claim_for_dispatch(server, &dispatch_id, "cancel");

    if is_terminal_task(&task) {
        return serde_json::to_string(&json!({
            "status": "already_terminal",
            "dispatch_id": dispatch_id,
            "terminal": true,
            "state": state,
            "task": task,
        }))
        .map_err(|e| format!("serialize cancel response: {e}"));
    }
    let timeout = StdDuration::from_secs(
        params
            .timeout_secs
            .unwrap_or(TASK_CONTROL_TIMEOUT_DEFAULT_SECS)
            .min(TASK_CONTROL_TIMEOUT_CAP_SECS),
    );
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

fn is_terminal_task(task: &Value) -> bool {
    let state = task
        .get("state")
        .and_then(Value::as_str)
        .unwrap_or("unknown");
    is_terminal_task_state(state)
        || (state == "TASK_STATE_INPUT_REQUIRED"
            && task.get("closure_kind").and_then(Value::as_str) == Some("partial"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use tachi_params::TachiTaskParams;

    fn make_server_with_runs_dir() -> (tempfile::TempDir, MemoryServer) {
        let tmp = tempfile::tempdir().expect("tempdir");
        let global_dir = tmp.path().join("global");
        std::fs::create_dir_all(&global_dir).expect("create global dir");
        let global_db = global_dir.join("memory.db");
        let server = MemoryServer::new(global_db, None).expect("server");
        (tmp, server)
    }

    fn write_fake_run(runs_dir: &std::path::Path, dispatch_id: &str, result_body: Option<&str>) {
        let run_dir = runs_dir.join(dispatch_id);
        std::fs::create_dir_all(&run_dir).expect("create run dir");
        let status = json!({
            "dispatch_id": dispatch_id,
            "agent": "claude",
            "state": "TASK_STATE_COMPLETED",
            "exit_code": 0,
            "updated_at": Utc::now().to_rfc3339(),
        });
        std::fs::write(run_dir.join("status.json"), status.to_string()).expect("write status.json");
        if let Some(body) = result_body {
            std::fs::write(run_dir.join("result.md"), body).expect("write result.md");
        }
    }

    fn status_params(dispatch_id: &str, include_result: bool) -> TachiTaskParams {
        let json_str = format!(
            r#"{{"action":"status","dispatch_id":"{}","include_result":{}}}"#,
            dispatch_id, include_result
        );
        serde_json::from_str(&json_str).expect("deserialize status params")
    }

    fn wait_params(dispatch_id: &str) -> TachiTaskParams {
        let json_str = format!(
            r#"{{"action":"wait","dispatch_id":"{}","timeout_secs":1}}"#,
            dispatch_id
        );
        serde_json::from_str(&json_str).expect("deserialize wait params")
    }

    fn cancel_params(dispatch_id: &str) -> TachiTaskParams {
        let json_str = format!(r#"{{"action":"cancel","dispatch_id":"{}"}}"#, dispatch_id);
        serde_json::from_str(&json_str).expect("deserialize cancel params")
    }

    fn write_input_required_run(
        runs_dir: &std::path::Path,
        dispatch_id: &str,
        closure_kind: Option<&str>,
    ) {
        let run_dir = runs_dir.join(dispatch_id);
        std::fs::create_dir_all(&run_dir).expect("create run dir");
        let mut status = json!({
            "dispatch_id": dispatch_id,
            "agent": "claude",
            "state": "TASK_STATE_INPUT_REQUIRED",
            "exit_code": 0,
            "updated_at": Utc::now().to_rfc3339(),
        });
        if let Some(closure_kind) = closure_kind {
            status["closure_kind"] = json!(closure_kind);
        }
        std::fs::write(run_dir.join("status.json"), status.to_string()).expect("write status.json");
    }

    /// tachi#1173 item 7: a terminal-FAILED run whose `progress.jsonl` carries
    /// a `subprocess_finished` event with an ANSI-colored `output_tail`.
    fn write_fake_failed_run(runs_dir: &std::path::Path, dispatch_id: &str, output_tail: &str) {
        let run_dir = runs_dir.join(dispatch_id);
        std::fs::create_dir_all(&run_dir).expect("create run dir");
        let status = json!({
            "dispatch_id": dispatch_id,
            "agent": "claude",
            "state": "TASK_STATE_FAILED",
            "exit_code": 1,
            "updated_at": Utc::now().to_rfc3339(),
        });
        std::fs::write(run_dir.join("status.json"), status.to_string()).expect("write status.json");
        let progress_line = json!({
            "event": "subprocess_finished",
            "dispatch_id": dispatch_id,
            "output_tail": output_tail,
        });
        std::fs::write(run_dir.join("progress.jsonl"), format!("{progress_line}\n"))
            .expect("write progress.jsonl");
    }

    #[tokio::test]
    async fn status_include_result_returns_result_md_content() {
        let (tmp, server) = make_server_with_runs_dir();
        let runs_dir = tmp.path().join("runs");
        let dispatch_id = "test-dispatch-with-result";
        write_fake_run(
            &runs_dir,
            dispatch_id,
            Some("# Verdict\n\nAll tests passed. OK."),
        );

        let params = status_params(dispatch_id, true);
        let response_str = handle_tachi_task_status(&server, &params)
            .await
            .expect("status call");
        let response: serde_json::Value =
            serde_json::from_str(&response_str).expect("parse response");

        assert_eq!(response["state"], "TASK_STATE_COMPLETED");
        assert!(
            response["result"]["body"]
                .as_str()
                .unwrap_or("")
                .contains("All tests passed"),
            "result body should contain the result.md content, got: {response}"
        );
        assert_eq!(response["result"]["truncated"], false);
    }

    #[tokio::test]
    async fn status_include_result_handles_missing_result_md() {
        let (tmp, server) = make_server_with_runs_dir();
        let runs_dir = tmp.path().join("runs");
        let dispatch_id = "test-dispatch-no-result";
        write_fake_run(&runs_dir, dispatch_id, None);

        let params = status_params(dispatch_id, true);
        let response_str = handle_tachi_task_status(&server, &params)
            .await
            .expect("status call");
        let response: serde_json::Value =
            serde_json::from_str(&response_str).expect("parse response");

        assert!(
            response["result"]["body"].is_null(),
            "missing result.md should produce null body, got: {response}"
        );
        assert!(
            response["result"]["note"]
                .as_str()
                .unwrap_or("")
                .contains("no result.md"),
            "should explain missing result.md, got: {response}"
        );
    }

    #[tokio::test]
    async fn status_without_include_result_omits_result_field() {
        let (tmp, server) = make_server_with_runs_dir();
        let runs_dir = tmp.path().join("runs");
        let dispatch_id = "test-dispatch-no-include";
        write_fake_run(&runs_dir, dispatch_id, Some("# Report\nVerdict: OK"));

        let params = status_params(dispatch_id, false);
        let response_str = handle_tachi_task_status(&server, &params)
            .await
            .expect("status call");
        let response: serde_json::Value =
            serde_json::from_str(&response_str).expect("parse response");

        assert!(
            response.get("result").is_none(),
            "result field should be absent when include_result=false, got: {response}"
        );
    }

    #[tokio::test]
    async fn status_include_result_multibyte_not_false_truncated() {
        // A result.md with multibyte (Chinese) chars: char count under cap,
        // but byte count over cap. Must NOT be marked truncated.
        let (tmp, server) = make_server_with_runs_dir();
        let runs_dir = tmp.path().join("runs");
        let dispatch_id = "test-dispatch-multibyte";
        // 100 Chinese chars = 300 bytes (UTF-8), well under 8000 char cap
        // but if the old byte-based check were used, a longer string would
        // falsely trigger truncated.
        let body = "测试结果。".repeat(100); // 600 chars, ~1800 bytes
        write_fake_run(&runs_dir, dispatch_id, Some(&body));

        let params = status_params(dispatch_id, true);
        let response_str = handle_tachi_task_status(&server, &params)
            .await
            .expect("status call");
        let response: serde_json::Value =
            serde_json::from_str(&response_str).expect("parse response");

        assert_eq!(
            response["result"]["truncated"], false,
            "multibyte content under char cap must not be falsely truncated, got: {response}"
        );
        assert_eq!(
            response["result"]["full_size_chars"], 500,
            "char count should be 500, got: {response}"
        );
    }

    /// tachi#1173 item 7 discriminator: a terminal-FAILED dispatch's `wait`
    /// response must carry `failure_tail` sourced from the run's
    /// `progress.jsonl` output_tail, with ANSI escapes stripped.
    #[tokio::test]
    async fn wait_terminal_failure_includes_ansi_free_failure_tail() {
        let (tmp, server) = make_server_with_runs_dir();
        let runs_dir = tmp.path().join("runs");
        let dispatch_id = "test-dispatch-failed-tail";
        write_fake_failed_run(
            &runs_dir,
            dispatch_id,
            "\u{1b}[31merror: build failed\u{1b}[0m",
        );

        let params = wait_params(dispatch_id);
        let response_str = handle_tachi_task_wait(&server, &params)
            .await
            .expect("wait call");
        let response: serde_json::Value =
            serde_json::from_str(&response_str).expect("parse response");

        assert_eq!(response["status"], "completed");
        assert_eq!(response["terminal"], true);
        assert_eq!(response["state"], "TASK_STATE_FAILED");
        let tail = response["failure_tail"].as_str().unwrap_or_else(|| {
            panic!("failure_tail should be present as a string, got: {response}")
        });
        assert!(
            tail.contains("error: build failed"),
            "failure_tail should carry the underlying message, got: {tail:?}"
        );
        assert!(
            !tail.contains('\u{1b}'),
            "failure_tail must not contain raw ANSI escape bytes: {tail:?}"
        );
    }

    /// A non-failed terminal state (completed) must not synthesize a
    /// failure_tail -- the field is present (stable response shape) but null.
    #[tokio::test]
    async fn wait_terminal_completion_has_null_failure_tail() {
        let (tmp, server) = make_server_with_runs_dir();
        let runs_dir = tmp.path().join("runs");
        let dispatch_id = "test-dispatch-completed-no-tail";
        write_fake_run(&runs_dir, dispatch_id, None);

        let params = wait_params(dispatch_id);
        let response_str = handle_tachi_task_wait(&server, &params)
            .await
            .expect("wait call");
        let response: serde_json::Value =
            serde_json::from_str(&response_str).expect("parse response");

        assert_eq!(response["state"], "TASK_STATE_COMPLETED");
        assert!(
            response["failure_tail"].is_null(),
            "completed dispatch should not carry a failure_tail, got: {response}"
        );
    }

    /// A partial verdict keeps its public INPUT_REQUIRED state, but its
    /// durable closure marker makes it terminal for every task-facade action.
    #[tokio::test]
    async fn partial_closed_input_required_is_terminal_across_task_facade() {
        let (tmp, server) = make_server_with_runs_dir();
        let runs_dir = tmp.path().join("runs");
        let dispatch_id = "test-partial-closed";
        write_input_required_run(&runs_dir, dispatch_id, Some("partial"));

        let status: Value = serde_json::from_str(
            &handle_tachi_task_status(&server, &status_params(dispatch_id, false))
                .await
                .expect("status call"),
        )
        .expect("parse status response");
        assert_eq!(status["state"], "TASK_STATE_INPUT_REQUIRED");
        assert_eq!(status["terminal"], true, "partial closure must be terminal");

        let wait: Value = serde_json::from_str(
            &handle_tachi_task_wait(&server, &wait_params(dispatch_id))
                .await
                .expect("wait call"),
        )
        .expect("parse wait response");
        assert_eq!(wait["status"], "completed");
        assert_eq!(wait["terminal"], true);
        assert_eq!(wait["state"], "TASK_STATE_INPUT_REQUIRED");

        let cancel: Value = serde_json::from_str(
            &handle_tachi_task_cancel(&server, &cancel_params(dispatch_id))
                .await
                .expect("cancel call"),
        )
        .expect("parse cancel response");
        assert_eq!(cancel["status"], "already_terminal");
        assert_eq!(cancel["terminal"], true);
        assert_eq!(cancel["state"], "TASK_STATE_INPUT_REQUIRED");
    }

    /// The same state without a closure marker is an ordinary plan-review
    /// request and must stay actionable rather than silently becoming closed.
    #[tokio::test]
    async fn ordinary_input_required_without_closure_marker_stays_active() {
        let (tmp, server) = make_server_with_runs_dir();
        let runs_dir = tmp.path().join("runs");
        let dispatch_id = "test-plan-input-required";
        write_input_required_run(&runs_dir, dispatch_id, None);

        let status: Value = serde_json::from_str(
            &handle_tachi_task_status(&server, &status_params(dispatch_id, false))
                .await
                .expect("status call"),
        )
        .expect("parse status response");
        assert_eq!(status["state"], "TASK_STATE_INPUT_REQUIRED");
        assert_eq!(status["terminal"], false, "plan input must stay active");
    }
}
