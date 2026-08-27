use super::super::acp_native::{is_native_acp_transport, NativeAcpRunSpec};
use super::super::acpx::{is_acpx_transport, persist_acpx_events_and_map};
#[cfg(test)]
use super::super::dispatch_v2::stamp_route_decision_id;
#[cfg(test)]
use super::super::dispatch_v2::write_status_json;
use super::super::dispatch_v2::{
    append_trajectory_event, status_json_lock_for, write_status_json_for_terminal,
    ManagedTerminalStatusAnchor,
};
use super::super::kanban_helpers::{get_kanban_state, should_cleanup_run, update_kanban_state};
use super::super::subprocess::{
    run_agent_subprocess_with_liveness, run_managed_custom_subprocess_outcome,
    run_opencode_sop_subprocess_with_liveness, tail_chars,
};
use super::dedupe::release_flow_dispatch_slot;
use super::response_helpers::McpCleanup;
use crate::{MemoryServer, SaveMemoryParams};
use chrono::Utc;
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::time::Duration;
#[cfg(test)]
use std::{
    collections::HashSet,
    sync::{Arc, Barrier, Mutex, OnceLock},
};
use tachi_credential_profile::cleanup_ephemeral_credential_materializations;
use tachi_dispatch::{
    model_lineage_id, provider_model_parts, DispatchAcknowledgement, DispatchIdentityEffective,
    DispatchIdentityReceipt, UNKNOWN_IDENTITY,
};
use tokio::process::Command;

const WATCHDOG_STATUS_MAX_BYTES: usize = 1024 * 1024;

fn read_status_json_for_terminal(
    workspace_dir: &Path,
    status_path: &Path,
    managed_anchor: &ManagedTerminalStatusAnchor,
) -> Result<Option<Value>, String> {
    #[cfg(unix)]
    if let ManagedTerminalStatusAnchor::Anchored(anchor) = managed_anchor {
        let lock = anchor.lock();
        let _guard = lock.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        return anchor.read_json();
    }
    #[cfg(not(unix))]
    let _ = managed_anchor;

    let Some(raw) = crate::dispatch_ops::read_text_file_within(
        workspace_dir,
        status_path,
        WATCHDOG_STATUS_MAX_BYTES,
    )?
    else {
        return Ok(None);
    };
    serde_json::from_str::<Value>(&raw)
        .map(Some)
        .map_err(|error| {
            format!(
                "completion status artifact {} is not valid JSON: {error}",
                status_path.display()
            )
        })
}

fn managed_terminal_status_anchor(
    registry: &crate::managed_run_control::ManagedRunControlRegistry,
    dispatch_id: &str,
    managed_cancellation: Option<&crate::managed_run_control::ManagedCancelCommand>,
) -> ManagedTerminalStatusAnchor {
    #[cfg(unix)]
    {
        if let Some(command) = managed_cancellation {
            return ManagedTerminalStatusAnchor::Anchored(command.status_anchor.clone());
        }
        if let Some(anchor) = registry.accepted_status_anchor(dispatch_id) {
            return ManagedTerminalStatusAnchor::Anchored(anchor);
        }
    }
    #[cfg(not(unix))]
    {
        let _ = registry;
        let _ = dispatch_id;
        let _ = managed_cancellation;
    }
    ManagedTerminalStatusAnchor::Missing
}

pub(super) enum DispatchExecution {
    Subprocess(Command),
    ManagedCustom(
        Command,
        tokio::sync::mpsc::Receiver<crate::managed_run_control::ManagedCancelCommand>,
    ),
    NativeAcp(NativeAcpRunSpec),
}

/// Private lifecycle evidence that this registered Staff-managed run owns only
/// its own run-scoped ephemeral materializations. It is established before
/// background handoff and is independent of process exit or completion state.
pub(super) enum ManagedEphemeralCredentialCleanupObligation {
    Required,
}

pub(super) struct BackgroundDispatchContext {
    pub(super) server: MemoryServer,
    pub(super) dispatch_id: String,
    pub(super) agent: String,
    pub(super) stage: Option<String>,
    /// The dispatch's `TachiDispatchParams::project`, threaded through so a
    /// watchdog-recorded terminal outcome row lands in the same DB a
    /// `tachi_complete` for this dispatch would resolve to (scope symmetry,
    /// #774 round 2).
    pub(super) project: Option<String>,
    pub(super) trajectory_path: PathBuf,
    pub(super) workspace_dir: PathBuf,
    pub(super) v2: bool,
    pub(super) plan_generated_at: Option<String>,
    pub(super) plan_duration_ms: Option<u64>,
    pub(super) timeout_secs: u64,
    pub(super) timeout: Duration,
    pub(super) feedback_rules_trace: Value,
    pub(super) harness_transport: String,
    pub(super) harness_server_url: Option<String>,
    pub(super) host_adapter: Option<String>,
    pub(super) opencode_sop_label: Option<String>,
    pub(super) execution_backend_metadata: Option<Value>,
    pub(super) execution: DispatchExecution,
    pub(super) flow_dispatch_slot: Option<PathBuf>,
    pub(super) mcp_config_path: Option<PathBuf>,
    pub(super) managed_run_guard: Option<crate::managed_run_control::ManagedRunGuard>,
    pub(super) managed_ephemeral_credential_cleanup:
        Option<ManagedEphemeralCredentialCleanupObligation>,
    pub(super) postflight_gate: Option<crate::exec_env_postflight::PostflightGate>,
    pub(super) postflight_dispatch_lease: Option<crate::exec_env_ops::ExecEnvDispatchLeaseGuard>,
}

/// Covers an unwind before the ordinary background terminal path reaches its
/// explicit cleanup. It is declared after the managed authority guard so this
/// Drop releases credentials and the dispatch slot before authority vanishes.
struct BackgroundEarlyExitCleanup {
    server: MemoryServer,
    workspace_dir: PathBuf,
    flow_dispatch_slot: Option<PathBuf>,
    postflight_dispatch_lease: Option<crate::exec_env_ops::ExecEnvDispatchLeaseGuard>,
    armed: bool,
}

impl BackgroundEarlyExitCleanup {
    fn new(
        server: MemoryServer,
        workspace_dir: PathBuf,
        flow_dispatch_slot: Option<PathBuf>,
        postflight_dispatch_lease: Option<crate::exec_env_ops::ExecEnvDispatchLeaseGuard>,
    ) -> Self {
        Self {
            server,
            workspace_dir,
            flow_dispatch_slot,
            postflight_dispatch_lease,
            armed: true,
        }
    }

    fn complete(&mut self) {
        release_flow_dispatch_slot(self.flow_dispatch_slot.take());
        self.armed = false;
    }

    fn postflight_dispatch_lease_mut(
        &mut self,
    ) -> Option<&mut crate::exec_env_ops::ExecEnvDispatchLeaseGuard> {
        self.postflight_dispatch_lease.as_mut()
    }
}

impl Drop for BackgroundEarlyExitCleanup {
    fn drop(&mut self) {
        if !self.armed {
            return;
        }
        let _ = self.server.with_global_store(|store| {
            cleanup_ephemeral_credential_materializations(store, &self.workspace_dir, false)
        });
        release_flow_dispatch_slot(self.flow_dispatch_slot.take());
    }
}

#[cfg(test)]
static MANAGED_TIMEOUT_OVERRIDES: OnceLock<
    Mutex<std::collections::HashMap<(PathBuf, PathBuf), std::time::Duration>>,
> = OnceLock::new();

#[cfg(test)]
static MANAGED_CREDENTIAL_CLEANUP_FAILURES: OnceLock<Mutex<HashSet<(PathBuf, PathBuf)>>> =
    OnceLock::new();

#[cfg(test)]
pub(crate) struct ManagedCredentialCleanupFailureGuard {
    key: (PathBuf, PathBuf),
}

#[cfg(test)]
impl Drop for ManagedCredentialCleanupFailureGuard {
    fn drop(&mut self) {
        if let Some(failures) = MANAGED_CREDENTIAL_CLEANUP_FAILURES.get() {
            failures
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .remove(&self.key);
        }
    }
}

#[cfg(test)]
pub(crate) fn install_managed_credential_cleanup_failure(
    home: &std::path::Path,
    run_root: &std::path::Path,
) -> ManagedCredentialCleanupFailureGuard {
    let key = (home.to_path_buf(), run_root.to_path_buf());
    assert!(MANAGED_CREDENTIAL_CLEANUP_FAILURES
        .get_or_init(|| Mutex::new(HashSet::new()))
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .insert(key.clone()));
    ManagedCredentialCleanupFailureGuard { key }
}

#[cfg(test)]
fn managed_credential_cleanup_failure_injected() -> bool {
    let Some(home) = std::env::var_os("TACHI_HOME") else {
        return false;
    };
    let Some(run_root) = std::env::var_os("TACHI_RUN_ROOT") else {
        return false;
    };
    MANAGED_CREDENTIAL_CLEANUP_FAILURES
        .get()
        .is_some_and(|failures| {
            failures
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .contains(&(PathBuf::from(home), PathBuf::from(run_root)))
        })
}

#[cfg(test)]
pub(crate) struct ManagedTimeoutOverrideGuard {
    key: (PathBuf, PathBuf),
}

#[cfg(test)]
impl Drop for ManagedTimeoutOverrideGuard {
    fn drop(&mut self) {
        if let Some(overrides) = MANAGED_TIMEOUT_OVERRIDES.get() {
            overrides
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .remove(&self.key);
        }
    }
}

#[cfg(test)]
pub(crate) fn install_managed_timeout_override(
    home: &std::path::Path,
    run_root: &std::path::Path,
    timeout: std::time::Duration,
) -> ManagedTimeoutOverrideGuard {
    let key = (home.to_path_buf(), run_root.to_path_buf());
    assert!(MANAGED_TIMEOUT_OVERRIDES
        .get_or_init(|| Mutex::new(std::collections::HashMap::new()))
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .insert(key.clone(), timeout)
        .is_none());
    ManagedTimeoutOverrideGuard { key }
}

#[cfg(test)]
fn managed_timeout_override() -> Option<std::time::Duration> {
    let key = (
        PathBuf::from(std::env::var_os("TACHI_HOME")?),
        PathBuf::from(std::env::var_os("TACHI_RUN_ROOT")?),
    );
    MANAGED_TIMEOUT_OVERRIDES.get().and_then(|overrides| {
        overrides
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .get(&key)
            .copied()
    })
}

