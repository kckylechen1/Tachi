use super::acp_native::{build_native_acp_run_spec, is_native_acp_transport};
use super::acpx::{
    build_acpx_command, build_acpx_command_spec, is_acpx_transport, prepare_acpx_prompt,
};
use super::dispatch_v2::{
    append_trajectory_event, build_execute_prompt, parse_plan_sections, plan_review_required,
    plan_timeout_secs, run_plan_stage, v2_enabled_from_env, write_status_json, V2Decision,
};
use super::kanban_helpers::{close_kanban_row_on_early_exit, init_kanban_task};
use super::launcher::{
    build_claude_command, build_codex_command, build_custom_command, build_grok_command,
    build_kimi_command,
};
use super::mcp_config::generate_mcp_config;
use super::prompt::{assemble_prompt_with_trace, resolve_effective_skills};
use crate::agent_registry::{
    dispatch_agent_help_list, mcp_inject_supported, normalize_dispatch_agent_name,
    resolve_dispatch_agent,
};
use crate::credential_profile::{
    apply_credential_materialization, credential_materialize_report_json, default_credentials_dir,
    find_credential_profile, plan_credential_materialization_with_run_dir, profile_secret_names,
    CredentialApplyOptions, CredentialMaterializeReport,
};
use crate::dispatch_profile::{
    resolve_and_apply_dispatch_profile_for_server, ResolvedDispatchProfile,
};
use crate::tool_params::TachiDispatchParams;
use crate::vault_ops::read_unlocked_vault_secret;
use crate::MemoryServer;
use chrono::Utc;
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::time::Duration;

const DISPATCH_DEDUPE_STALE_LOCK_SECS: i64 = 300;

fn is_opencode_serve_transport(transport: &str) -> bool {
    matches!(
        transport.trim().to_ascii_lowercase().as_str(),
        "serve" | "opencode_serve" | "server"
    )
}

fn infer_harness_server_url(
    params: &TachiDispatchParams,
    harness_transport: &str,
) -> Option<String> {
    params.harness_server_url.clone().or_else(|| {
        if !is_opencode_serve_transport(harness_transport) {
            return None;
        }
        params
            .command
            .windows(2)
            .find(|pair| pair.first().is_some_and(|arg| arg == "--attach"))
            .and_then(|pair| pair.get(1))
            .cloned()
    })
}

fn opencode_serve_preflight_error(status: &Value, server_url: Option<&str>) -> String {
    let location = server_url.unwrap_or("missing harness_server_url");
    let server_auth = status
        .pointer("/layers/server_auth")
        .and_then(Value::as_str)
        .unwrap_or("unknown");
    let password_configured = status
        .get("server_password_configured")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let doc_error = status.get("doc_error").and_then(Value::as_str);
    let reason = if !password_configured
        && server_auth == "failed"
        && doc_error.is_some_and(|err| err.contains("HTTP 401"))
    {
        "daemon env lacks OPENCODE_SERVER_PASSWORD (probe got HTTP 401 on /doc)".to_string()
    } else if let Some(doc_error) = doc_error {
        doc_error.to_string()
    } else if let Some(readiness) = status.get("readiness").and_then(Value::as_str) {
        format!("readiness={readiness}")
    } else if status.is_null() {
        "harness_server_url is required for opencode_serve readiness checks".to_string()
    } else {
        "attach_ready=false".to_string()
    };

    format!(
        "opencode_serve attach not ready for {location}: {reason}. Set OPENCODE_SERVER_PASSWORD and restart the Tachi daemon, or pass OPENCODE_SERVER_PASSWORD via a credential profile for this dispatch."
    )
}

fn opencode_sop_label(agent_norm: &str, params: &TachiDispatchParams) -> Option<String> {
    if agent_norm != "opencode" {
        return None;
    }
    let route = crate::copilot_ops::build_task_brief_routing(&params.task, &[]);
    let selected = route
        .selected_sops
        .iter()
        .filter_map(|sop| sop.get("id").and_then(Value::as_str))
        .take(3)
        .collect::<Vec<_>>();
    if selected.is_empty() {
        Some(route.intent.to_string())
    } else {
        Some(format!("{}:{}", route.intent, selected.join(",")))
    }
}

