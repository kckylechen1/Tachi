use std::path::Path;
use std::time::Duration;

use chrono::Utc;
use serde_json::json;
use tokio::io::AsyncReadExt;
use tokio::process::Command;

use super::super::dispatch::DispatchResult;
use super::super::dispatch_v2::append_trajectory_event;
use super::session::write_native_acp_session_record;
use super::{NativeAcpConnection, NativeAcpRunSpec, ACP_STREAM_FILE};

pub(in crate::dispatch_ops) async fn run_native_acp_dispatch(
    spec: NativeAcpRunSpec,
    run_dir: &Path,
    trajectory_path: &Path,
    dispatch_id: &str,
    agent: &str,
    timeout: Duration,
) -> Result<DispatchResult, String> {
    match tokio::time::timeout(
        timeout,
        run_native_acp_dispatch_inner(spec, run_dir, trajectory_path, dispatch_id, agent),
    )
    .await
    {
        Ok(result) => result,
        Err(_) => Err(format!(
            "Native ACP dispatch timed out after {}s (adapter killed)",
            timeout.as_secs()
        )),
    }
}

async fn run_native_acp_dispatch_inner(
    spec: NativeAcpRunSpec,
    run_dir: &Path,
    trajectory_path: &Path,
    dispatch_id: &str,
    agent: &str,
) -> Result<DispatchResult, String> {
    let mut cmd = Command::new(&spec.command);
    cmd.args(&spec.args)
        .current_dir(&spec.cwd)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true);
    for (name, value) in &spec.env {
        cmd.env(name, value);
    }

    let mut child = cmd
        .spawn()
        .map_err(|err| format!("Failed to spawn native ACP adapter: {err}"))?;
    let stdin = child
        .stdin
        .take()
        .ok_or_else(|| "Native ACP adapter stdin was not piped".to_string())?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| "Native ACP adapter stdout was not piped".to_string())?;
    let mut stderr = child
        .stderr
        .take()
        .ok_or_else(|| "Native ACP adapter stderr was not piped".to_string())?;
    let stderr_task = tokio::spawn(async move {
        let mut captured = String::new();
        let _ = stderr.read_to_string(&mut captured).await;
        captured
    });

    let mut connection = NativeAcpConnection::new(
        stdin,
        stdout,
        &spec,
        dispatch_id,
        agent,
        run_dir,
        trajectory_path,
    );
    let outcome = connection.run_prompt_turn(&spec).await;
    let close_result = connection.close_stdin().await;
    let stream_result = connection.persist_raw_stream(run_dir);

    let graceful_exit = tokio::time::timeout(Duration::from_millis(1500), child.wait()).await;
    let process_exit_code = match graceful_exit {
        Ok(Ok(status)) => status.code(),
        Ok(Err(err)) => {
            append_trajectory_event(
                trajectory_path,
                json!({
                    "event": "acp_native_process_wait_failed",
                    "dispatch_id": dispatch_id,
                    "agent": agent,
                    "error": err.to_string(),
                    "timestamp": Utc::now().to_rfc3339(),
                }),
            );
            None
        }
        Err(_) => {
            let _ = child.kill().await;
            let _ = child.wait().await;
            append_trajectory_event(
                trajectory_path,
                json!({
                    "event": "acp_native_process_killed_after_turn",
                    "dispatch_id": dispatch_id,
                    "agent": agent,
                    "reason": "adapter did not exit after stdin close",
                    "timestamp": Utc::now().to_rfc3339(),
                }),
            );
            None
        }
    };
    let stderr_output = stderr_task.await.unwrap_or_default();

    if let Err(err) = close_result {
        append_trajectory_event(
            trajectory_path,
            json!({
                "event": "acp_native_stdin_close_failed",
                "dispatch_id": dispatch_id,
                "agent": agent,
                "error": err,
                "timestamp": Utc::now().to_rfc3339(),
            }),
        );
    }
    let stream_path = match stream_result {
        Ok(path) => path,
        Err(err) => {
            append_trajectory_event(
                trajectory_path,
                json!({
                    "event": "acp_native_stream_persist_failed",
                    "dispatch_id": dispatch_id,
                    "agent": agent,
                    "error": err,
                    "timestamp": Utc::now().to_rfc3339(),
                }),
            );
            run_dir.join(ACP_STREAM_FILE)
        }
    };

    let outcome = match outcome {
        Ok(outcome) => outcome,
        Err(err) => {
            let stderr_tail = crate::dispatch_ops::subprocess::tail_chars(&stderr_output, 4000);
            let suffix = if stderr_tail.is_empty() {
                String::new()
            } else {
                format!("; adapter stderr: {stderr_tail}")
            };
            return Err(format!("{err}{suffix}"));
        }
    };

    if let Some(record_path) = spec.session_record_path.as_ref() {
        write_native_acp_session_record(
            &spec,
            record_path,
            spec.session_distill_path.as_ref(),
            &outcome,
            &stream_path,
            dispatch_id,
        )?;
    }

    append_trajectory_event(
        trajectory_path,
        json!({
            "event": "acp_native_turn_finished",
            "dispatch_id": dispatch_id,
            "agent": agent,
            "session_id": outcome.session_id,
            "agent_session_id": outcome.agent_session_id,
            "used_existing_session": outcome.used_existing_session,
            "mapped_events": outcome.mapped_events,
            "raw_stream": stream_path.to_string_lossy(),
            "process_exit_code": process_exit_code,
            "timestamp": Utc::now().to_rfc3339(),
        }),
    );

    Ok(DispatchResult {
        output: outcome.output,
        worker_pid: child.id(),
        exit_code: Some(0),
        observed_model: outcome.observed_model,
    })
}