pub(super) fn spawn_background_dispatch(ctx: BackgroundDispatchContext) {
    let server_clone = ctx.server;
    let d_id = ctx.dispatch_id;
    let agent_for_watchdog = ctx.agent;
    let project_for_watchdog = ctx.project;
    let stage_for_traj = ctx.stage;
    let traj_path_for_spawn = ctx.trajectory_path;
    let workspace_dir = ctx.workspace_dir.clone();
    let workspace_dir_for_spawn = ctx.workspace_dir;
    let v2_for_spawn = ctx.v2;
    let plan_generated_at_for_spawn = ctx.plan_generated_at;
    let plan_duration_ms_for_spawn = ctx.plan_duration_ms;
    let (timeout_secs_for_spawn, timeout) = {
        #[cfg(test)]
        {
            managed_timeout_override()
                .map(|override_timeout| (override_timeout.as_secs().max(1), override_timeout))
                .unwrap_or((ctx.timeout_secs, ctx.timeout))
        }
        #[cfg(not(test))]
        {
            (ctx.timeout_secs, ctx.timeout)
        }
    };
    let feedback_rules_trace_for_spawn = ctx.feedback_rules_trace;
    let harness_transport_for_spawn = ctx.harness_transport;
    let harness_server_url_for_spawn = ctx.harness_server_url;
    let host_adapter_for_spawn = ctx.host_adapter;
    let opencode_sop_label_for_spawn = ctx.opencode_sop_label;
    let execution_backend_metadata_for_spawn = ctx.execution_backend_metadata;
    let execution_for_spawn = ctx.execution;
    let flow_dispatch_slot_for_spawn = ctx.flow_dispatch_slot;
    let mcp_config_path = ctx.mcp_config_path;
    let managed_run_guard = ctx.managed_run_guard;
    let managed_ephemeral_credential_cleanup = ctx.managed_ephemeral_credential_cleanup;
    let postflight_gate_for_spawn = ctx.postflight_gate;
    let postflight_dispatch_lease = ctx.postflight_dispatch_lease;

    tokio::task::spawn(async move {
        // Keep the registry entry and its sender alive for the entire
        // background lifecycle. Binding this outside the async move drops the
        // guard as soon as scheduling returns and makes the child observe a
        // closed cancellation receiver before it can spawn.
        let _managed_run_guard = managed_run_guard;
        let _mcp_cleanup = McpCleanup(mcp_config_path);
        let mut early_exit_cleanup = BackgroundEarlyExitCleanup::new(
            server_clone.clone(),
            workspace_dir_for_spawn.clone(),
            flow_dispatch_slot_for_spawn,
            postflight_dispatch_lease,
        );

        // execute_started — Stage 2 (or, in V1, the only stage).
        let execute_started_at = Utc::now();
        let execute_started_instant = std::time::Instant::now();
        append_trajectory_event(
            &traj_path_for_spawn,
            json!({
                "event": "execute_started",
                "dispatch_id": d_id,
                "agent": agent_for_watchdog,
                "stage": stage_for_traj,
                "v2": v2_for_spawn,
                "harness_transport": harness_transport_for_spawn.clone(),
                "harness_server_url": harness_server_url_for_spawn.clone(),
                "host_adapter": host_adapter_for_spawn.clone(),
                "timestamp": execute_started_at.to_rfc3339(),
            }),
        );

        let require_postflight_containment = postflight_gate_for_spawn.is_some();
        let (runner_outcome, mut managed_cancellation, managed_termination_proof) =
            match execution_for_spawn {
                DispatchExecution::Subprocess(cmd) if agent_for_watchdog == "opencode" => (
                    run_opencode_sop_subprocess_with_liveness(
                        cmd,
                        timeout,
                        opencode_sop_label_for_spawn
                            .as_deref()
                            .unwrap_or("opencode_sop"),
                        require_postflight_containment,
                    )
                    .await,
                    None,
                    None,
                ),
                DispatchExecution::Subprocess(cmd) => (
                    run_agent_subprocess_with_liveness(
                        cmd,
                        timeout,
                        require_postflight_containment,
                    )
                    .await,
                    None,
                    None,
                ),
                DispatchExecution::ManagedCustom(cmd, receiver) => {
                    let managed_run_dir = workspace_dir_for_spawn.clone();
                    let outcome = match tokio::spawn(async move {
                        run_managed_custom_subprocess_outcome(
                            cmd,
                            timeout,
                            receiver,
                            &managed_run_dir,
                            require_postflight_containment,
                        )
                        .await
                    })
                    .await
                    {
                        Ok(outcome) => outcome,
                        Err(error) => {
                            super::super::subprocess::ManagedSubprocessOutcome::indeterminate(
                                Err(format!("managed subprocess panicked: {error}")),
                                "managed runner task panicked after spawn state became unknown"
                                    .to_string(),
                            )
                        }
                    };
                    (
                        crate::dispatch_ops::dispatch::DispatchRunOutcome {
                            result: outcome.result,
                            liveness: outcome.liveness,
                            deferred_native_acp: None,
                        },
                        outcome.cancellation,
                        outcome.termination_proof,
                    )
                }
                DispatchExecution::NativeAcp(spec) => (
                    super::super::acp_native::run_native_acp_dispatch_with_liveness(
                        spec,
                        &workspace_dir_for_spawn,
                        &traj_path_for_spawn,
                        &d_id,
                        &agent_for_watchdog,
                        timeout,
                        require_postflight_containment,
                    )
                    .await,
                    None,
                    None,
                ),
            };
        let runner_liveness = runner_outcome.liveness;
        let mut pending_native_acp_artifacts = runner_outcome.deferred_native_acp;
        let result = runner_outcome.result;
        let execute_duration_ms = execute_started_instant.elapsed().as_millis() as u64;

        // CLI subprocesses intentionally never acknowledge a receipt: they
        // have no protocol field that reports an executed model. Native ACP is
        // the sole carrier with that evidence surface.
        if is_native_acp_transport(&harness_transport_for_spawn) {
            let observed_model = result
                .as_ref()
                .ok()
                .and_then(|outcome| outcome.observed_model.as_deref());
            if let Err(error) =
                persist_acp_model_acknowledgement(&workspace_dir_for_spawn, observed_model)
            {
                append_trajectory_event(
                    &traj_path_for_spawn,
                    json!({
                        "event": "acp_identity_acknowledgement_persist_failed",
                        "dispatch_id": d_id,
                        "agent": agent_for_watchdog,
                        "error": error,
                        "timestamp": Utc::now().to_rfc3339(),
                    }),
                );
            }
        }

        // Append subprocess_finished event to trajectory.jsonl
        let mut full_output = match &result {
            Ok(r) => r.output.clone(),
            Err(e) => e.clone(),
        };
        let mut acpx_event_summary_json: Option<serde_json::Value> = None;
        let mut pending_acpx_output: Option<String> = None;
        if is_acpx_transport(&harness_transport_for_spawn) {
            let release_now = postflight_gate_for_spawn.is_none();
            match persist_acpx_events_and_map(
                &workspace_dir_for_spawn,
                &traj_path_for_spawn,
                &d_id,
                &agent_for_watchdog,
                &full_output,
                release_now,
            ) {
                Ok(summary) => {
                    if let Some(final_response) = summary.final_response.clone() {
                        full_output = final_response;
                    }
                    if release_now {
                        acpx_event_summary_json = Some(json!({
                            "events_file": summary.events_file.to_string_lossy(),
                            "mapped_events": summary.mapped_events,
                            "final_response_extracted": summary.final_response.is_some(),
                        }));
                    } else {
                        pending_acpx_output = Some(match &result {
                            Ok(outcome) => outcome.output.clone(),
                            Err(error) => error.clone(),
                        });
                    }
                }
                Err(err) => {
                    append_trajectory_event(
                        &traj_path_for_spawn,
                        json!({
                            "event": "acpx_events_persist_failed",
                            "dispatch_id": d_id,
                            "agent": agent_for_watchdog.clone(),
                            "timestamp": Utc::now().to_rfc3339(),
                            "error": err,
                        }),
                    );
                }
            }
        }
        {
            let exit_code = match &result {
                Err(_) => None,
                Ok(r) => r.exit_code,
            };
            let output_tail = if postflight_gate_for_spawn.is_some() {
                "[withheld pending exec_env_postflight]".to_string()
            } else {
                match &result {
                    Err(e) => e.chars().take(200).collect::<String>(),
                    Ok(r) => tail_chars(&r.output, 500),
                }
            };
            let finished_event = json!({
                "event": "subprocess_finished",
                "dispatch_id": d_id,
                "agent": agent_for_watchdog,
                "stage": stage_for_traj,
                "exit_code": exit_code,
                "timestamp": Utc::now().to_rfc3339(),
                "output_tail": output_tail,
            });
            if let Ok(line) = serde_json::to_string(&finished_event) {
                use std::io::Write;
                if let Ok(mut f) = std::fs::OpenOptions::new()
                    .append(true)
                    .open(&traj_path_for_spawn)
                {
                    let _ = writeln!(f, "{}", line);
                }
                let progress_path = workspace_dir_for_spawn.join("progress.jsonl");
                if let Ok(mut f) = std::fs::OpenOptions::new()
                    .append(true)
                    .create(true)
                    .open(progress_path)
                {
                    let _ = writeln!(f, "{}", line);
                }
            }
        }

        // --- POSTFLIGHT GATE: verify write-contract and liveness (#894 S2e, #1322) ---
        let mut postflight_outcome = if let Some(gate) = &postflight_gate_for_spawn {
            // The runner owns termination and reap. Postflight receives typed
            // terminal evidence only; no numeric PID crosses this handoff and
            // this layer has no signalling capability.
            let outcome = gate.run(&runner_liveness);
            match outcome {
                Ok(mut outcome) => {
                    let quarantine_sink = crate::exec_env_postflight::DaemonQuarantineSink {
                        server: server_clone.clone(),
                    };
                    match crate::exec_env_postflight::apply_verdict(&mut outcome, &quarantine_sink)
                    {
                        Ok(_) => {
                            append_trajectory_event(
                                &traj_path_for_spawn,
                                outcome.trajectory_event(),
                            );
                            Some(outcome)
                        }
                        Err(error) => {
                            outcome.verdict = crate::exec_env_postflight::GateVerdict::Error {
                                detail: format!("resource fence persistence failed: {error}"),
                            };
                            append_trajectory_event(
                                &traj_path_for_spawn,
                                outcome.trajectory_event(),
                            );
                            Some(outcome)
                        }
                    }
                }
                Err(err) => {
                    tracing::error!(
                        dispatch_id = %d_id,
                        error = %err,
                        "postflight gate execution failed"
                    );
                    let mut error_outcome = gate.execution_error(
                        &runner_liveness,
                        format!("required gate execution failed: {err}"),
                    );
                    let quarantine_sink = crate::exec_env_postflight::DaemonQuarantineSink {
                        server: server_clone.clone(),
                    };
                    if let Err(fence_error) = crate::exec_env_postflight::apply_verdict(
                        &mut error_outcome,
                        &quarantine_sink,
                    ) {
                        error_outcome.verdict = crate::exec_env_postflight::GateVerdict::Error {
                            detail: format!(
                                "required gate execution failed: {err}; resource fence persistence failed: {fence_error}"
                            ),
                        };
                    }
                    append_trajectory_event(&traj_path_for_spawn, error_outcome.trajectory_event());
                    Some(error_outcome)
                }
            }
        } else {
            None
        };

        if let Some(outcome) = postflight_outcome.as_mut() {
            if outcome.artifacts_released() {
                let mut publication_error = None;
                if let Some(raw_output) = pending_acpx_output.take() {
                    match persist_acpx_events_and_map(
                        &workspace_dir_for_spawn,
                        &traj_path_for_spawn,
                        &d_id,
                        &agent_for_watchdog,
                        &raw_output,
                        true,
                    ) {
                        Ok(summary) => {
                            acpx_event_summary_json = Some(json!({
                                "events_file": summary.events_file.to_string_lossy(),
                                "mapped_events": summary.mapped_events,
                                "final_response_extracted": summary.final_response.is_some(),
                            }));
                        }
                        Err(error) => {
                            publication_error = Some(format!(
                                "postflight approved output but ACPX artifact publication failed: {error}"
                            ));
                        }
                    }
                }
                if publication_error.is_none() {
                    if let Some(deferred) = pending_native_acp_artifacts.take() {
                        if let Err(error) =
                            crate::dispatch_ops::acp_native::publish_native_acp_artifacts(
                                deferred,
                                &workspace_dir_for_spawn,
                                &traj_path_for_spawn,
                                &d_id,
                                &agent_for_watchdog,
                            )
                        {
                            publication_error = Some(format!(
                                "postflight approved output but native ACP artifact publication failed: {error}"
                            ));
                        }
                    }
                }
                if let Some(error) = publication_error {
                    outcome.verdict =
                        crate::exec_env_postflight::GateVerdict::Error { detail: error };
                    let quarantine_sink = crate::exec_env_postflight::DaemonQuarantineSink {
                        server: server_clone.clone(),
                    };
                    if let Err(fence_error) =
                        crate::exec_env_postflight::apply_verdict(outcome, &quarantine_sink)
                    {
                        outcome.verdict = crate::exec_env_postflight::GateVerdict::Error {
                            detail: format!(
                                "carrier artifact publication failed and resource fence persistence failed: {fence_error}"
                            ),
                        };
                    }
                    append_trajectory_event(&traj_path_for_spawn, outcome.trajectory_event());
                }
            }

            let lease_release = match early_exit_cleanup.postflight_dispatch_lease_mut() {
                Some(lease) if outcome.artifacts_released() => lease.release_clean(),
                Some(lease) if outcome.lease_fenced() => lease.release_after_fence(),
                Some(_) => Err(
                    "postflight lease remains exclusively admitted because its resource fence was not persisted"
                        .to_string(),
                ),
                None => Err("required postflight gate lost its dispatch lease guard".to_string()),
            };
            if let Err(error) = lease_release {
                outcome.verdict = crate::exec_env_postflight::GateVerdict::Error {
                    detail: format!("postflight lease finalization failed closed: {error}"),
                };
                append_trajectory_event(&traj_path_for_spawn, outcome.trajectory_event());
            }
            if outcome.artifacts_released() {
                append_trajectory_event(
                    &traj_path_for_spawn,
                    json!({
                        "event": "subprocess_output_released",
                        "dispatch_id": d_id,
                        "agent": agent_for_watchdog,
                        "output_tail": tail_chars(&full_output, 500),
                        "timestamp": Utc::now().to_rfc3339(),
                    }),
                );
            }
        }

        let postflight_withheld_error = if postflight_gate_for_spawn.is_some() {
            match &postflight_outcome {
                Some(outcome) if !outcome.artifacts_released() => {
                    Some(outcome.failure_message().unwrap_or_else(|| {
                        "postflight gate withheld dispatch artifacts".to_string()
                    }))
                }
                None => Some("postflight gate execution failed to resolve".to_string()),
                _ => None,
            }
        } else {
            None
        };
        let postflight_artifacts_withheld = postflight_withheld_error.is_some();

        // Save full output to result.md for orchestrator eval (only if postflight permitted release)
        let result_persist_error = if let Some(err) = postflight_withheld_error {
            tracing::warn!(
                dispatch_id = %d_id,
                reason = %err,
                "postflight gate withheld result artifact"
            );
            append_trajectory_event(
                &traj_path_for_spawn,
                json!({
                    "event": "result_withheld_by_postflight",
                    "dispatch_id": d_id,
                    "timestamp": Utc::now().to_rfc3339(),
                    "reason": err,
                }),
            );
            Some(err)
        } else {
            let result_path = workspace_dir.join("result.md");
            let error = super::persist_dispatch_result_artifact(
                &result_path,
                full_output.as_bytes(),
                managed_ephemeral_credential_cleanup.is_some(),
            )
            .err();
            if let Some(err) = error.as_ref() {
                tracing::warn!(
                    dispatch_id = %d_id,
                    path = %result_path.display(),
                    error = %err,
                    "failed to persist dispatch result artifact"
                );
                append_trajectory_event(
                    &traj_path_for_spawn,
                    json!({
                        "event": "result_persist_failed",
                        "dispatch_id": d_id,
                        "timestamp": Utc::now().to_rfc3339(),
                        "path": result_path.to_string_lossy(),
                        "error": err,
                    }),
                );
            }
            error
        };

        // --- WATCHDOG: check if sub-agent properly closed the loop ---
        // Poll for kanban state instead of a fixed sleep to avoid race conditions
        let (watchdog_polls, watchdog_interval) = watchdog_poll_config();
        let mut receipt_terminal_state = None;
        let mut pending_completion_recovery = false;
        let mut receipt_read_error = None;
        let mut kanban_state = None;
        // A runner that has reaped the managed group and returned its
        // cancellation proof already owns the terminal classification.  Do
        // not feed that result through the ordinary watchdog path: it would
        // synthesize a failure eval/outcome before the terminal writer has
        // persisted the cancellation receipt.
        let managed_cancellation_confirmed =
            matches!(&result, Err(error) if error == "managed_cancelled");
        if managed_cancellation_confirmed {
            receipt_terminal_state = Some("TASK_STATE_CANCELED");
        }
        if !managed_cancellation_confirmed {
            match completion_receipt_state_after_admission(
                &workspace_dir_for_spawn,
                watchdog_interval,
                &server_clone.managed_run_controls,
                &d_id,
                managed_cancellation.as_ref(),
            )
            .await
            {
                Ok(CompletionReceiptState::Terminal(receipt_state)) => {
                    receipt_terminal_state = Some(receipt_state);
                }
                Ok(CompletionReceiptState::PendingRecovery) => pending_completion_recovery = true,
                Ok(CompletionReceiptState::Open) => {}
                Ok(CompletionReceiptState::AdmissionInProgress) => {
                    unreachable!("admission wait resolves")
                }
                Err(error) => receipt_read_error = Some(error),
            }
        }
        for _ in 0..watchdog_polls {
            if managed_cancellation_confirmed
                || receipt_terminal_state.is_some()
                || pending_completion_recovery
                || receipt_read_error.is_some()
            {
                break;
            }
            tokio::time::sleep(watchdog_interval).await;
            match completion_receipt_state_after_admission(
                &workspace_dir_for_spawn,
                watchdog_interval,
                &server_clone.managed_run_controls,
                &d_id,
                managed_cancellation.as_ref(),
            )
            .await
            {
                Ok(CompletionReceiptState::Terminal(receipt_state)) => {
                    receipt_terminal_state = Some(receipt_state);
                    break;
                }
                Ok(CompletionReceiptState::PendingRecovery) => {
                    pending_completion_recovery = true;
                    break;
                }
                Ok(CompletionReceiptState::AdmissionInProgress) => {
                    unreachable!("admission wait resolves")
                }
                Ok(CompletionReceiptState::Open) => {}
                Err(error) => {
                    receipt_read_error = Some(error);
                    break;
                }
            }
            let state = get_kanban_state(&server_clone, &d_id).await;
            if canonical_terminal_state(state.as_deref()).is_some() {
                kanban_state = state;
                break;
            }
        }
        let kanban_state = match kanban_state {
            Some(s) => Some(s),
            None => get_kanban_state(&server_clone, &d_id).await,
        };
        if receipt_terminal_state.is_none()
            && !pending_completion_recovery
            && receipt_read_error.is_none()
        {
            match completion_receipt_state_after_admission(
                &workspace_dir_for_spawn,
                watchdog_interval,
                &server_clone.managed_run_controls,
                &d_id,
                managed_cancellation.as_ref(),
            )
            .await
            {
                Ok(CompletionReceiptState::Terminal(receipt_state)) => {
                    receipt_terminal_state = Some(receipt_state)
                }
                Ok(CompletionReceiptState::PendingRecovery) => pending_completion_recovery = true,
                Ok(CompletionReceiptState::AdmissionInProgress) => {
                    unreachable!("admission wait resolves")
                }
                Ok(CompletionReceiptState::Open) => {}
                Err(error) => receipt_read_error = Some(error),
            }
        }
        let polled_terminal_state = canonical_terminal_state(kanban_state.as_deref());
        let is_closed = !pending_completion_recovery
            && (receipt_terminal_state.is_some() || polled_terminal_state.is_some());
        if receipt_terminal_state == Some("TASK_STATE_FAILED") {
            crate::complete_ops::dispatch_outcome::record_terminal_failure_outcome(
                &server_clone,
                &d_id,
                "termination_unconfirmed",
                Some(agent_for_watchdog.as_str()),
                project_for_watchdog.as_deref(),
            );
        }
        // #1250: terminal accounting in the final `status.json` rewrite must
        // reflect the resolved predicate verdict, NOT the raw process exit
        // code. The watchdog branch below is the only path that actually
        // evaluates the predicate (via the same `resolve_completion_state`
        // machinery used at the success seam) — when it runs and resolves a
        // state, capture it here so the status write prefers it over the
        // exit-code derivation. Stays `None` when the watchdog branch does
        // not run (run already closed via `tachi_complete`) or runs the
        // crash/timeout sub-branch, preserving the existing exit-code
        // semantics for those non-predicate paths.
        let mut watchdog_resolved_state = receipt_read_error.as_ref().map(|error| {
            eprintln!(
                "[watchdog] refusing completion status read for dispatch {}: {}",
                d_id, error
            );
            "TASK_STATE_FAILED"
        });
        if pending_completion_recovery {
            eprintln!(
                "[watchdog] dispatch {} has a pending canonical-outcome recovery receipt; \
                 skipping terminalization until tachi_complete reconciles it",
                d_id
            );
        }
        if !is_closed && !pending_completion_recovery && watchdog_resolved_state.is_none() {
            let exited_ok = matches!(&result, Ok(r) if r.exit_code == Some(0));

            if exited_ok {
                // exit_code=0 but no tachi_complete: sub-agent forgot to
                // close the loop. Apply the #878-A completion predicate when
                // declared — an unsatisfied artifact/output contract is a
                // FALSE SUCCESS and must land FAILED, not COMPLETED. When no
                // predicate is declared, keep the conservative unreviewed
                // COMPLETED (no synthesized success eval for routing stats).
                let tail = if postflight_artifacts_withheld {
                    "[withheld by exec_env_postflight]".to_string()
                } else {
                    tail_chars(&full_output, 500)
                };
                let verdict = match crate::dispatch_ops::evaluate_completion_predicate_for_dispatch(
                    &server_clone.tachi_home_dir(),
                    &d_id,
                    &full_output,
                ) {
                    Ok((_, verdict)) => verdict,
                    Err(error) => {
                        eprintln!(
                            "[watchdog] refusing completion artifact read for dispatch {}: {}",
                            d_id, error
                        );
                        crate::dispatch_ops::PredicateVerdict::Fail(format!(
                            "completion artifact read refused: {error}"
                        ))
                    }
                };
                let (kanban_state, reviewed, override_reason) =
                    crate::dispatch_ops::resolve_completion_state("success", &verdict);
                // #1250: thread the predicate-resolved state into the final
                // `status.json` rewrite so an exit-0 run whose predicate
                // verdict is FAILURE lands FAILED there too — not collapsed
                // back to COMPLETED via the raw exit code.
                watchdog_resolved_state = Some(kanban_state);
                eprintln!(
                    "[watchdog] dispatch {} exited 0 without tachi_complete; predicate={} → {} reviewed={} tail={}",
                    d_id,
                    verdict.tag(),
                    kanban_state,
                    reviewed,
                    tail
                );
                if let Some(reason) = override_reason {
                    eprintln!("[watchdog] false-success intercepted: {reason}");
                }
                if let Err(error) =
                    update_kanban_state(&server_clone, &d_id, kanban_state, None, Some(reviewed))
                        .await
                {
                    eprintln!(
                        "[watchdog] failed to mark dispatch {} {} in kanban: {}",
                        d_id, kanban_state, error
                    );
                }
                // #773 Layer-2 ② (hole b): exit-0-without-tachi_complete that
                // the predicate intercepts as a FALSE SUCCESS is a terminal
                // FAILED the agent never `tachi_complete`d — record a canonical
                // outcome row (reported_outcome NULL — no self-report reached us).
                if kanban_state == "TASK_STATE_FAILED" {
                    crate::complete_ops::dispatch_outcome::record_terminal_failure_outcome(
                        &server_clone,
                        &d_id,
                        "watchdog",
                        Some(agent_for_watchdog.as_str()),
                        project_for_watchdog.as_deref(),
                    );
                }
            } else {
                // Crash / timeout / error: record a failure eval so the
                // failure is still visible in the ledger, but tag it as
                // `auto_synthesized=true` so the daily routing analysis can
                // exclude synthesized records from agent success-rate stats.
                let note = match &result {
                    Err(e) => format!("Watchdog: {}", e),
                    Ok(r) => {
                        let tail = tail_chars(&r.output, 500);
                        format!(
                            "Watchdog: Agent crashed (exit {:?}). Stderr tail: {}",
                            r.exit_code, tail
                        )
                    }
                };

                let ts = Utc::now();
                let eval_id = format!(
                    "eval_ws_{}_{}",
                    ts.format("%Y%m%dT%H%M%SZ"),
                    d_id.chars().take(16).collect::<String>()
                );
                let metadata = json!({
                    "task_id": eval_id,
                    "agent": format!("watchdog/{}", agent_for_watchdog),
                    "outcome": "failure",
                    "dispatch_id": d_id,
                    // Nightly routing analysis must exclude these so "fake"
                    // failures attributed to the watchdog agent don't pollute
                    // the real backend's success-rate.
                    "auto_synthesized": true,
                });
                let save_params = SaveMemoryParams {
                    text: note.clone(),
                    summary: format!("Watchdog auto-close FAILURE: {}", d_id),
                    path: format!("/eval/{}/{}", ts.format("%Y%m%d"), eval_id),
                    importance: 0.4,
                    category: "experience".to_string(),
                    topic: "eval".to_string(),
                    keywords: vec![
                        "eval".to_string(),
                        "watchdog".to_string(),
                        "failure".to_string(),
                        "auto_synthesized".to_string(),
                    ],
                    persons: Vec::new(),
                    entities: Vec::new(),
                    location: String::new(),
                    scope: "project".to_string(),
                    vector: None,
                    id: Some(eval_id.clone()),
                    force: true,
                    auto_link: false,
                    project: None,
                    project_explicit: false,
                    retention_policy: Some("durable".to_string()),
                    domain: Some("system".to_string()),
                    timestamp: None,
                    valid_from: None,
                    valid_until: None,
                    metadata: Some(metadata),
                    emit_continuity: false,
                };
                if let Err(error) =
                    crate::memory_search_ops::handle_save_memory(&server_clone, save_params).await
                {
                    eprintln!(
                        "[watchdog] failed to persist synthesized failure eval for dispatch {}: {}",
                        d_id, error
                    );
                }

                if let Err(error) = update_kanban_state(
                    &server_clone,
                    &d_id,
                    "TASK_STATE_FAILED",
                    Some(&eval_id),
                    Some(false),
                )
                .await
                {
                    eprintln!(
                        "[watchdog] failed to mark dispatch {} FAILED in kanban: {}",
                        d_id, error
                    );
                }
                // #773 Layer-2 ② (hole b): crash/timeout is a terminal FAILED
                // the agent never `tachi_complete`d — record a canonical outcome
                // row so the router learns from the failure (not just successes).
                crate::complete_ops::dispatch_outcome::record_terminal_failure_outcome(
                    &server_clone,
                    &d_id,
                    "watchdog",
                    Some(agent_for_watchdog.as_str()),
                    project_for_watchdog.as_deref(),
                );
            }
        }

        let should_cleanup = managed_ephemeral_credential_cleanup.is_some()
            || match &result {
                Ok(r) if !pending_completion_recovery => {
                    should_cleanup_run(r.exit_code, kanban_state.as_deref())
                }
                Err(_) => false,
                Ok(_) => false,
            };
        // A managed cancellation is confirmed only if its ephemeral
        // credentials are already gone.  The cleanup result is therefore an
        // input to the one canonical terminal write below, never a later
        // downgrade of a committed confirmation.
        let credential_cleanup_failed = if should_cleanup {
            let credential_cleanup = {
                #[cfg(test)]
                if managed_credential_cleanup_failure_injected() {
                    Err("injected managed credential cleanup failure".to_string())
                } else {
                    server_clone.with_global_store(|store| {
                        cleanup_ephemeral_credential_materializations(
                            store,
                            &workspace_dir_for_spawn,
                            false,
                        )
                    })
                }
                #[cfg(not(test))]
                {
                    server_clone.with_global_store(|store| {
                        cleanup_ephemeral_credential_materializations(
                            store,
                            &workspace_dir_for_spawn,
                            false,
                        )
                    })
                }
            };
            let failed = match &credential_cleanup {
                Ok(report) => !report.errors.is_empty(),
                Err(_) => true,
            };
            append_trajectory_event(
                &traj_path_for_spawn,
                json!({
                    "event": "credentials_cleanup",
                    "dispatch_id": d_id,
                    "agent": agent_for_watchdog,
                    "report": credential_cleanup
                        .as_ref()
                        .map(|report| serde_json::to_value(report).unwrap_or_else(|_| json!({"error": "serialize cleanup report"})))
                        .unwrap_or_else(|err| json!({"errors": [err]})),
                    "timestamp": Utc::now().to_rfc3339(),
                }),
            );
            append_trajectory_event(
                &traj_path_for_spawn,
                json!({
                    "event": "workspace_retained",
                    "dispatch_id": d_id,
                    "reason": "run_dir is retained so board/status links remain valid",
                    "run_dir": workspace_dir.to_string_lossy(),
                    "timestamp": Utc::now().to_rfc3339(),
                }),
            );
            failed
        } else {
            false
        };

        // Final audit: dispatch_finished + status.json refresh.
        let final_exit_code = match &result {
            Ok(r) => r.exit_code,
            Err(_) => None,
        };
        let total_duration_ms = plan_duration_ms_for_spawn.unwrap_or(0) + execute_duration_ms;

        append_trajectory_event(
            &traj_path_for_spawn,
            json!({
                "event": "dispatch_finished",
                "dispatch_id": d_id,
                "agent": agent_for_watchdog,
                "stage": stage_for_traj,
                "v2": v2_for_spawn,
                "exit_code": final_exit_code,
                "duration_ms_execute": execute_duration_ms,
                "duration_ms_plan": plan_duration_ms_for_spawn,
                "total_duration_ms": total_duration_ms,
                "timestamp": Utc::now().to_rfc3339(),
            }),
        );

        // Preserve dispatch-time contract fields across the final status rewrite
        // so complete/watchdog can still evaluate the #878-A predicate after exit.
        let status_path = workspace_dir_for_spawn.join("status.json");
        let managed_terminal_anchor = managed_terminal_status_anchor(
            &server_clone.managed_run_controls,
            &d_id,
            managed_cancellation.as_ref(),
        );
        let (prev_status, final_status_read_error) = match read_status_json_for_terminal(
            &workspace_dir_for_spawn,
            &status_path,
            &managed_terminal_anchor,
        ) {
            Ok(status) => (status, None),
            Err(error) => (None, Some(error)),
        };
        let preserved_predicate = prev_status
            .as_ref()
            .and_then(|v| v.get("completion_predicate").cloned())
            .unwrap_or(Value::Null);
        let preserved_cwd = prev_status
            .as_ref()
            .and_then(|v| v.get("cwd").cloned())
            .unwrap_or(Value::Null);
        // #894 S2d: the effective-authority receipt is a dispatch-time fact —
        // it must survive the terminal rewrite, or "who was actually stopping
        // this agent from writing?" becomes unanswerable the moment the run
        // finishes, which is exactly when someone asks.
        let preserved_authority = prev_status
            .as_ref()
            .and_then(|v| v.get("authority").cloned())
            .unwrap_or(Value::Null);

        let artifact_read_error = final_status_read_error.or(receipt_read_error);
        if let Some(error) = artifact_read_error.as_deref() {
            eprintln!(
                "[watchdog] completion artifact refusal persisted for dispatch {}: {}",
                d_id, error
            );
        }
        let final_status_state = if result_persist_error.is_some() {
            "TASK_STATE_FAILED"
        } else if matches!(&result, Err(error) if error == "managed_cancelled")
            && credential_cleanup_failed
        {
            "TASK_STATE_FAILED"
        } else if matches!(&result, Err(error) if error == "managed_cancelled") {
            "TASK_STATE_CANCELED"
        } else if matches!(&result, Err(error) if error == "termination_unconfirmed" || error.starts_with("managed cancellation child probe failed"))
        {
            "TASK_STATE_FAILED"
        } else if pending_completion_recovery {
            pending_recovery_status_state(prev_status.as_ref())
        } else if artifact_read_error.is_some() {
            "TASK_STATE_FAILED"
        } else {
            terminal_status_state(
                receipt_terminal_state,
                watchdog_resolved_state,
                polled_terminal_state,
                final_exit_code,
            )
        };
        // A dequeued managed cancellation is finalized by this background
        // owner, after the result artifact and all terminal accounting have
        // been chosen. The runner only owns process lifetime; it never wakes
        // the caller with a speculative receipt.
        let managed_finalization = managed_cancellation.as_ref().map(|command| {
            json!({
                "expected_status_revision": command.expected_status_revision,
                "runner_error": result.as_ref().err(),
                "termination_proof": managed_termination_proof,
                "credential_cleanup_failed": credential_cleanup_failed,
                "result_persist_failed": result_persist_error.is_some(),
            })
        });

        let managed_cancel_completion = write_status_json_for_terminal(
            &workspace_dir_for_spawn,
            &d_id,
            v2_for_spawn,
            plan_generated_at_for_spawn.as_deref(),
            Some(&execute_started_at.to_rfc3339()),
            if v2_for_spawn { "approved" } else { "n/a" },
            final_exit_code,
            plan_duration_ms_for_spawn,
            Some(execute_duration_ms),
            Some(total_duration_ms),
            Some(json!({
                "agent": agent_for_watchdog.clone(),
                "state": final_status_state,
                "closure_kind": terminal_closure_kind(final_status_state),
                "updated_at": Utc::now().to_rfc3339(),
                "run_dir": workspace_dir_for_spawn.to_string_lossy(),
                "result_written": result_persist_error.is_none(),
                "result_persist_error": result_persist_error,
                "artifact_read_error": artifact_read_error,
                "completion_predicate": preserved_predicate,
                "cwd": preserved_cwd,
                "authority": preserved_authority,
                "harness_transport": harness_transport_for_spawn.clone(),
                "harness_server_url": harness_server_url_for_spawn.clone(),
                "host_adapter": host_adapter_for_spawn.clone(),
                "execution_backend": if is_acpx_transport(&harness_transport_for_spawn) {
                    Some("acpx")                } else if is_native_acp_transport(&harness_transport_for_spawn) {
                    Some("acp_native")
                } else {
                    None
                },
                "acpx": if is_acpx_transport(&harness_transport_for_spawn) {
                    execution_backend_metadata_for_spawn.clone()
                } else {
                    None
                },
                "acp_native": if is_native_acp_transport(&harness_transport_for_spawn) {
                    execution_backend_metadata_for_spawn.clone()
                } else {
                    None
                },
                "acpx_events": acpx_event_summary_json,
                "feedback_rules": feedback_rules_trace_for_spawn,
                "timeout_secs": timeout_secs_for_spawn,
                "managed_cancellation_finalization": managed_finalization,
                "exec_env_postflight": postflight_outcome
                    .as_ref()
                    .map(|o| o.receipt())
                    .unwrap_or_else(|| {
                        json!({
                            "gate": "exec_env_postflight",
                            "status": "not_applicable"
                        })
                    }),
            })),
            managed_terminal_anchor,
        );

        if matches!(
            managed_cancel_completion,
            Some(crate::managed_run_control::CancelCompletion::Confirmed { .. })
        ) {
            if let Err(error) = update_kanban_state(
                &server_clone,
                &d_id,
                "TASK_STATE_CANCELED",
                None,
                Some(false),
            )
            .await
            {
                eprintln!(
                    "[watchdog] failed to mark cancelled dispatch {}: {}",
                    d_id, error
                );
            }
        }
        if let Some(crate::managed_run_control::CancelCompletion::Unavailable(reason)) =
            managed_cancel_completion.as_ref()
        {
            if *reason == "credential_cleanup_failed"
                || *reason == "persist_failed"
                || *reason == "result_persist_failed"
            {
                if let Err(error) = update_kanban_state(
                    &server_clone,
                    &d_id,
                    "TASK_STATE_FAILED",
                    None,
                    Some(false),
                )
                .await
                {
                    eprintln!(
                        "[watchdog] failed to mark managed terminal failure {}: {}",
                        d_id, error
                    );
                }
                crate::complete_ops::dispatch_outcome::record_terminal_failure_outcome(
                    &server_clone,
                    &d_id,
                    reason,
                    Some(agent_for_watchdog.as_str()),
                    project_for_watchdog.as_deref(),
                );
            }
        }
        early_exit_cleanup.complete();
        // The response itself is a lifecycle receipt: it is released only
        // once credential cleanup, slot release, and registry removal are all
        // complete. No detached responder can outlive this owner.
        drop(_managed_run_guard);
        if let Some(command) = managed_cancellation.take() {
            #[cfg(test)]
            let mut command = command;
            #[cfg(test)]
            if let Some(observation) = command.test_observation.take() {
                observation.complete(&workspace_dir_for_spawn, managed_cancel_completion.as_ref());
            }
            if let Some(completion) = managed_cancel_completion {
                let _ = command.response.send(completion);
            }
        }
        #[cfg(test)]
        mark_background_dispatch_cleanup_complete(&d_id);
    });
}

