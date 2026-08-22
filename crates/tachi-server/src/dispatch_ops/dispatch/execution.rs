use super::super::acp_native::{
    is_native_acp_transport, run_native_acp_dispatch, NativeAcpRunSpec,
};
use super::super::acpx::{is_acpx_transport, persist_acpx_events_and_map};
#[cfg(test)]
use super::super::dispatch_v2::stamp_route_decision_id;
use super::super::dispatch_v2::{append_trajectory_event, status_json_lock_for, write_status_json};
use super::super::kanban_helpers::{get_kanban_state, should_cleanup_run, update_kanban_state};
use super::super::subprocess::{
    run_agent_subprocess, run_managed_custom_subprocess, run_opencode_sop_subprocess, tail_chars,
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

pub(super) enum DispatchExecution {
    Subprocess(Command),
    ManagedCustom(
        Command,
        tokio::sync::mpsc::Receiver<crate::managed_run_control::ManagedCancelCommand>,
    ),
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
    pub(super) managed_run_guard: Option<crate::managed_run_control::ManagedRunGuard>,
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
    let _managed_run_guard = ctx.managed_run_guard;

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
            DispatchExecution::ManagedCustom(cmd, receiver) => {
                run_managed_custom_subprocess(cmd, timeout, receiver, &workspace_dir_for_spawn)
                    .await
            }
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
        let mut receipt_terminal_state = None;
        let mut pending_completion_recovery = false;
        let mut receipt_read_error = None;
        let mut kanban_state = None;
        for _ in 0..watchdog_polls {
            tokio::time::sleep(watchdog_interval).await;
            match completion_receipt_state(&workspace_dir_for_spawn) {
                Ok(CompletionReceiptState::Terminal(receipt_state)) => {
                    receipt_terminal_state = Some(receipt_state);
                    break;
                }
                Ok(CompletionReceiptState::PendingRecovery) => {
                    pending_completion_recovery = true;
                    break;
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
            match completion_receipt_state(&workspace_dir_for_spawn) {
                Ok(CompletionReceiptState::Terminal(receipt_state)) => {
                    receipt_terminal_state = Some(receipt_state)
                }
                Ok(CompletionReceiptState::PendingRecovery) => pending_completion_recovery = true,
                Ok(CompletionReceiptState::Open) => {}
                Err(error) => receipt_read_error = Some(error),
            }
        }
        let polled_terminal_state = canonical_terminal_state(kanban_state.as_deref());
        let is_closed = !pending_completion_recovery
            && (receipt_terminal_state.is_some() || polled_terminal_state.is_some());
        if receipt_terminal_state == Some("TASK_STATE_CANCELED") {
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
                let tail = tail_chars(&full_output, 500);
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

        let should_cleanup = match &result {
            Err(error) if error == "managed_cancelled" => true,
            Ok(r) if !pending_completion_recovery => {
                should_cleanup_run(r.exit_code, kanban_state.as_deref())
            }
            Err(_) => false,
            Ok(_) => false,
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
        let (prev_status, final_status_read_error) =
            match crate::dispatch_ops::read_text_file_within(
                &workspace_dir_for_spawn,
                &status_path,
                WATCHDOG_STATUS_MAX_BYTES,
            ) {
                Ok(Some(raw)) => match serde_json::from_str::<Value>(&raw) {
                    Ok(status) => (Some(status), None),
                    Err(error) => (
                        None,
                        Some(format!(
                            "completion status artifact {} is not valid JSON: {error}",
                            status_path.display()
                        )),
                    ),
                },
                Ok(None) => (None, None),
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
        let final_status_state = if pending_completion_recovery {
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
                "state": final_status_state,
                "closure_kind": terminal_closure_kind(final_status_state),
                "updated_at": Utc::now().to_rfc3339(),
                "run_dir": workspace_dir_for_spawn.to_string_lossy(),
                "result_written": true,
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
    super::super::dispatch_v2::advance_status_revision(status_object)?;
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
    Open,
}

/// Read a handler-written completion receipt from the dispatch run. A pending
/// canonical-outcome recovery takes precedence over any stale terminal data:
/// it is an explicit barrier until tachi_complete reconciles the outcome row.
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
    if status.get("completion_recovery").is_some() {
        return Ok(CompletionReceiptState::PendingRecovery);
    }
    if status
        .get("cancellation")
        .and_then(Value::as_object)
        .is_some_and(|receipt| {
            receipt.get("receipt").and_then(Value::as_str) == Some("cancellation_confirmed")
                && matches!(
                    receipt.get("termination_proof").and_then(Value::as_str),
                    Some("spawn_suppressed" | "unix_process_group_absent")
                )
        })
    {
        return Ok(CompletionReceiptState::Terminal("TASK_STATE_CANCELED"));
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

#[cfg(test)]
fn resolved_completion_terminal_state(
    run_dir: &std::path::Path,
) -> Result<Option<&'static str>, String> {
    Ok(match completion_receipt_state(run_dir)? {
        CompletionReceiptState::Terminal(state) => Some(state),
        CompletionReceiptState::PendingRecovery | CompletionReceiptState::Open => None,
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