pub(crate) struct DispatchResult {
    pub output: String,
    pub exit_code: Option<i32>,
}

mod artifacts;
mod backend;
mod backend_failure;
mod credential_apply;
mod credentials;
mod dedupe;
mod execution;
mod flow_setup;
mod harness_preflight;
mod plan_stage;
mod recovery;
mod response_helpers;
mod start;
mod workspace_setup;

#[cfg(test)]
mod tests;

use self::artifacts::{write_dispatch_artifacts, DispatchArtifactInputs, DispatchArtifacts};
use self::backend::{prepare_dispatch_backend, DispatchBackendContext, PreparedDispatchBackend};
use self::backend_failure::*;
use self::credential_apply::{
    apply_materialized_credentials, inject_legacy_vault_env, CredentialApplyInputs,
    CredentialApplyOutcome,
};
use self::credentials::*;
use self::dedupe::*;
use self::execution::{spawn_background_dispatch, BackgroundDispatchContext, DispatchExecution};
use self::flow_setup::{init_kanban_and_flow, FlowSetupInputs};
use self::harness_preflight::{run_harness_preflight, HarnessPreflightInputs};
use self::plan_stage::{run_v2_plan_stage, PlanStageInputs};
use self::response_helpers::*;
use self::start::*;
use self::workspace_setup::prepare_workspace_and_mcp;

#[cfg(test)]
pub(crate) use self::credentials::apply_unlocked_vault_env;
pub(crate) use self::dedupe::new_dispatch_id;
pub(crate) use self::recovery::recover_orphaned_dispatch_runs;

// ─── Main dispatch handler ───────────────────────────────────────────────────

/// Compute the effective harness transport from `params` alone — pure,
/// side-effect-free string/vec inspection. Hoisted to run immediately after
/// `resolve_dispatch_start` (before any stage/preflight/spawn work) so the
/// entry-point sandbox validation below knows which transport a request will
/// actually reach.
fn effective_harness_transport(params: &TachiDispatchParams, agent_norm: &str) -> String {
    params.harness_transport.clone().unwrap_or_else(|| {
        if agent_norm == "custom"
            && params.command.first().is_some_and(|cmd| cmd == "opencode")
            && params.command.iter().any(|arg| arg == "--attach")
        {
            "opencode_serve".to_string()
        } else {
            "cli".to_string()
        }
    })
}

/// #894 S0 round 2 (cross-vendor review): the single choke-point for
/// `params.sandbox` validation. Every dispatch caller (`tachi_task`, arena
/// spawn, shell dispatch, convoy, poke probes) funnels through
/// `handle_tachi_dispatch`, so running this check here — before step 1
/// (workspace creation), before prompt assembly, before the V2 plan stage's
/// `ClaudePool` spawn, and before any builder's own preflight (acpx's `node
/// --version` probe, etc.) — closes the whole class of "expensive work
/// happens before a doomed-to-fail sandbox request is rejected" findings in
/// one place. The per-builder calls (`build_codex_launch`,
/// `build_claude_launch`, `acpx::build_acpx_command_spec`,
/// `acp_native::build_native_acp_run_spec`, ...) all still run their own copy
/// of this check — kept intentionally as defense-in-depth, not removed, in
/// case a future caller reaches a builder through a path that bypasses this
/// entry point.
fn validate_dispatch_sandbox_at_entry(
    agent_norm: &str,
    harness_transport: &str,
    sandbox: Option<&str>,
) -> Result<(), String> {
    // Transport takes priority over agent name for the error label, matching
    // the per-builder wording exactly: even a codex agent has no sandbox
    // primitive once it's routed through acpx or native-ACP.
    if is_acpx_transport(harness_transport) {
        return tachi_dispatch::reject_unsupported_sandbox("acpx", sandbox);
    }
    if is_native_acp_transport(harness_transport) {
        return tachi_dispatch::reject_unsupported_sandbox("acp-native", sandbox);
    }
    if agent_norm == "codex" {
        // Codex is the one backend with a real sandbox primitive. Validate
        // its enum here too (not just inside `build_codex_launch`) so a typo
        // is rejected before Stage 1 spends a ClaudePool call.
        return match sandbox.map(str::trim) {
            None => Ok(()),
            Some("") => Err(
                "invalid codex --sandbox value: blank/whitespace-only sandbox request is malformed input, not equivalent to omitting it (fail-closed, #894 S0)"
                    .to_string(),
            ),
            Some(value) => tachi_dispatch::validate_codex_sandbox(value).map(|_| ()),
        };
    }
    // Every other backend (claude, grok, kimi, custom/opencode) has no
    // sandbox concept.
    tachi_dispatch::reject_unsupported_sandbox(agent_norm, sandbox)
}

