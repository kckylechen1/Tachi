use super::super::acp_native::{
    is_native_acp_transport, run_native_acp_dispatch, NativeAcpRunSpec,
};
use super::super::acpx::{is_acpx_transport, persist_acpx_events_and_map};
use super::super::dispatch_v2::{append_trajectory_event, write_status_json};
use super::super::kanban_helpers::{get_kanban_state, should_cleanup_run, update_kanban_state};
use super::super::subprocess::{run_agent_subprocess, run_opencode_sop_subprocess, tail_chars};
use super::dedupe::release_flow_dispatch_slot;
use super::response_helpers::McpCleanup;
use crate::credential_profile::cleanup_ephemeral_credential_materializations;
use crate::{MemoryServer, SaveMemoryParams};
use chrono::Utc;
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::time::Duration;
use tachi_dispatch::{
    model_lineage_id, provider_model_parts, DispatchAcknowledgement, DispatchIdentityEffective,
    DispatchIdentityReceipt, UNKNOWN_IDENTITY,
};
use tokio::process::Command;

pub(super) enum DispatchExecution {
    Subprocess(Command),
    NativeAcp(NativeAcpRunSpec),
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
    pub(super) capability_bundle_card: Value,
    pub(super) feedback_rules_trace: Value,
    pub(super) harness_transport: String,
    pub(super) harness_server_url: Option<String>,
    pub(super) host_adapter: Option<String>,
    pub(super) opencode_sop_label: Option<String>,
    pub(super) execution_backend_metadata: Option<Value>,
    pub(super) execution: DispatchExecution,
    pub(super) flow_dispatch_slot: Option<PathBuf>,
    pub(super) mcp_config_path: Option<PathBuf>,
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
    let timeout_secs_for_spawn = ctx.timeout_secs;
    let timeout = ctx.timeout;
    let capability_bundle_card_for_spawn = ctx.capability_bundle_card;
    let feedback_rules_trace_for_spawn = ctx.feedback_rules_trace;
    let harness_transport_for_spawn = ctx.harness_transport;
    let harness_server_url_for_spawn = ctx.harness_server_url;
    let host_adapter_for_spawn = ctx.host_adapter;
    let opencode_sop_label_for_spawn = ctx.opencode_sop_label;
    let execution_backend_metadata_for_spawn = ctx.execution_backend_metadata;
    let execution_for_spawn = ctx.execution;
    let flow_dispatch_slot_for_spawn = ctx.flow_dispatch_slot;
    let mcp_config_path = ctx.mcp_config_path;