#[cfg(test)]
fn background_dispatch_cleanup_completions() -> &'static Mutex<HashSet<String>> {
    static COMPLETIONS: OnceLock<Mutex<HashSet<String>>> = OnceLock::new();
    COMPLETIONS.get_or_init(|| Mutex::new(HashSet::new()))
}

#[cfg(test)]
fn mark_background_dispatch_cleanup_complete(dispatch_id: &str) {
    background_dispatch_cleanup_completions()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .insert(dispatch_id.to_string());
}

#[cfg(test)]
pub(crate) fn background_dispatch_cleanup_complete(dispatch_id: &str) -> bool {
    background_dispatch_cleanup_completions()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .contains(dispatch_id)
}

#[cfg(test)]
#[derive(Clone)]
struct AcpStatusReadBarrier {
    run_dir: std::path::PathBuf,
    read: Arc<Barrier>,
    resume: Arc<Barrier>,
}

#[cfg(test)]
fn acp_status_read_barrier() -> &'static Mutex<Option<AcpStatusReadBarrier>> {
    static BARRIER: OnceLock<Mutex<Option<AcpStatusReadBarrier>>> = OnceLock::new();
    BARRIER.get_or_init(|| Mutex::new(None))
}

#[cfg(test)]
struct AcpStatusReadBarrierGuard;

