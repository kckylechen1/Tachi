use super::*;

const TASK_STATUS_MAX_BYTES: usize = 1024 * 1024;
const TASK_RESULT_MAX_BYTES: usize = 64 * 1024;
const TASK_RESULT_RESPONSE_MAX_CHARS: usize = 8_000;

fn task_runs_root(run_dir: &std::path::Path) -> Result<&std::path::Path, String> {
    run_dir.parent().ok_or_else(|| {
        format!(
            "dispatch run directory {} has no runs root",
            run_dir.display()
        )
    })
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
    let task = crate::dispatch_ops::collect_run_task_for_server(server, &dispatch_id)?
        .ok_or_else(|| format!("dispatch '{dispatch_id}' was not found in the run ledger"))?;
    let run_dir = task
        .get("run_dir")
        .and_then(Value::as_str)
        .filter(|path| !path.is_empty())
        .map(PathBuf::from)
        .ok_or_else(|| format!("dispatch '{dispatch_id}' has no readable run_dir"))?;
    let status_path = run_dir.join("status.json");
    let status_raw = crate::dispatch_ops::read_text_file_within(
        task_runs_root(&run_dir)?,
        &status_path,
        TASK_STATUS_MAX_BYTES,
    )?
    .ok_or_else(|| format!("dispatch '{dispatch_id}' has no status.json"))?;
    let status = serde_json::from_str(&status_raw).map_err(|error| {
        format!(
            "dispatch status artifact {} is not valid JSON: {error}",
            status_path.display()
        )
    })?;
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
        match crate::dispatch_ops::read_text_file_within(
            task_runs_root(&run_dir)?,
            &result_path,
            TASK_RESULT_MAX_BYTES,
        )? {
            Some(content) => {
                let char_count = content.chars().count();
                let byte_count = content.len();
                let (body, truncated) = if char_count > TASK_RESULT_RESPONSE_MAX_CHARS {
                    (
                        content
                            .chars()
                            .take(TASK_RESULT_RESPONSE_MAX_CHARS)
                            .collect::<String>(),
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
            None => {
                response["result"] = json!({
                    "body": null,
                    "note": "no result.md found in run directory (lane may not have produced a report)",
                });
            }
        }
    }
    serde_json::to_string(&response).map_err(|e| format!("serialize status response: {e}"))
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
    // Sole consumer of `chrono::Utc` under `crate::tools`; imported here
    // instead of riding a `#[cfg(test)]` import in the parent `tools.rs`.
    use chrono::Utc;
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

    #[cfg(unix)]
    #[tokio::test]
    async fn status_include_result_loudly_refuses_outward_result_symlink() {
        let (tmp, server) = make_server_with_runs_dir();
        let runs_dir = tmp.path().join("runs");
        let dispatch_id = "test-dispatch-result-symlink";
        write_fake_run(&runs_dir, dispatch_id, None);
        let outside = tempfile::tempdir().expect("outside target");
        let outside_result = outside.path().join("result.md");
        std::fs::write(&outside_result, "outside result bytes").unwrap();
        std::os::unix::fs::symlink(
            &outside_result,
            runs_dir.join(dispatch_id).join("result.md"),
        )
        .unwrap();

        let error = handle_tachi_task_status(&server, &status_params(dispatch_id, true))
            .await
            .expect_err("result symlink refusal must reach the task caller");
        assert!(error.contains("refusing descriptor-bound read"), "{error}");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn status_loudly_refuses_outward_status_symlink() {
        let (tmp, server) = make_server_with_runs_dir();
        let runs_dir = tmp.path().join("runs");
        let dispatch_id = "test-dispatch-status-symlink";
        let run_dir = runs_dir.join(dispatch_id);
        std::fs::create_dir_all(&run_dir).unwrap();
        let outside = tempfile::tempdir().expect("outside target");
        let outside_status = outside.path().join("status.json");
        std::fs::write(
            &outside_status,
            json!({
                "dispatch_id": dispatch_id,
                "state": "TASK_STATE_COMPLETED",
                "updated_at": Utc::now().to_rfc3339(),
            })
            .to_string(),
        )
        .unwrap();
        std::os::unix::fs::symlink(&outside_status, run_dir.join("status.json")).unwrap();

        let error = handle_tachi_task_status(&server, &status_params(dispatch_id, false))
            .await
            .expect_err("status symlink refusal must reach the task caller");
        assert!(error.contains("refusing descriptor-bound read"), "{error}");
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

    #[tokio::test]
    async fn status_include_result_enforces_named_byte_limit_at_exact_boundary() {
        let (tmp, server) = make_server_with_runs_dir();
        let runs_dir = tmp.path().join("runs");
        let dispatch_id = "test-dispatch-result-byte-limit";
        write_fake_run(&runs_dir, dispatch_id, None);
        let result_path = runs_dir.join(dispatch_id).join("result.md");

        std::fs::write(&result_path, vec![b'x'; TASK_RESULT_MAX_BYTES]).unwrap();
        let exact = handle_tachi_task_status(&server, &status_params(dispatch_id, true))
            .await
            .expect("exact byte-limit result must remain readable");
        let exact: Value = serde_json::from_str(&exact).unwrap();
        assert_eq!(
            exact["result"]["full_size_bytes"],
            json!(TASK_RESULT_MAX_BYTES)
        );

        std::fs::write(&result_path, vec![b'x'; TASK_RESULT_MAX_BYTES + 1]).unwrap();
        let error = handle_tachi_task_status(&server, &status_params(dispatch_id, true))
            .await
            .expect_err("one byte over the task result limit must be loud");
        assert!(error.contains("named limit"), "{error}");
        assert!(
            error.contains(&format!("{} bytes", TASK_RESULT_MAX_BYTES)),
            "{error}"
        );
    }

    /// A partial verdict keeps its public INPUT_REQUIRED state, but its
    /// durable closure marker makes it terminal for the (now status-only)
    /// task-facade read path. Wait/cancel left Task in #1319-C2; status is
    /// the surviving unified work read model and must still honor the
    /// partial-closure terminal discriminator.
    #[tokio::test]
    async fn partial_closed_input_required_is_terminal_for_status() {
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