/// #971 review-fix (F3): output of the guarded post-BOARD-FIRST-init
/// section in `handle_tachi_dispatch`. Either the plan stage's own
/// pending-review early-response fires (`EarlyResponse`), or every fallible
/// stage succeeded and execution is ready to spawn (`Ready`).
enum PostInitDispatchOutcome {
    EarlyResponse(String),
    Ready(Box<ReadyDispatch>),
}

/// Everything the post-guard spawn/response-building code (steps 8-9) needs,
/// carried out of the guarded `async` block in one bundle.
struct ReadyDispatch {
    execution: DispatchExecution,
    execution_backend_name: Option<&'static str>,
    execution_backend_metadata: Option<Value>,
    acpx_enabled: bool,
    native_acp_enabled: bool,
    credentials: CredentialApplyOutcome,
    plan_duration_ms: Option<u64>,
    plan_generated_at: Option<String>,
    flow_dispatch_slot: Option<PathBuf>,
}

pub(crate) async fn handle_tachi_dispatch(
    server: &MemoryServer,
    mut params: TachiDispatchParams,
) -> Result<String, String> {
    let now = Utc::now();
    let DispatchStart {
        dispatch_id,
        agent_norm,
        resolved_profile,
        profile_payload,
        timeout_secs_for_status,
        timeout,
        inject_tachi,
        inject_hub,
        workspace_dir,
        host_adapter,
    } = resolve_dispatch_start(server, &mut params, now)?;

    // 0. Validate `params.sandbox` before ANY stage/preflight/spawn work.
    let harness_transport = effective_harness_transport(&params, &agent_norm);
    validate_dispatch_sandbox_at_entry(&agent_norm, &harness_transport, params.sandbox.as_deref())?;

    // 1. Create isolated workspace directory + MCP config
    let mcp_config_path = prepare_workspace_and_mcp(
        server,
        &workspace_dir,
        &dispatch_id,
        inject_tachi,
        inject_hub,
        params.tool_profile.as_deref(),
        &params.allowed_mcp_servers,
    )
    .await?;

    // ─── #971 RECEIPT-FIRST ────────────────────────────────────────────────
    // Seed status.json + the first trajectory event immediately after the
    // run directory exists, BEFORE prompt assembly / the V2 plan stage's
    // (up to 180s) LLM call. External pollers must see *something* the
    // instant a dispatch is accepted, not only after the plan stage
    // succeeds. Every field serialized here is available straight off
    // `params` + `DispatchStart` — no prompt/artifact/plan dependency.
    // `v2` is not yet decided (that needs `params.stage`, which IS already
    // resolved) so compute it early too; capability_bundle/feedback_rules
    // are not known yet (they come from `assemble_prompt_with_trace` /
    // `write_dispatch_artifacts` below) and are seeded as neutral
    // "pending" placeholders here, then overwritten by the existing
    // post-artifacts `write_status_json` call once real values exist.
    let harness_server_url = infer_harness_server_url(&params, &harness_transport);
    let v2_decision = v2_enabled_from_env(params.stage.as_deref());
    let v2 = matches!(v2_decision, V2Decision::Enabled);

    write_status_json(
        &workspace_dir,
        &dispatch_id,
        v2,
        None,
        None,
        if v2 { "pending" } else { "n/a" },
        None,
        None,
        None,
        None,
        Some(json!({
            "agent": agent_norm.clone(),
            "task": params.task.clone(),
            "state": "TASK_STATE_WORKING",
            "updated_at": Utc::now().to_rfc3339(),
            "run_dir": workspace_dir.to_string_lossy(),
            "result_written": false,
            "harness_transport": harness_transport.clone(),
            "harness_server_url": harness_server_url.clone(),
            "host_adapter": host_adapter.clone(),
            "capability_bundle": Value::Null,
            "feedback_rules": Value::Null,
            "timeout_secs": timeout_secs_for_status,
            // #878-A: persist the working directory + completion predicate so
            // the complete gate (handler.rs) and the watchdog (execution.rs) can
            // machine-verify self-reported / exit-0 success against a contract.
            "cwd": params.cwd.clone(),
            "completion_predicate":
                serde_json::to_value(&params.completion_predicate).unwrap_or(Value::Null),
        })),
    );

    // Trajectory file does not exist yet (it is created by
    // `write_dispatch_artifacts` below) — `append_trajectory_event` creates
    // it on first write, and `write_dispatch_artifacts` was adapted to
    // APPEND its `dispatch_started` event instead of truncating, so this
    // "dispatch_received" line is preserved as the first trajectory event.
    let trajectory_path_early = workspace_dir.join("trajectory.jsonl");
    append_trajectory_event(
        &trajectory_path_early,
        json!({
            "event": "dispatch_received",
            "dispatch_id": dispatch_id,
            "timestamp": Utc::now().to_rfc3339(),
        }),
    );

    // 2. Assemble prompt & write audit files to workspace
    let prompt_assembly = assemble_prompt_with_trace(server, &params).await;
    let base_prompt = prompt_assembly.prompt.clone();
    let (effective_skills_for_files, _) = resolve_effective_skills(&params);

    let DispatchArtifacts {
        plan_path,
        prompt_md_path,
        context_md_path,
        trajectory_path,
        capability_bundle_file,
        capability_bundle_card,
        feedback_rules_trace,
    } = write_dispatch_artifacts(DispatchArtifactInputs {
        workspace_dir: &workspace_dir,
        dispatch_id: &dispatch_id,
        agent_norm: &agent_norm,
        params: &params,
        base_prompt: &base_prompt,
        prompt_assembly: &prompt_assembly,
        effective_skills_for_files: &effective_skills_for_files,
        v2,
    })
    .await?;

    // Enrich status.json now that capability_bundle / feedback_rules are
    // known. Same call shape as the original single seed — now the SECOND
    // write, not the first (receipt-first seed above is the first).
    write_status_json(
        &workspace_dir,
        &dispatch_id,
        v2,
        None,
        None,
        if v2 { "pending" } else { "n/a" },
        None,
        None,
        None,
        None,
        Some(json!({
            "agent": agent_norm.clone(),
            "task": params.task.clone(),
            "state": "TASK_STATE_WORKING",
            "updated_at": Utc::now().to_rfc3339(),
            "run_dir": workspace_dir.to_string_lossy(),
            "result_written": false,
            "harness_transport": harness_transport.clone(),
            "harness_server_url": harness_server_url.clone(),
            "host_adapter": host_adapter.clone(),
            "capability_bundle": capability_bundle_card.clone(),
            "feedback_rules": feedback_rules_trace.clone(),
            "timeout_secs": timeout_secs_for_status,
            // #878-A: persist the working directory + completion predicate so
            // the complete gate (handler.rs) and the watchdog (execution.rs) can
            // machine-verify self-reported / exit-0 success against a contract.
            "cwd": params.cwd.clone(),
            "completion_predicate":
                serde_json::to_value(&params.completion_predicate).unwrap_or(Value::Null),
        })),
    );

    // 3. ─── #971 BOARD-FIRST ────────────────────────────────────────────
    // Initialize the kanban row BEFORE the V2 plan stage (which can block
    // for up to 180s on an LLM call) so board/status pollers see a row
    // immediately, not only after planning succeeds. Artifacts (including
    // the V1-placeholder plan.md) are already written above either way, so
    // `init_kanban_task`'s `plan_path` argument is always valid here — for
    // V2 it currently points at the not-yet-overwritten placeholder, not
    // the real LLM plan; that's an accepted, documented consequence of
    // moving this earlier (see report), not a functional break: V2 later
    // overwrites plan.md in place at the same path once Stage 1 completes.
    init_kanban_and_flow(FlowSetupInputs {
        server,
        dispatch_id: &dispatch_id,
        params: &params,
        agent_norm: &agent_norm,
        plan_path: &plan_path,
        workspace_dir: &workspace_dir,
        prompt_md_path: &prompt_md_path,
        context_md_path: &context_md_path,
        trajectory_path: &trajectory_path,
        capability_bundle_card: &capability_bundle_card,
        capability_bundle_file: &capability_bundle_file,
        evidence_required: &resolved_profile.evidence_required,
        route_explanation: &resolved_profile.route_explanation,
    })
    .await?;

    // ─── #971 review-fix (F3) ────────────────────────────────────────────
    // Structural guard: everything from here through the success
    // return/spawn handoff below runs inside this `async` block so that
    // ANY `?` exit within it (Stage-1 plan, backend prep, credential
    // materialization, harness preflight, slot reservation) is caught at
    // ONE boundary and closes the BOARD-FIRST kanban row seeded above,
    // instead of leaking it in TASK_STATE_WORKING per uncovered callsite.
    // `close_kanban_row_on_early_exit` is idempotent-safe (checks current
    // state first) so it does not clobber branches — like the plan stage's
    // own failure/timeout paths and the pending-review path (F2) — that
    // already close/transition the row themselves before their `?`
    // propagates out of this block. A real `Drop` guard can't `.await`,
    // so this "wrap the fallible section, match on Err" shape is the
    // idiom that covers all current exits without per-callsite tracking.
    let post_init: Result<PostInitDispatchOutcome, String> = async {
        // 4. Stage 1 (V2 only): generate plan via ClaudePool. On failure/timeout
        // the kanban row created above must not be left orphaned in
        // TASK_STATE_WORKING — `run_v2_plan_stage`'s failure branches now close
        // it directly (see plan_stage.rs) ahead of this guard ever seeing them.
        let plan_stage_outcome = run_v2_plan_stage(PlanStageInputs {
            server,
            params: &params,
            dispatch_id: &dispatch_id,
            agent_norm: &agent_norm,
            resolved_profile: &resolved_profile,
            profile_payload: &profile_payload,
            base_prompt: &base_prompt,
            plan_path: &plan_path,
            prompt_md_path: &prompt_md_path,
            context_md_path: &context_md_path,
            trajectory_path: &trajectory_path,
            workspace_dir: &workspace_dir,
            capability_bundle_card: &capability_bundle_card,
            capability_bundle_file: &capability_bundle_file,
            feedback_rules_trace: &feedback_rules_trace,
            v2_decision,
        })
        .await?;
        if let Some(early_response) = plan_stage_outcome.early_response {
            return Ok(PostInitDispatchOutcome::EarlyResponse(early_response));
        }
        let prompt = plan_stage_outcome.prompt;
        let plan_duration_ms = plan_stage_outcome.plan_duration_ms;
        let plan_generated_at = plan_stage_outcome.plan_generated_at;

        // 5. Build execution backend
        let PreparedDispatchBackend {
            mut execution,
            execution_backend_name,
            execution_backend_metadata,
            acpx_enabled,
            native_acp_enabled,
        } = prepare_dispatch_backend(DispatchBackendContext {
            trajectory_path: &trajectory_path,
            workspace_dir: &workspace_dir,
            dispatch_id: &dispatch_id,
            agent_norm: &agent_norm,
            params: &params,
            prompt: &prompt,
            prompt_md_path: &prompt_md_path,
            mcp_config_path: mcp_config_path.as_ref(),
            v2,
            plan_generated_at: plan_generated_at.as_deref(),
            plan_duration_ms,
            harness_transport: &harness_transport,
            harness_server_url: &harness_server_url,
            capability_bundle_card: &capability_bundle_card,
            timeout_secs_for_status,
        })?;

        // 6. Inject legacy vault env + materialize credentials
        inject_legacy_vault_env(
            server,
            params.cwd.as_deref().map(std::path::Path::new),
            &mut execution,
            &trajectory_path,
            &dispatch_id,
            &agent_norm,
        );

        let credentials = apply_materialized_credentials(
            CredentialApplyInputs {
                server,
                params: &params,
                agent_norm: &agent_norm,
                selected_profile: resolved_profile.selected_profile.as_deref(),
                workspace_dir: &workspace_dir,
                trajectory_path: &trajectory_path,
                dispatch_id: &dispatch_id,
                v2,
                plan_generated_at: plan_generated_at.as_deref(),
                plan_duration_ms,
                harness_transport: &harness_transport,
                harness_server_url: &harness_server_url,
                host_adapter: &host_adapter,
                execution_backend_name,
                execution_backend_metadata: &execution_backend_metadata,
                acpx_enabled,
                native_acp_enabled,
                capability_bundle_card: &capability_bundle_card,
                timeout_secs_for_status,
            },
            &mut execution,
        )?;

        // 7. Harness preflight (opencode_serve only)
        run_harness_preflight(HarnessPreflightInputs {
            harness_transport: &harness_transport,
            harness_server_url: &harness_server_url,
            credential_env: &credentials.env,
            dispatch_id: &dispatch_id,
            agent_norm: &agent_norm,
            task: &params.task,
            trajectory_path: &trajectory_path,
            workspace_dir: &workspace_dir,
            v2,
            plan_generated_at: plan_generated_at.as_deref(),
            plan_duration_ms,
            host_adapter: &host_adapter,
            execution_backend_name,
            execution_backend_metadata: &execution_backend_metadata,
            acpx_enabled,
            native_acp_enabled,
            capability_bundle_card: &capability_bundle_card,
            timeout_secs_for_status,
        })?;

        let flow_dispatch_slot =
            reserve_dispatch_slot(params.flow_id.as_deref(), &params.task, &dispatch_id)?;

        Ok(PostInitDispatchOutcome::Ready(Box::new(ReadyDispatch {
            execution,
            execution_backend_name,
            execution_backend_metadata,
            acpx_enabled,
            native_acp_enabled,
            credentials,
            plan_duration_ms,
            plan_generated_at,
            flow_dispatch_slot,
        })))
    }
    .await;

    let ReadyDispatch {
        mut execution,
        execution_backend_name,
        execution_backend_metadata,
        acpx_enabled,
        native_acp_enabled,
        credentials,
        plan_duration_ms,
        plan_generated_at,
        flow_dispatch_slot,
    } = match post_init {
        Ok(PostInitDispatchOutcome::EarlyResponse(early_response)) => {
            return Ok(early_response);
        }
        Ok(PostInitDispatchOutcome::Ready(ready)) => *ready,
        Err(e) => {
            close_kanban_row_on_early_exit(server, &dispatch_id, "post-init dispatch stage").await;
            return Err(e);
        }
    };

    // 8. Spawn background task with Watchdog
    let workspace_dir_for_response = workspace_dir.clone();
    spawn_background_dispatch(BackgroundDispatchContext {
        server: server.clone(),
        dispatch_id: dispatch_id.clone(),
        agent: agent_norm.clone(),
        stage: params.stage.clone(),
        trajectory_path: trajectory_path.clone(),
        workspace_dir: workspace_dir.clone(),
        v2,
        plan_generated_at: plan_generated_at.clone(),
        plan_duration_ms,
        timeout_secs: timeout_secs_for_status,
        timeout,
        capability_bundle_card: capability_bundle_card.clone(),
        feedback_rules_trace: feedback_rules_trace.clone(),
        harness_transport: harness_transport.clone(),
        harness_server_url: harness_server_url.clone(),
        host_adapter: host_adapter.clone(),
        opencode_sop_label: opencode_sop_label(&agent_norm, &params),
        execution_backend_metadata: execution_backend_metadata.clone(),
        execution,
        flow_dispatch_slot,
        mcp_config_path,
    });

    // 9. Immediately return — main agent is unblocked!
    build_dispatch_response(DispatchResponseInputs {
        dispatch_id: &dispatch_id,
        agent_norm: &agent_norm,
        profile_payload: &profile_payload,
        resolved_profile: &resolved_profile,
        credential_reports_json: &credentials.reports_json,
        capability_bundle_card: &capability_bundle_card,
        capability_bundle_file: &capability_bundle_file,
        feedback_rules_trace: &feedback_rules_trace,
        harness_transport: &harness_transport,
        harness_server_url: &harness_server_url,
        host_adapter: &host_adapter,
        execution_backend_name,
        execution_backend_metadata: &execution_backend_metadata,
        acpx_enabled,
        native_acp_enabled,
        v2,
        plan_duration_ms,
        params: &params,
        plan_path: &plan_path,
        prompt_md_path: &prompt_md_path,
        context_md_path: &context_md_path,
        trajectory_path: &trajectory_path,
        workspace_dir: &workspace_dir_for_response,
    })
}