#[cfg(test)]
impl Drop for AcpStatusReadBarrierGuard {
    fn drop(&mut self) {
        *acp_status_read_barrier()
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = None;
    }
}

#[cfg(test)]
fn install_acp_status_read_barrier(
    run_dir: std::path::PathBuf,
    read: Arc<Barrier>,
    resume: Arc<Barrier>,
) -> AcpStatusReadBarrierGuard {
    *acp_status_read_barrier()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(AcpStatusReadBarrier {
        run_dir,
        read,
        resume,
    });
    AcpStatusReadBarrierGuard
}

#[cfg(test)]
fn pause_acp_after_status_read(run_dir: &Path) {
    let barrier = {
        let mut barrier = acp_status_read_barrier()
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        barrier
            .as_ref()
            .is_some_and(|configured| configured.run_dir == run_dir)
            .then(|| barrier.take())
            .flatten()
    };
    if let Some(barrier) = barrier {
        barrier.read.wait();
        barrier.resume.wait();
    }
}

/// Persist a model value reported by the native ACP session into the existing
/// identity receipt. The carrier reported only the concrete model, so every
/// other observed identity field deliberately stays `unknown`.
fn persist_acp_model_acknowledgement(
    run_dir: &Path,
    observed_model: Option<&str>,
) -> Result<bool, String> {
    let Some(observed_model) = observed_model
        .map(str::trim)
        .filter(|model| !model.is_empty())
    else {
        return Ok(false);
    };
    let status_lock = status_json_lock_for(run_dir);
    let _status_guard = status_lock
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let status_path = run_dir.join("status.json");
    let mut status = crate::task_lifecycle::read_json_file(&status_path)?
        .ok_or_else(|| format!("ACP acknowledgement requires {}", status_path.display()))?;
    #[cfg(test)]
    pause_acp_after_status_read(run_dir);
    let receipt_value = status.get("identity_receipt").cloned().ok_or_else(|| {
        format!(
            "ACP acknowledgement requires identity_receipt in {}",
            status_path.display()
        )
    })?;
    let mut receipt =
        serde_json::from_value::<DispatchIdentityReceipt>(receipt_value).map_err(|error| {
            format!(
                "parse identity_receipt in {}: {error}",
                status_path.display()
            )
        })?;

    let observed = acp_model_only_identity(observed_model);
    let acknowledgement = if receipt.planned.model.as_deref() == Some(observed_model) {
        DispatchAcknowledgement::Acknowledged
    } else {
        DispatchAcknowledgement::Substituted
    };
    if acknowledgement == DispatchAcknowledgement::Substituted
        && observed.model_lineage_id == UNKNOWN_IDENTITY
    {
        return Err(
            "ACP reported a different model without canonical provider/family lineage evidence"
                .to_string(),
        );
    }
    receipt
        .acknowledge(
            observed,
            acknowledgement,
            "native ACP session config option reported the concrete model".to_string(),
        )
        .map_err(|error| format!("acknowledge ACP identity: {error}"))?;

    let status_object = status
        .as_object_mut()
        .ok_or_else(|| format!("status is not an object: {}", status_path.display()))?;
    status_object.insert(
        "identity_receipt".to_string(),
        serde_json::to_value(receipt)
            .map_err(|error| format!("serialize ACP identity receipt: {error}"))?,
    );
    crate::managed_run_control::advance_status_revision(status_object)?;
    let body = serde_json::to_vec_pretty(&status)
        .map_err(|error| format!("serialize {}: {error}", status_path.display()))?;
    crate::utils::write_owner_only_file_atomic(&status_path, &body)
        .map_err(|error| format!("persist {}: {error}", status_path.display()))?;
    Ok(true)
}