    tokio::task::spawn(async move {
        let _mcp_cleanup = McpCleanup(mcp_config_path);

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

        let result = match execution_for_spawn {
            DispatchExecution::Subprocess(cmd) if agent_for_watchdog == "opencode" => {
                run_opencode_sop_subprocess(
                    cmd,
                    timeout,
                    opencode_sop_label_for_spawn
                        .as_deref()
                        .unwrap_or("opencode_sop"),
                )
                .await
            }
            DispatchExecution::Subprocess(cmd) => run_agent_subprocess(cmd, timeout).await,
            DispatchExecution::NativeAcp(spec) => {
                run_native_acp_dispatch(
                    spec,
                    &workspace_dir_for_spawn,
                    &traj_path_for_spawn,
                    &d_id,
                    &agent_for_watchdog,
                    timeout,
                )
                .await
            }
        };
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
        if is_acpx_transport(&harness_transport_for_spawn) {
            match persist_acpx_events_and_map(
                &workspace_dir_for_spawn,
                &traj_path_for_spawn,
                &d_id,
                &agent_for_watchdog,
                &full_output,
            ) {
                Ok(summary) => {
                    if let Some(final_response) = summary.final_response.clone() {
                        full_output = final_response;
                    }
                    acpx_event_summary_json = Some(json!({
                        "events_file": summary.events_file.to_string_lossy(),
                        "mapped_events": summary.mapped_events,
                        "final_response_extracted": summary.final_response.is_some(),
                    }));
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
            let (exit_code, output_tail) = match &result {
                Err(e) => (None, e.chars().take(200).collect::<String>()),
                Ok(r) => (r.exit_code, tail_chars(&r.output, 500)),
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

        // Save full output to result.md for orchestrator eval
        {
            let result_path = workspace_dir.join("result.md");
            if let Err(err) =
                crate::utils::write_owner_only_file_atomic(&result_path, full_output.as_bytes())
            {
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
        }

        // --- WATCHDOG: check if sub-agent properly closed the loop ---
        // Poll for kanban state instead of a fixed sleep to avoid race conditions
        let (watchdog_polls, watchdog_interval) = watchdog_poll_config();
        let mut kanban_state = None;
        for _ in 0..watchdog_polls {
            tokio::time::sleep(watchdog_interval).await;
            let state = get_kanban_state(&server_clone, &d_id).await;
            if let Some(ref s) = state {
                if matches!(
                    s.as_str(),
                    "TASK_STATE_COMPLETED" | "TASK_STATE_FAILED" | "TASK_STATE_CANCELED"
                ) {
                    kanban_state = state;
                    break;
                }
            }
        }
        let kanban_state = match kanban_state {
            Some(s) => Some(s),
            None => get_kanban_state(&server_clone, &d_id).await,
        };
        let is_closed = matches!(
            kanban_state.as_deref(),
            Some("TASK_STATE_COMPLETED" | "TASK_STATE_FAILED" | "TASK_STATE_CANCELED")
        );
        if !is_closed {
            let exited_ok = matches!(&result, Ok(r) if r.exit_code == Some(0));

            if exited_ok {
                // exit_code=0 but no tachi_complete: sub-agent forgot to
                // close the loop. Apply the #878-A completion predicate when
                // declared — an unsatisfied artifact/output contract is a
                // FALSE SUCCESS and must land FAILED, not COMPLETED. When no
                // predicate is declared, keep the conservative unreviewed
                // COMPLETED (no synthesized success eval for routing stats).
                let tail = tail_chars(&full_output, 500);
                let (run_dir_opt, declared_pred, pred_cwd) =
                    crate::dispatch_ops::resolve_completion_predicate_context(
                        &server_clone.tachi_home_dir(),
                        &d_id,
                    );
                let empty_run = std::path::PathBuf::new();
                let run_dir = run_dir_opt.as_deref().unwrap_or(&empty_run);
                let output_for_pred = run_dir
                    .join("result.md")
                    .exists()
                    .then(|| std::fs::read_to_string(run_dir.join("result.md")).ok())
                    .flatten()
                    .unwrap_or_else(|| full_output.clone());
                let verdict = crate::dispatch_ops::evaluate_completion_predicate(
                    declared_pred.as_ref(),
                    run_dir,
                    pred_cwd.as_deref(),
                    &output_for_pred,
                );
                let (kanban_state, reviewed, override_reason) =
                    crate::dispatch_ops::resolve_completion_state("success", &verdict);
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

        let should_cleanup = match &result {
            Ok(r) => should_cleanup_run(r.exit_code, kanban_state.as_deref()),
            Err(_) => false,
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
        let prev_status =
            crate::task_lifecycle::read_json_file(&workspace_dir_for_spawn.join("status.json"))
                .ok()
                .flatten();
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

        write_status_json(
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
                "state": match final_exit_code {
                    Some(0) => "TASK_STATE_COMPLETED",
                    Some(_) => "TASK_STATE_FAILED",
                    None => "TASK_STATE_FAILED",
                },
                "updated_at": Utc::now().to_rfc3339(),
                "run_dir": workspace_dir_for_spawn.to_string_lossy(),
                "result_written": true,
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
                "capability_bundle": capability_bundle_card_for_spawn,
                "feedback_rules": feedback_rules_trace_for_spawn,
                "timeout_secs": timeout_secs_for_spawn,
            })),
        );

        if should_cleanup {
            let credential_cleanup = server_clone.with_global_store(|store| {
                cleanup_ephemeral_credential_materializations(
                    store,
                    &workspace_dir_for_spawn,
                    false,
                )
            });
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
        }
        release_flow_dispatch_slot(flow_dispatch_slot_for_spawn);
    });
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
    let status_path = run_dir.join("status.json");
    let mut status = crate::task_lifecycle::read_json_file(&status_path)?
        .ok_or_else(|| format!("ACP acknowledgement requires {}", status_path.display()))?;
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
}
