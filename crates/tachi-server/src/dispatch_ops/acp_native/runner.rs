use std::path::Path;
use std::time::Duration;

use chrono::Utc;
use serde_json::json;
use tokio::io::AsyncReadExt;
use tokio::process::Command;

use super::super::dispatch::{DispatchResult, DispatchRunOutcome};
use super::super::dispatch_v2::append_trajectory_event;
use super::session::write_native_acp_session_record;
use super::{
    NativeAcpConnection, NativeAcpDeferredArtifacts, NativeAcpEventTarget, NativeAcpPromptOutcome,
    NativeAcpRunSpec, ACP_STREAM_FILE,
};

#[cfg(test)]
pub(in crate::dispatch_ops) async fn run_native_acp_dispatch(
    spec: NativeAcpRunSpec,
    run_dir: &Path,
    trajectory_path: &Path,
    dispatch_id: &str,
    agent: &str,
    timeout: Duration,
) -> Result<DispatchResult, String> {
    run_native_acp_dispatch_with_liveness(
        spec,
        run_dir,
        trajectory_path,
        dispatch_id,
        agent,
        timeout,
        false,
    )
    .await
    .result
}

pub(in crate::dispatch_ops) async fn run_native_acp_dispatch_with_liveness(
    spec: NativeAcpRunSpec,
    run_dir: &Path,
    trajectory_path: &Path,
    dispatch_id: &str,
    agent: &str,
    timeout: Duration,
    defer_artifacts: bool,
) -> DispatchRunOutcome {
    // The inner runner owns the timeout after it has spawned the adapter, so a
    // timeout cannot drop the future before the parent retains its process
    // group identity for postflight liveness probing.
    run_native_acp_dispatch_inner(
        spec,
        run_dir,
        trajectory_path,
        dispatch_id,
        agent,
        timeout,
        defer_artifacts,
    )
    .await
}