fn acp_model_only_identity(model: &str) -> DispatchIdentityEffective {
    // These values are derived solely from the concrete model the ACP carrier
    // reported. They are never copied from the requested/planned receipt, so
    // `acknowledge` can still reject a cross-lineage substitution.
    let (concrete_model_release, provider_model, provider_model_version) =
        provider_model_parts(Some(model));
    DispatchIdentityEffective {
        profile: None,
        model: Some(model.to_string()),
        backend: UNKNOWN_IDENTITY.to_string(),
        harness: UNKNOWN_IDENTITY.to_string(),
        model_lineage_id: model_lineage_id(Some(model), UNKNOWN_IDENTITY),
        concrete_model_release,
        provider_model,
        provider_model_version,
        role: UNKNOWN_IDENTITY.to_string(),
        seat: UNKNOWN_IDENTITY.to_string(),
        transport: UNKNOWN_IDENTITY.to_string(),
        adapter_version: UNKNOWN_IDENTITY.to_string(),
        carrier_version: UNKNOWN_IDENTITY.to_string(),
    }
}

fn watchdog_poll_config() -> (usize, Duration) {
    let default_polls = if cfg!(test) { 1 } else { 10 };
    let default_poll_ms = if cfg!(test) { 10 } else { 300 };
    let polls = bounded_env_usize("TACHI_DISPATCH_WATCHDOG_POLLS", default_polls, 100);
    let poll_ms = bounded_env_u64("TACHI_DISPATCH_WATCHDOG_POLL_MS", default_poll_ms, 5_000);
    (polls, Duration::from_millis(poll_ms))
}

/// Resolve the terminal kanban `state` field written into the final
/// `status.json` rewrite (#1250).
///
/// The raw process exit code is NOT a faithful terminal signal: a vendor can
/// exit 0 while the declared completion contract is unmet — a FALSE SUCCESS.
/// Two sites resolve the real terminal state and we must honor either over
/// the exit code:
///
/// 1. **Durable completion receipt (round 3):** when `tachi_complete` resolves
///    an outcome it writes `resolved_completion` to `status.json` before the
///    best-effort kanban projection. This is authoritative because kanban may
///    be missing or stale.
/// 2. **Watchdog path (round 1):** when the run exits 0 without calling
///    `tachi_complete`, the watchdog branch in [`spawn_background_dispatch`]
///    evaluates the predicate via `resolve_completion_state("success",
///    &verdict)` and threads the result through `watchdog_resolved`.
/// 3. **`tachi_complete` kanban projection (round 2):** when the agent DID call
///    `tachi_complete`, `complete_ops::handler.rs` already ran the same
///    `resolve_completion_state` machinery and persisted the resolved state
///    to the kanban row. The watchdog spawn's kanban poll reads that state
///    back; `polled_terminal_state` carries it into this helper so an exit-0
///    run cannot collapse a deliberate close (including `partial` /
///    INPUT_REQUIRED) back to COMPLETED.
///
/// Priority: the durable receipt beats every derived signal. Then
/// `watchdog_resolved` beats `polled_terminal_state` (both never set
/// simultaneously in the live path — the watchdog branch only runs when
/// `!is_closed`). When all are `None`, the exit-code-derived mapping is kept.
fn terminal_status_state(
    receipt_terminal_state: Option<&'static str>,
    watchdog_resolved: Option<&'static str>,
    polled_terminal_state: Option<&'static str>,
    final_exit_code: Option<i32>,
) -> &'static str {
    receipt_terminal_state
        .or(watchdog_resolved)
        .or(polled_terminal_state)
        .unwrap_or(match final_exit_code {
            Some(0) => "TASK_STATE_COMPLETED",
            Some(_) => "TASK_STATE_FAILED",
            None => "TASK_STATE_FAILED",
        })
}