async fn run_native_acp_dispatch_inner(
    spec: NativeAcpRunSpec,
    run_dir: &Path,
    trajectory_path: &Path,
    dispatch_id: &str,
    agent: &str,
    timeout: Duration,
    defer_artifacts: bool,
) -> DispatchRunOutcome {
    let mut cmd = Command::new(&spec.command);
    cmd.args(&spec.args).current_dir(&spec.cwd);
    for name in &spec.env_remove {
        cmd.env_remove(name);
    }
    for (name, value) in &spec.env {
        cmd.env(name, value);
    }
    let escape_contained = defer_artifacts
        && crate::dispatch_ops::subprocess::configure_required_postflight_containment(&mut cmd);
    cmd.stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    #[cfg(unix)]
    cmd.kill_on_drop(false);
    #[cfg(not(unix))]
    cmd.kill_on_drop(true);
    crate::dispatch_ops::subprocess::configure_process_group(&mut cmd);

    let mut child = match cmd.spawn() {
        Ok(child) => child,
        Err(error) => {
            return DispatchRunOutcome::failure(
                format!("Failed to spawn native ACP adapter: {error}"),
                crate::exec_env_postflight::RunnerLivenessEvidence::NoWorkerSpawned,
            )
        }
    };
    let child_pid = child.id();
    #[cfg(unix)]
    let mut process_group =
        crate::dispatch_ops::subprocess::ManagedProcessGroupGuard::arm(child_pid);
    let stdin = child
        .stdin
        .take()
        .ok_or_else(|| "Native ACP adapter stdin was not piped".to_string());
    let stdin = match stdin {
        Ok(stdin) => stdin,
        Err(error) => {
            #[cfg(unix)]
            let (_, liveness) = crate::dispatch_ops::subprocess::terminate_reap_and_prove(
                &mut child,
                &mut process_group,
                escape_contained,
            )
            .await;
            #[cfg(not(unix))]
            let liveness =
                crate::dispatch_ops::subprocess::terminate_reap_uncontained_child(&mut child).await;
            return DispatchRunOutcome::failure(error, liveness);
        }
    };
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| "Native ACP adapter stdout was not piped".to_string());
    let stdout = match stdout {
        Ok(stdout) => stdout,
        Err(error) => {
            #[cfg(unix)]
            let (_, liveness) = crate::dispatch_ops::subprocess::terminate_reap_and_prove(
                &mut child,
                &mut process_group,
                escape_contained,
            )
            .await;
            #[cfg(not(unix))]
            let liveness =
                crate::dispatch_ops::subprocess::terminate_reap_uncontained_child(&mut child).await;
            return DispatchRunOutcome::failure(error, liveness);
        }
    };
    let mut stderr = match child
        .stderr
        .take()
        .ok_or_else(|| "Native ACP adapter stderr was not piped".to_string())
    {
        Ok(stderr) => stderr,
        Err(error) => {
            #[cfg(unix)]
            let (_, liveness) = crate::dispatch_ops::subprocess::terminate_reap_and_prove(
                &mut child,
                &mut process_group,
                escape_contained,
            )
            .await;
            #[cfg(not(unix))]
            let liveness =
                crate::dispatch_ops::subprocess::terminate_reap_uncontained_child(&mut child).await;
            return DispatchRunOutcome::failure(error, liveness);
        }
    };
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
        defer_artifacts,
    );
    let outcome = match tokio::time::timeout(timeout, connection.run_prompt_turn(&spec)).await {
        Ok(outcome) => outcome,
        Err(_) => {
            let _ = connection.close_stdin().await;
            #[cfg(unix)]
            let (_, liveness) = crate::dispatch_ops::subprocess::terminate_reap_and_prove(
                &mut child,
                &mut process_group,
                escape_contained,
            )
            .await;
            #[cfg(not(unix))]
            let liveness =
                crate::dispatch_ops::subprocess::terminate_reap_uncontained_child(&mut child).await;
            let _ = stderr_task.await;
            append_trajectory_event(
                trajectory_path,
                json!({
                    "event": "acp_native_dispatch_timed_out",
                    "dispatch_id": dispatch_id,
                    "agent": agent,
                    "timeout_secs": timeout.as_secs(),
                    "child_pid": child_pid,
                    "timestamp": Utc::now().to_rfc3339(),
                }),
            );
            return DispatchRunOutcome::failure(
                format!(
                    "Native ACP dispatch timed out after {}s (process group killed)",
                    timeout.as_secs()
                ),
                liveness,
            );
        }
    };
    let close_result = connection.close_stdin().await;

    #[cfg(unix)]
    let root_exit = crate::dispatch_ops::subprocess::wait_for_owned_root_exit(
        child_pid,
        Duration::from_millis(1500),
    )
    .await;
    #[cfg(unix)]
    let (status, liveness) = crate::dispatch_ops::subprocess::terminate_reap_and_prove(
        &mut child,
        &mut process_group,
        escape_contained,
    )
    .await;
    #[cfg(unix)]
    let (process_exit_code, process_exit_error) = match (root_exit, status) {
        (Ok(true), Ok(status)) => (status.code(), None),
        (Err(err), _) | (_, Err(err)) => {
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
            (None, Some(format!("Native ACP adapter wait failed: {err}")))
        }
        (Ok(false), Ok(_)) => {
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
            (
                None,
                Some("Native ACP adapter did not exit after stdin close".to_string()),
            )
        }
    };
    #[cfg(not(unix))]
    let (process_exit_code, process_exit_error, liveness) = {
        let graceful_exit = tokio::time::timeout(Duration::from_millis(1500), child.wait()).await;
        match graceful_exit {
            Ok(Ok(status)) => (
                status.code(),
                None,
                crate::exec_env_postflight::RunnerLivenessEvidence::indeterminate(
                    "native ACP root child was reaped; descendant containment is unavailable on this platform",
                ),
            ),
            Ok(Err(err)) => {
                let liveness =
                    crate::dispatch_ops::subprocess::terminate_reap_uncontained_child(&mut child)
                        .await;
                (
                    None,
                    Some(format!("Native ACP adapter wait failed: {err}")),
                    liveness,
                )
            }
            Err(_) => {
                let liveness =
                    crate::dispatch_ops::subprocess::terminate_reap_uncontained_child(&mut child)
                        .await;
                (
                    None,
                    Some("Native ACP adapter did not exit after stdin close".to_string()),
                    liveness,
                )
            }
        }
    };
    let stderr_output = stderr_task.await.unwrap_or_default();

    if let Some(error) = process_exit_error {
        return DispatchRunOutcome::failure(error, liveness);
    }

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
    let outcome = match outcome {
        Ok(outcome) => outcome,
        Err(err) => {
            let stderr_tail = crate::dispatch_ops::subprocess::tail_chars(&stderr_output, 4000);
            let suffix = if stderr_tail.is_empty() {
                String::new()
            } else {
                format!("; adapter stderr: {stderr_tail}")
            };
            return DispatchRunOutcome::failure(format!("{err}{suffix}"), liveness);
        }
    };

    let result = DispatchResult {
        output: outcome.output.clone(),
        exit_code: Some(0),
        observed_model: outcome.observed_model.clone(),
    };
    let deferred = NativeAcpDeferredArtifacts {
        spec,
        outcome,
        process_exit_code,
    };
    if defer_artifacts {
        DispatchRunOutcome {
            result: Ok(result),
            liveness,
            deferred_native_acp: Some(deferred),
        }
    } else if let Err(error) =
        publish_native_acp_artifacts(deferred, run_dir, trajectory_path, dispatch_id, agent, true)
    {
        DispatchRunOutcome::failure(error, liveness)
    } else {
        DispatchRunOutcome::success(result, liveness)
    }
}