/// Read a handler-written completion receipt from the dispatch run. A partial
/// receipt is valid only with its explicit closure marker, so the generic
/// INPUT_REQUIRED vocabulary used by plan review cannot be misclassified as a
/// terminal partial close.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CompletionReceiptState {
    Terminal(&'static str),
    PendingRecovery,
    AdmissionInProgress,
    Open,
}

/// Read a handler-written completion receipt from the dispatch run. A pending
/// canonical-outcome recovery takes precedence over any stale terminal data:
/// it is an explicit barrier until tachi_complete reconciles the outcome row.
#[cfg(test)]
fn completion_receipt_state(run_dir: &std::path::Path) -> Result<CompletionReceiptState, String> {
    let Some(raw_status) = crate::dispatch_ops::read_text_file_within(
        run_dir,
        &run_dir.join("status.json"),
        WATCHDOG_STATUS_MAX_BYTES,
    )?
    else {
        return Ok(CompletionReceiptState::Open);
    };
    let status: Value = serde_json::from_str(&raw_status).map_err(|error| {
        format!(
            "completion status artifact {} is not valid JSON: {error}",
            run_dir.join("status.json").display()
        )
    })?;
    completion_receipt_state_from_status(&status)
}

fn completion_receipt_state_for_terminal(
    run_dir: &std::path::Path,
    managed_anchor: &ManagedTerminalStatusAnchor,
) -> Result<CompletionReceiptState, String> {
    let status_path = run_dir.join("status.json");
    let Some(status) = read_status_json_for_terminal(run_dir, &status_path, managed_anchor)? else {
        return Ok(CompletionReceiptState::Open);
    };
    completion_receipt_state_from_status(&status)
}

fn completion_receipt_state_from_status(status: &Value) -> Result<CompletionReceiptState, String> {
    if let Some(recovery_status) = status
        .get("completion_recovery")
        .and_then(Value::as_object)
        .and_then(|recovery| recovery.get("status"))
        .and_then(Value::as_str)
    {
        return Ok(if recovery_status == "completion_admitted" {
            // A handler holding the private registry lease owns all
            // irreversible completion writes. The terminal owner waits for
            // that lease to resolve instead of racing an eval/outcome receipt.
            CompletionReceiptState::AdmissionInProgress
        } else {
            CompletionReceiptState::PendingRecovery
        });
    }
    if status
        .get("cancellation")
        .and_then(Value::as_object)
        .is_some_and(|receipt| {
            matches!(
                receipt.get("receipt").and_then(Value::as_str),
                Some("termination_unconfirmed")
            ) || (receipt.get("receipt").and_then(Value::as_str) == Some("cancellation_confirmed")
                && matches!(
                    receipt.get("termination_proof").and_then(Value::as_str),
                    Some("spawn_suppressed" | "unix_process_group_absent")
                ))
        })
    {
        return Ok(CompletionReceiptState::Terminal(
            if status["cancellation"]["receipt"] == "termination_unconfirmed" {
                "TASK_STATE_FAILED"
            } else {
                "TASK_STATE_CANCELED"
            },
        ));
    }
    let Some(receipt) = status.get("resolved_completion").and_then(Value::as_object) else {
        return Ok(CompletionReceiptState::Open);
    };
    let Some(state) = receipt.get("state").and_then(Value::as_str) else {
        return Ok(CompletionReceiptState::Open);
    };
    if receipt
        .get("eval_ledger_id")
        .and_then(Value::as_str)
        .is_none_or(|value| value.trim().is_empty())
        || receipt.get("reviewed").and_then(Value::as_bool).is_none()
        || receipt
            .get("recorded_at")
            .and_then(Value::as_str)
            .and_then(|value| chrono::DateTime::parse_from_rfc3339(value).ok())
            .is_none()
    {
        return Ok(CompletionReceiptState::Open);
    }
    Ok(match state {
        "TASK_STATE_INPUT_REQUIRED"
            if receipt.get("closure_kind").and_then(Value::as_str) == Some("partial") =>
        {
            CompletionReceiptState::Terminal("TASK_STATE_INPUT_REQUIRED")
        }
        "TASK_STATE_INPUT_REQUIRED" => CompletionReceiptState::Open,
        terminal if receipt.get("closure_kind").is_some_and(Value::is_null) => {
            canonical_terminal_state(Some(terminal))
                .map(CompletionReceiptState::Terminal)
                .unwrap_or(CompletionReceiptState::Open)
        }
        _ => CompletionReceiptState::Open,
    })
}

async fn completion_receipt_state_after_admission(
    run_dir: &std::path::Path,
    poll_interval: Duration,
    registry: &crate::managed_run_control::ManagedRunControlRegistry,
    dispatch_id: &str,
    managed_cancellation: Option<&crate::managed_run_control::ManagedCancelCommand>,
) -> Result<CompletionReceiptState, String> {
    loop {
        let managed_anchor =
            managed_terminal_status_anchor(registry, dispatch_id, managed_cancellation);
        match completion_receipt_state_for_terminal(run_dir, &managed_anchor)? {
            CompletionReceiptState::AdmissionInProgress => tokio::time::sleep(poll_interval).await,
            state => return Ok(state),
        }
    }
}

#[cfg(test)]
fn resolved_completion_terminal_state(
    run_dir: &std::path::Path,
) -> Result<Option<&'static str>, String> {
    Ok(match completion_receipt_state(run_dir)? {
        CompletionReceiptState::Terminal(state) => Some(state),
        CompletionReceiptState::PendingRecovery
        | CompletionReceiptState::AdmissionInProgress
        | CompletionReceiptState::Open => None,
    })
}

fn pending_recovery_status_state(previous_status: Option<&Value>) -> &'static str {
    previous_status
        .and_then(|status| status.get("state"))
        .and_then(Value::as_str)
        .and_then(|state| match state {
            "TASK_STATE_PENDING" => Some("TASK_STATE_PENDING"),
            "TASK_STATE_WORKING" => Some("TASK_STATE_WORKING"),
            "TASK_STATE_RUNNING" => Some("TASK_STATE_RUNNING"),
            "TASK_STATE_INPUT_REQUIRED" => Some("TASK_STATE_INPUT_REQUIRED"),
            _ => None,
        })
        .unwrap_or("TASK_STATE_WORKING")
}

/// The status ledger keeps INPUT_REQUIRED for both partial outcomes and plan
/// review. The execution path only reaches this state after a closed partial,
/// so persist the discriminator that downstream readers need to preserve that
/// distinction once the kanban row is no longer available.
fn terminal_closure_kind(terminal_state: &str) -> Option<&'static str> {
    (terminal_state == "TASK_STATE_INPUT_REQUIRED").then_some("partial")
}

/// Reduce a polled kanban state `String` (whose lifetime is not `'static`) to
/// its canonical `&'static str` literal when it is one of the recognized
/// terminal states; otherwise `None`. INPUT_REQUIRED deliberately remains
/// non-terminal here because the kanban vocabulary also represents active
/// plan review. A completed partial is recognized only from the strict
/// resolved-completion receipt above. Used at the `terminal_status_state` call
/// site to thread the polled kanban value through the helper without leaking
/// the borrowed `String`'s lifetime. Non-terminal / unknown values map to
/// `None` so the watchdog evaluates the predicate instead of silently
/// synthesizing a partial close.
fn canonical_terminal_state(polled: Option<&str>) -> Option<&'static str> {
    polled.and_then(|s| match s {
        "TASK_STATE_COMPLETED" => Some("TASK_STATE_COMPLETED"),
        "TASK_STATE_FAILED" => Some("TASK_STATE_FAILED"),
        "TASK_STATE_CANCELED" => Some("TASK_STATE_CANCELED"),
        _ => None,
    })
}

fn bounded_env_usize(name: &str, default: usize, max: usize) -> usize {
    std::env::var(name)
        .ok()
        .and_then(|raw| raw.parse::<usize>().ok())
        .unwrap_or(default)
        .clamp(1, max)
}