pub(in crate::dispatch_ops) fn publish_native_acp_artifacts(
    deferred: NativeAcpDeferredArtifacts,
    run_dir: &Path,
    trajectory_path: &Path,
    dispatch_id: &str,
    agent: &str,
    publish_worker_mappings: bool,
) -> Result<(), String> {
    let NativeAcpDeferredArtifacts {
        spec,
        outcome,
        process_exit_code,
    } = deferred;
    let stream_path = run_dir.join(ACP_STREAM_FILE);

    // Required-postflight publication makes the raw stream its sole atomic
    // worker-authored artifact. Session continuity metadata is deliberately
    // stripped of prompt/output material and written first, so a later raw
    // stream failure cannot leave carrier output visible.
    if !publish_worker_mappings {
        if let Some(record_path) = spec.session_record_path.as_ref() {
            write_native_acp_session_record(
                &spec,
                record_path,
                spec.session_distill_path.as_ref(),
                &outcome,
                &stream_path,
                dispatch_id,
                false,
            )?;
        }
    }
    persist_raw_stream(&outcome, run_dir)?;
    if publish_worker_mappings {
        if let Some(record_path) = spec.session_record_path.as_ref() {
            write_native_acp_session_record(
                &spec,
                record_path,
                spec.session_distill_path.as_ref(),
                &outcome,
                &stream_path,
                dispatch_id,
                true,
            )?;
        }
    }
    for event in &outcome.staged_events {
        if publish_worker_mappings
            || matches!(event.target, NativeAcpEventTarget::PostflightReceipt)
        {
            let target = match event.target {
                NativeAcpEventTarget::Progress => run_dir.join("progress.jsonl"),
                NativeAcpEventTarget::Trajectory | NativeAcpEventTarget::PostflightReceipt => {
                    trajectory_path.to_path_buf()
                }
            };
            append_trajectory_event(&target, event.payload.clone());
        }
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
            "worker_mappings_published": publish_worker_mappings,
            "timestamp": Utc::now().to_rfc3339(),
        }),
    );
    Ok(())
}

fn persist_raw_stream(
    outcome: &NativeAcpPromptOutcome,
    run_dir: &Path,
) -> Result<std::path::PathBuf, String> {
    let stream_path = run_dir.join(ACP_STREAM_FILE);
    let lines = outcome
        .raw_messages
        .iter()
        .map(|message| {
            serde_json::to_string(message)
                .map_err(|err| format!("serialize ACP stream message: {err}"))
        })
        .collect::<Result<Vec<_>, _>>()?;
    let payload = if lines.is_empty() {
        String::new()
    } else {
        format!("{}\n", lines.join("\n"))
    };
    crate::utils::write_owner_only_file_atomic(&stream_path, payload.as_bytes())
        .map_err(|err| format!("write {ACP_STREAM_FILE}: {err}"))?;
    Ok(stream_path)
}