fn bounded_env_u64(name: &str, default: u64, max: u64) -> u64 {
    std::env::var(name)
        .ok()
        .and_then(|raw| raw.parse::<u64>().ok())
        .unwrap_or(default)
        .clamp(1, max)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tachi_dispatch::{
        DispatchAcknowledgement, DispatchIdentityReceipt, DispatchIdentityRequest, UNKNOWN_IDENTITY,
    };

    fn write_planned_receipt(run_dir: &Path, planned_model: &str) {
        let receipt = DispatchIdentityReceipt::planned(
            DispatchIdentityRequest {
                profile: None,
                model: Some(planned_model.to_string()),
                agent: Some("codex".to_string()),
                harness: Some("acp".to_string()),
            },
            acp_model_only_identity(planned_model),
            "test planned identity".to_string(),
            false,
        );
        std::fs::write(
            run_dir.join("status.json"),
            serde_json::to_vec_pretty(&json!({ "identity_receipt": receipt }))
                .expect("serialize receipt"),
        )
        .expect("write status");
    }

    fn read_receipt(run_dir: &Path) -> DispatchIdentityReceipt {
        let status: Value = serde_json::from_slice(
            &std::fs::read(run_dir.join("status.json")).expect("read status"),
        )
        .expect("parse status");
        serde_json::from_value(status["identity_receipt"].clone()).expect("parse receipt")
    }

    /// Native ACP acknowledgement is a receipt merge, not a second lifecycle:
    /// terminal state and post-acceptance route evidence must survive its
    /// identity update even when the child finishes before acknowledgement.
    #[test]
    fn native_acp_acknowledgement_preserves_terminal_route_evidence() {
        let temp = tempfile::tempdir().expect("temporary run directory");
        write_planned_receipt(temp.path(), "gpt-5.5");
        write_status_json(
            temp.path(),
            "20260822T000001Z-acp-terminal",
            false,
            None,
            None,
            "n/a",
            Some(0),
            None,
            Some(1),
            Some(1),
            Some(json!({
                "state": "TASK_STATE_COMPLETED",
                "result_written": true,
            })),
        );
        stamp_route_decision_id(temp.path(), "route-fast-terminal").expect("stamp route evidence");

        assert!(
            persist_acp_model_acknowledgement(temp.path(), Some("gpt-5.5"))
                .expect("merge ACP acknowledgement")
        );
        let status: Value = serde_json::from_slice(
            &std::fs::read(temp.path().join("status.json")).expect("read merged receipt"),
        )
        .expect("parse merged receipt");
        assert_eq!(status["state"], "TASK_STATE_COMPLETED");
        assert_eq!(status["result_written"], true);
        assert_eq!(status["route_decision_id"], "route-fast-terminal");
        assert_eq!(
            status["status_revision"], 3,
            "base status, route evidence, and ACP acknowledgement each advance the shared revision"
        );
        assert_eq!(
            read_receipt(temp.path())
                .observed
                .effective
                .model
                .as_deref(),
            Some("gpt-5.5"),
            "ACP update must merge only identity receipt"
        );
    }

    /// The ACP path intentionally pauses after a stale receipt read while
    /// holding the same per-run lock as terminal lifecycle and route evidence.
    /// If any of those lock acquisitions is removed, terminal+route writes can
    /// finish before ACP resumes and its stale snapshot erases them.
    #[test]
    fn acp_stale_read_cannot_lose_terminal_or_route_evidence() {
        let temp = tempfile::tempdir().expect("temporary run directory");
        write_planned_receipt(temp.path(), "gpt-5.5");
        let read = Arc::new(Barrier::new(2));
        let resume = Arc::new(Barrier::new(2));
        let run_dir = temp.path().to_path_buf();
        let _barrier = install_acp_status_read_barrier(
            run_dir.clone(),
            Arc::clone(&read),
            Arc::clone(&resume),
        );
        let acp = std::thread::spawn(move || {
            persist_acp_model_acknowledgement(&run_dir, Some("gpt-5.5"))
        });

        read.wait();
        let terminal_dir = temp.path().to_path_buf();
        let (terminal_done, terminal_result) = std::sync::mpsc::sync_channel(1);
        let terminal = std::thread::spawn(move || {
            write_status_json(
                &terminal_dir,
                "20260822T000002Z-acp-overlap",
                false,
                None,
                None,
                "n/a",
                Some(0),
                None,
                Some(1),
                Some(1),
                Some(json!({ "state": "TASK_STATE_COMPLETED", "result_written": true })),
            );
            let result = stamp_route_decision_id(&terminal_dir, "route-overlap");
            terminal_done.send(result).expect("report terminal result");
        });
        assert!(
            terminal_result
                .recv_timeout(Duration::from_millis(100))
                .is_err(),
            "terminal lifecycle must wait behind the ACP stale-read lock"
        );

        resume.wait();
        assert!(acp.join().expect("join ACP").expect("ACP merge"));
        terminal.join().expect("join terminal lifecycle");
        terminal_result
            .recv_timeout(Duration::from_secs(1))
            .expect("terminal result after ACP release")
            .expect("route stamp after terminal lifecycle");

        let status: Value = serde_json::from_slice(
            &std::fs::read(temp.path().join("status.json")).expect("read merged receipt"),
        )
        .expect("parse merged receipt");
        assert_eq!(status["state"], "TASK_STATE_COMPLETED");
        assert_eq!(status["result_written"], true);
        assert_eq!(status["route_decision_id"], "route-overlap");
        assert_eq!(
            read_receipt(temp.path())
                .observed
                .effective
                .model
                .as_deref(),
            Some("gpt-5.5")
        );

        let execution_source = include_str!("execution.rs");
        let lifecycle_source = include_str!("../dispatch_v2.rs");
        assert!(execution_source.contains("status_json_lock_for(run_dir)"));
        assert_eq!(
            lifecycle_source
                .matches("status_json_lock_for(run_dir)")
                .count(),
            2,
            "route stamp and lifecycle rewrite must each acquire the shared per-run lock"
        );
    }

    #[test]
    fn acp_matching_model_acknowledges_without_fabricating_identity_fields() {
        let temp = tempfile::tempdir().expect("temp run dir");
        write_planned_receipt(temp.path(), "openai/gpt-5.2");

        assert!(
            persist_acp_model_acknowledgement(temp.path(), Some("openai/gpt-5.2"))
                .expect("persist acknowledgement"),
            "a concrete ACP model must acknowledge the frozen receipt"
        );

        let receipt = read_receipt(temp.path());
        assert_eq!(
            receipt.observed.acknowledgement,
            DispatchAcknowledgement::Acknowledged
        );
        assert!(!receipt.observed.mismatch);
        assert_eq!(
            receipt.observed.effective.model.as_deref(),
            Some("openai/gpt-5.2")
        );
        assert_eq!(receipt.observed.effective.backend, UNKNOWN_IDENTITY);
        assert_eq!(receipt.observed.effective.model_lineage_id, "openai/gpt");
    }

    #[test]
    fn acp_different_model_is_substituted_with_only_the_reported_model() {
        let temp = tempfile::tempdir().expect("temp run dir");
        write_planned_receipt(temp.path(), "openai/gpt-5.1");

        assert!(
            persist_acp_model_acknowledgement(temp.path(), Some("openai/gpt-5.2"))
                .expect("persist acknowledgement"),
            "a concrete ACP substitution must be persisted"
        );

        let receipt = read_receipt(temp.path());
        assert_eq!(
            receipt.observed.acknowledgement,
            DispatchAcknowledgement::Substituted
        );
        assert!(receipt.observed.mismatch);
        assert_eq!(
            receipt.observed.effective.model.as_deref(),
            Some("openai/gpt-5.2")
        );
        assert_eq!(receipt.observed.effective.backend, UNKNOWN_IDENTITY);
        assert_eq!(receipt.observed.effective.model_lineage_id, "openai/gpt");
    }

    #[test]
    fn acp_cross_lineage_substitution_is_not_persisted_without_authorization() {
        let temp = tempfile::tempdir().expect("temp run dir");
        write_planned_receipt(temp.path(), "zhipuai-coding-plan/glm-5.2");

        let error = persist_acp_model_acknowledgement(temp.path(), Some("openai/gpt-5.2"))
            .expect_err("cross-lineage ACP model must be rejected by the frozen receipt");
        assert!(error.contains("cross-lineage"), "error={error}");
        assert_eq!(
            read_receipt(temp.path()).observed.acknowledgement,
            DispatchAcknowledgement::Unconfirmed,
            "a rejected acknowledgement must leave the persisted receipt unchanged"
        );
    }

    #[test]
    fn acp_unqualified_substitution_is_not_persisted_without_lineage_evidence() {
        let temp = tempfile::tempdir().expect("temp run dir");
        write_planned_receipt(temp.path(), "zhipuai-coding-plan/glm-5.2");

        let error = persist_acp_model_acknowledgement(temp.path(), Some("gpt-5.2"))
            .expect_err("a different unqualified ACP model has no lineage evidence");
        assert!(error.contains("canonical provider/family"), "error={error}");
        assert_eq!(
            read_receipt(temp.path()).observed.acknowledgement,
            DispatchAcknowledgement::Unconfirmed,
            "a rejected acknowledgement must leave the persisted receipt unchanged"
        );
    }

    #[test]
    fn missing_acp_model_leaves_the_receipt_unconfirmed() {
        let temp = tempfile::tempdir().expect("temp run dir");
        write_planned_receipt(temp.path(), "openai/gpt-5.2");

        assert!(!persist_acp_model_acknowledgement(temp.path(), None)
            .expect("no observation is a successful no-op"));

        assert_eq!(
            read_receipt(temp.path()).observed.acknowledgement,
            DispatchAcknowledgement::Unconfirmed
        );
    }

    /// #1250 discriminator: a dispatch that exits 0 but whose watchdog
    /// predicate verdict is FAILURE must be recorded as FAILED in the
    /// terminal `status.json` rewrite — NOT as COMPLETED.
    ///
    /// The watchdog branch in [`spawn_background_dispatch`] resolves the
    /// predicate via `resolve_completion_state("success", &verdict)` (the
    /// same machinery the `tachi_complete` seam uses). For a `Fail` verdict
    /// that resolved state is `TASK_STATE_FAILED` — see
    /// `predicate::tests::resolve_state_matrix_success_row`. This test fixes
    /// the contract end-to-end at the status-write helper: passing the
    /// resolved FAILED state alongside a raw `Some(0)` exit code must yield
    /// FAILED. Pre-fix the `match final_exit_code { Some(0) => COMPLETED }`
    /// branch overrode the predicate and produced COMPLETED — the exact
    /// accounting bug this issue closes.
    ///
    /// Deterministic: pure function only, no tokio task, no sleeps, no fs.
    #[test]
    fn exit_zero_with_predicate_resolved_failed_lands_failed_in_status() {
        // The resolved state the watchdog threads through when the predicate
        // intercepted a false success — exactly what
        // `resolve_completion_state("success", &PredicateVerdict::Fail(_))`
        // returns (first tuple element).
        let resolved_failed: &'static str = "TASK_STATE_FAILED";

        // Fixed behavior: the resolved predicate state wins over the raw
        // exit code, so exit-0-but-failed-predicate lands FAILED.
        assert_eq!(
            terminal_status_state(None, Some(resolved_failed), None, Some(0)),
            "TASK_STATE_FAILED",
            "exit-0 + predicate-resolved FAILED must stay FAILED in status.json"
        );

        // Regression guards — the fix must NOT widen into the non-predicate
        // paths. When the watchdog did not resolve a predicate state (run
        // closed via `tachi_complete`, or the crash/timeout sub-branch), the
        // pre-existing exit-code-derived mapping is preserved exactly.
        assert_eq!(
            terminal_status_state(None, None, None, Some(0)),
            "TASK_STATE_COMPLETED",
            "exit-0 without a resolved predicate state keeps the conservative COMPLETED"
        );
        assert_eq!(
            terminal_status_state(None, None, None, Some(1)),
            "TASK_STATE_FAILED",
            "non-zero exit without a resolved predicate state stays FAILED"
        );
        assert_eq!(
            terminal_status_state(None, None, None, None),
            "TASK_STATE_FAILED",
            "missing exit code (subprocess error) stays FAILED"
        );

        // Symmetry guard: a Pass verdict resolves to COMPLETED — the helper
        // must honor that too, not silently downgrade an earned success.
        let resolved_completed: &'static str = "TASK_STATE_COMPLETED";
        assert_eq!(
            terminal_status_state(None, Some(resolved_completed), None, Some(0)),
            "TASK_STATE_COMPLETED",
            "exit-0 + predicate-resolved COMPLETED stays COMPLETED"
        );
    }

    /// #1250 round 2 discriminator (the case round 1 missed): a run that
    /// closed via `tachi_complete(outcome="success")` whose predicate
    /// resolved FAILED, then exited 0 — the terminal `status.json` rewrite
    /// must stay FAILED, NOT collapse back to COMPLETED via the raw exit
    /// code.
    ///
    /// `complete_ops::handler.rs` already runs `resolve_completion_state`
    /// and calls `update_kanban_state(... new_state ...)` with the resolved
    /// FAILED before the agent exits. The watchdog spawn's kanban poll at
    /// `execution.rs:248-272` reads that FAILED back; `is_closed` is true;
    /// the watchdog branch (the ONLY path round 1 threaded into the helper)
    /// is skipped, leaving `watchdog_resolved_state = None`. Round 1 then
    /// fell through to the exit-code match and overwrote FAILED → COMPLETED.
    ///
    /// Round 2 closes the seam by also feeding the polled terminal state
    /// through `canonical_terminal_state` into `terminal_status_state`'s
    /// new middle parameter. This test exercises that path directly: with
    /// `watchdog_resolved = None` (watchdog did not run because the run was
    /// already closed) and `polled_terminal_state = Some(FAILED)`, exit 0
    /// must still produce FAILED.
    ///
    /// RED against b29a513f (round 1): the helper took only 2 args there
    /// (no `polled_terminal_state`), and the call site never populated any
    /// polled-state thread, so the same scenario yielded COMPLETED. This
    /// test cannot even compile against b29a513f — round 1 has no code
    /// path that exercises the `tachi_complete` seam at the helper level.
    ///
    /// Deterministic: pure function only, no tokio task, no sleeps, no fs.
    #[test]
    fn tachi_complete_success_with_predicate_failed_then_exit_zero_stays_failed() {
        // The polled kanban state when handler.rs intercepted a false
        // success: kanban was set to TASK_STATE_FAILED by the
        // `update_kanban_state(new_state=...)` call in `complete_ops::
        // handler.rs:420-426` after `resolve_completion_state("success",
        // &PredicateVerdict::Fail(_))` returned FAILED.
        let polled_failed = canonical_terminal_state(Some("TASK_STATE_FAILED"));
        assert_eq!(
            polled_failed,
            Some("TASK_STATE_FAILED"),
            "canon: a polled FAILED kanban state normalizes to its static literal"
        );

        // The bug scenario: watchdog did not run (run already closed via
        // tachi_complete, so `watchdog_resolved_state = None`), kanban was
        // set to FAILED by the tachi_complete path, agent then exited 0.
        // Pre-fix this returned COMPLETED; post-fix it returns FAILED.
        assert_eq!(
            terminal_status_state(None, None, polled_failed, Some(0)),
            "TASK_STATE_FAILED",
            "tachi_complete(success) + predicate FAILED + exit 0 must stay FAILED"
        );

        // Regression guards.
        // 1. A genuine COMPLETED self-report (predicate Pass) → COMPLETED
        //    still holds; the polled thread must not downgrade earned
        //    successes.
        let polled_completed = canonical_terminal_state(Some("TASK_STATE_COMPLETED"));
        assert_eq!(
            terminal_status_state(None, None, polled_completed, Some(0)),
            "TASK_STATE_COMPLETED",
            "tachi_complete(success) + predicate Pass + exit 0 stays COMPLETED"
        );

        // 2. A CANCELED self-report lands CANCELED, not FAILED/COMPLETED —
        //    the polled state must beat the exit code even for non-binary
        //    terminal states.
        let polled_canceled = canonical_terminal_state(Some("TASK_STATE_CANCELED"));
        assert_eq!(
            terminal_status_state(None, None, polled_canceled, Some(0)),
            "TASK_STATE_CANCELED",
            "tachi_complete canceled + exit 0 must stay CANCELED, not COMPLETED"
        );

        // 3. The watchdog-resolved thread still wins over the polled thread
        //    when both are somehow populated (defensive — they are
        //    mutually exclusive in the live code path, but the helper's
        //    priority must be deterministic).
        assert_eq!(
            terminal_status_state(
                None,
                Some("TASK_STATE_FAILED"),
                Some("TASK_STATE_COMPLETED"),
                Some(0)
            ),
            "TASK_STATE_FAILED",
            "watchdog-resolved state has priority over the polled state"
        );

        // 4. A non-terminal polled state (e.g. TASK_STATE_WORKING, which
        //    cannot survive the `is_closed` gate but could be observed
        //    transiently) maps to None and falls through to the exit code,
        //    preserving the conservative fallback.
        assert_eq!(
            canonical_terminal_state(Some("TASK_STATE_WORKING")),
            None,
            "canon: non-terminal polled states map to None"
        );
        assert_eq!(
            terminal_status_state(
                None,
                None,
                canonical_terminal_state(Some("TASK_STATE_WORKING")),
                Some(0)
            ),
            "TASK_STATE_COMPLETED",
            "non-terminal polled state falls through to the exit-code mapping"
        );
    }

    /// A deliberate `tachi_complete(outcome="partial")` writes
    /// INPUT_REQUIRED.  For this execution, that is a terminal close: the
    /// watchdog must not synthesize another outcome, and the final status
    /// rewrite must preserve the self-reported partial state over exit 0.
    #[test]
    fn tachi_complete_partial_then_exit_zero_stays_input_required() {
        let polled_partial = canonical_terminal_state(Some("TASK_STATE_INPUT_REQUIRED"));
        assert_eq!(
            polled_partial, None,
            "the ambiguous kanban spelling alone must not close an active plan review"
        );
        assert!(
            !(None::<&'static str>.is_some() || polled_partial.is_some()),
            "without the receipt, INPUT_REQUIRED must re-enter the watchdog path"
        );
        assert_eq!(
            terminal_closure_kind("TASK_STATE_INPUT_REQUIRED"),
            Some("partial"),
            "the final status rewrite must persist the partial discriminator"
        );
        assert_eq!(
            terminal_closure_kind("TASK_STATE_COMPLETED"),
            None,
            "ordinary terminal states must not carry partial closure metadata"
        );
    }

    /// #1254 discriminator: INPUT_REQUIRED in a live kanban card is also the
    /// ordinary plan-review waiting state.  It must not skip the watchdog or
    /// acquire the partial closure marker unless a valid receipt proves a
    /// deliberate `tachi_complete(partial)` close.
    #[test]
    fn ordinary_input_required_reenters_watchdog_without_synthetic_partial() {
        let temp = tempfile::tempdir().expect("temporary run directory");
        std::fs::write(
            temp.path().join("status.json"),
            serde_json::json!({
                "state": "TASK_STATE_INPUT_REQUIRED",
                "closure_kind": null,
            })
            .to_string(),
        )
        .expect("write ordinary plan-review status");

        let receipt_terminal_state = resolved_completion_terminal_state(temp.path())
            .expect("read ordinary plan-review status");
        let polled_terminal_state = canonical_terminal_state(Some("TASK_STATE_INPUT_REQUIRED"));
        assert_eq!(
            receipt_terminal_state, None,
            "ordinary plan-review status has no receipt-backed terminal outcome"
        );
        assert_eq!(
            polled_terminal_state, None,
            "ordinary INPUT_REQUIRED is not a directly closed kanban state"
        );
        assert!(
            !(receipt_terminal_state.is_some() || polled_terminal_state.is_some()),
            "the live watchdog gate must evaluate an active plan-review run"
        );

        // This is the exact final-write fallback after that live watchdog path
        // has not resolved a predicate outcome.  It must not manufacture a
        // partial closure simply because the card was awaiting input.
        let final_status =
            terminal_status_state(receipt_terminal_state, None, polled_terminal_state, Some(0));
        assert_eq!(
            final_status, "TASK_STATE_COMPLETED",
            "ordinary INPUT_REQUIRED must not become a synthetic partial terminal state"
        );
        assert_eq!(
            terminal_closure_kind(final_status),
            None,
            "the watchdog fallback must not persist closure_kind=partial"
        );
    }

    #[cfg(unix)]
    #[test]
    fn completion_receipt_reader_loudly_refuses_final_leaf_symlink() {
        use std::os::unix::fs::symlink;

        let root = tempfile::tempdir().expect("temporary root");
        let run_dir = root.path().join("run");
        std::fs::create_dir(&run_dir).expect("create run directory");
        let outside = root.path().join("outside-status.json");
        std::fs::write(
            &outside,
            serde_json::json!({
                "resolved_completion": {
                    "state": "TASK_STATE_COMPLETED",
                    "closure_kind": null,
                    "eval_ledger_id": "outside-eval",
                    "reviewed": true,
                    "recorded_at": "2026-07-19T00:00:00Z",
                }
            })
            .to_string(),
        )
        .expect("write outside status");
        symlink(&outside, run_dir.join("status.json")).expect("symlink status leaf");

        let error = resolved_completion_terminal_state(&run_dir)
            .expect_err("completion receipt symlink must be a loud refusal");
        assert!(
            error.contains("refusing descriptor-bound read"),
            "error={error}"
        );
    }

    /// A missing kanban card must not erase a deliberate partial close.  The
    /// completion handler records the resolved close in the run receipt first;
    /// the watchdog reads that receipt before consulting kanban, whose card may
    /// have been deleted or may be temporarily unavailable.
    #[test]
    fn partial_receipt_beats_missing_kanban_and_exit_zero() {
        let temp = tempfile::tempdir().expect("temporary run directory");
        std::fs::write(
            temp.path().join("status.json"),
            serde_json::json!({
                "resolved_completion": {
                    "state": "TASK_STATE_INPUT_REQUIRED",
                    "closure_kind": "partial",
                    "eval_ledger_id": "eval-partial-receipt",
                    "reviewed": true,
                    "recorded_at": "2026-07-19T00:00:00Z",
                }
            })
            .to_string(),
        )
        .expect("write resolved completion receipt");

        let receipt_terminal_state = resolved_completion_terminal_state(temp.path())
            .expect("read partial completion receipt");
        assert_eq!(
            receipt_terminal_state,
            Some("TASK_STATE_INPUT_REQUIRED"),
            "a valid partial receipt must remain terminal when kanban is missing"
        );
        assert_eq!(
            terminal_status_state(receipt_terminal_state, None, None, Some(0)),
            "TASK_STATE_INPUT_REQUIRED",
            "receipt-backed partial must beat missing kanban and an exit-zero fallback"
        );

        std::fs::write(
            temp.path().join("status.json"),
            serde_json::json!({
                "state": "TASK_STATE_INPUT_REQUIRED",
                "closure_kind": null,
            })
            .to_string(),
        )
        .expect("write ordinary plan-input status");
        assert_eq!(
            resolved_completion_terminal_state(temp.path())
                .expect("read ordinary plan-input status"),
            None,
            "ordinary plan input without a resolved-completion receipt must remain open"
        );

        std::fs::write(
            temp.path().join("status.json"),
            serde_json::json!({
                "resolved_completion": {
                    "state": "TASK_STATE_INPUT_REQUIRED",
                    "closure_kind": "partial",
                    "reviewed": true,
                    "recorded_at": "not-a-timestamp",
                }
            })
            .to_string(),
        )
        .expect("write incomplete partial receipt");
        assert_eq!(
            resolved_completion_terminal_state(temp.path())
                .expect("read incomplete partial receipt"),
            None,
            "an incomplete or garbled partial receipt must not close the watchdog"
        );

        std::fs::write(
            temp.path().join("status.json"),
            serde_json::json!({
                "resolved_completion": {
                    "state": "TASK_STATE_COMPLETED",
                    "closure_kind": "partial",
                    "eval_ledger_id": "eval-garbled-receipt",
                    "reviewed": true,
                    "recorded_at": "2026-07-19T00:00:00Z",
                }
            })
            .to_string(),
        )
        .expect("write garbled completed receipt");
        assert_eq!(
            resolved_completion_terminal_state(temp.path())
                .expect("read garbled completed receipt"),
            None,
            "a non-partial outcome carrying a partial marker must be rejected"
        );
    }

    #[test]
    fn pending_completion_recovery_is_a_watchdog_barrier() {
        let temp = tempfile::tempdir().expect("temporary run directory");
        let status = serde_json::json!({
            "state": "TASK_STATE_WORKING",
            "completion_recovery": {
                "status": "pending_canonical_outcome",
                "dispatch_outcome": {"recorded": false},
            },
        });
        std::fs::write(temp.path().join("status.json"), status.to_string())
            .expect("write pending completion recovery status");

        assert_eq!(
            completion_receipt_state(temp.path()).expect("read recovery receipt"),
            CompletionReceiptState::PendingRecovery,
            "the watchdog must stop before predicate or exit-code terminalization"
        );
        assert_eq!(
            pending_recovery_status_state(Some(&status)),
            "TASK_STATE_WORKING",
            "the finalizer must retain a non-terminal state while recovery is pending"
        );
    }
}

#[cfg(test)]
mod issue_1825_credential_cleanup_tests {
    use super::ManagedEphemeralCredentialCleanupObligation;

    #[test]
    fn managed_terminal_failures_always_select_ephemeral_credential_cleanup() {
        let managed = Some(ManagedEphemeralCredentialCleanupObligation::Required);
        let non_managed: Option<ManagedEphemeralCredentialCleanupObligation> = None;
        assert!(managed.is_some());
        assert!(non_managed.is_none());
    }
}
