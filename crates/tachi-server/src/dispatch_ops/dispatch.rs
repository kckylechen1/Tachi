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

/// Single-sourced from `tachi_dispatch::transport_kind` (#894 S2d) so the
/// transport alias lists used by the authority compiler and by the launch path
/// cannot drift.
fn is_opencode_serve_transport(transport: &str) -> bool {
    tachi_dispatch::transport_kind(transport) == tachi_dispatch::TransportKind::HarnessServe
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
mod authority;
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
use self::authority::{compile_dispatch_contract, contract_receipt};
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
    // Fail before ANY run directory, workspace, prompt, or child process is
    // created. This is the machine boundary; confirmation for a permitted L3
    // action remains the responsibility of that action's existing gate.
    let (host_profile, execution_level) =
        crate::host_profile::authorize_dispatch(params.execution_level)?;
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

    // 0a. Resolve the execution-environment binding through the fail-safe gate
    // (#894 S1) before ANY workspace/preflight/spawn work. env_id → cwd from the
    // managed lease; a bare cwd is rejected unless explicitly opted-in as
    // unmanaged. The resolved cwd replaces params.cwd for all downstream use
    // (status ledger, completion predicate, launcher), and the resolution is
    // stamped into status.json so the ledger records managed/unmanaged/default.
    let env_resolution = server.resolve_dispatch_env_binding(
        params.env_id.as_deref(),
        params.cwd.as_deref(),
        params.unmanaged_cwd.unwrap_or(false),
    )?;
    if let Some(resolved_cwd) = env_resolution.cwd() {
        params.cwd = Some(resolved_cwd.to_string());
    }
    let env_stamp = env_resolution.stamp();
    let env_id_stamp = env_resolution.env_id().map(str::to_string);

    // #1001: zero-ceremony presence claim — auto-register/heartbeat so a
    // briefing read from another session sees this dispatch is in flight.
    // Degrades to no-op on any storage error; never fails dispatch.
    crate::claims_ops::auto_register_or_heartbeat_claim(
        server,
        &crate::claims_ops::ClaimHookInput {
            issue_ref: params.issue_ref.clone(),
            flow_id: params.flow_id.clone(),
            dispatch_id: Some(dispatch_id.clone()),
            // TachiDispatchParams has no bare `branch` field (branch naming is
            // an internal detail of workspace/env provisioning, not a
            // dispatch param); env_resolution's cwd is the closest available
            // identity and is not branch-shaped, so this hook leaves branch
            // unset rather than guessing.
            branch: None,
            declared_file_scope: None,
        },
    );

    // 0b. Compile the effective-authority contract before ANY stage/preflight/
    // spawn work (#894 S2d). This is the single choke-point every dispatch
    // caller (`tachi_task`, arena spawn, shell dispatch, convoy, poke probes)
    // funnels through, so it runs before step 1 (workspace creation), before
    // prompt assembly, before the V2 plan stage's `ClaudePool` spawn, before
    // credential materialization, and before any builder's own preflight
    // (acpx's `node --version` probe, etc.).
    //
    // It subsumes the #894 S0 `validate_dispatch_sandbox_at_entry` gate: an
    // unhonorable sandbox request still fails closed with the same receipt, but
    // now an *omitted* sandbox is resolved from the effective profile instead
    // of falling through to codex's `workspace-write` default, a read-only
    // level nothing can enforce is refused here, and write-intent skills are
    // dropped from a read-only mount. The per-builder `reject_unsupported_sandbox`
    // / `validate_codex_sandbox` calls stay as defense-in-depth for any future
    // caller that reaches a builder without passing through this entry point.
    //
    // Fail-closed reality as shipped: the only row in `PROVIDER_QUALIFICATIONS`
    // (codex/cli) is `Unverified` — its kill-test is `#[ignore]`d and has never
    // been executed — so a dispatch that compiles to read-only AND can run shell
    // unattended is refused right here, on every backend. Run the kill-test and
    // certify the row to bring those lanes back (see `tachi_dispatch::authority`).
    let harness_transport = effective_harness_transport(&params, &agent_norm);
    let effective_contract = compile_dispatch_contract(
        &mut params,
        &agent_norm,
        &harness_transport,
        &resolved_profile,
        tachi_dispatch::PROVIDER_QUALIFICATIONS,
    )?;
    let authority_receipt = contract_receipt(&effective_contract);

    // 1. Create isolated workspace directory + MCP config
    //
    // `agent_seat` (round-3 fix, codex final review of #964/PR #1003, BUG
    // CP2): the seat identity written to `TACHI_AGENT_SEAT` for the spawned
    // worker, distinct from `params.tool_profile` (the capability/tool
    // surface). Round-2 derived this from `params.profile` (falling back to
    // `agent_norm`), but `params.profile` is a `DispatchProfile` — a
    // capability-surface selector like `codex_55_review`, not a seat — so
    // two concurrently dispatched workers on the same profile collided as
    // one seat (both write/consume the same `TACHI_AGENT_SEAT`, silently
    // cross-consuming each other's stickies). The dispatch `dispatch_id`
    // (already unique per lane — timestamp + sanitized agent + uuid suffix,
    // see `new_dispatch_id`) is available at this point precisely because
    // every dispatched worker gets one; using it as the seat makes collision
    // structurally impossible. A `to:`-addressed sticky can target a
    // dispatch id directly if ever needed.
    //
    // Collision-window caveat (round-4, codex review of #964/PR #1003 —
    // NOT a #964 regression, leader-adjudicated as out of scope here):
    // `new_dispatch_id` mints an 8-hex-char (32-bit) uuid suffix alongside a
    // same-second timestamp and the agent name, so two same-agent dispatches
    // landing in the same second could in theory mint the same `dispatch_id`
    // and therefore the same seat. `dispatch_id` already keys run
    // directories system-wide, so a collision here would break far more
    // than sticky delivery — this is a pre-existing property of
    // `new_dispatch_id`, not introduced by this fix, and widening its
    // entropy is a dispatch-system-wide change tracked as a follow-up, not
    // done in this PR.
    let agent_seat = Some(dispatch_id.as_str());
    let mcp_config_path = prepare_workspace_and_mcp(
        server,
        &workspace_dir,
        &dispatch_id,
        inject_tachi,
        inject_hub,
        params.tool_profile.as_deref(),
        agent_seat,
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
            "host_profile": host_profile.name(),
            "execution_level": execution_level.as_str(),
            "capability_bundle": Value::Null,
            "feedback_rules": Value::Null,
            "timeout_secs": timeout_secs_for_status,
            // #894 S2d: the authority receipt — compiled workspace authority,
            // who enforces it (a certified provider, nobody, or an operator
            // bypass), and which skills were excluded for asking for more than
            // the contract grants.
            "authority": authority_receipt.clone(),
            // #878-A: persist the working directory + completion predicate so
            // the complete gate (handler.rs) and the watchdog (execution.rs) can
            // machine-verify self-reported / exit-0 success against a contract.
            "cwd": params.cwd.clone(),
            "completion_predicate":
                serde_json::to_value(&params.completion_predicate).unwrap_or(Value::Null),
            // #774 round 3: stamp the dispatch's named project (if any) into
            // the receipt itself. Daemon-restart orphan recovery
            // (`recover_orphaned_dispatch_runs`) only has this on-disk
            // status.json to read from — it has no live `TachiDispatchParams`
            // — so unless `project` rides along in the receipt, a crash
            // mid-flight silently drops a named-project dispatch's terminal
            // outcome row into the default store instead of the named one,
            // splitting the same first-writer-wins invariant round 2 fixed
            // for the backend/preflight/watchdog/early-exit paths.
            "project": params.project.clone(),
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
            "host_profile": host_profile.name(),
            "execution_level": execution_level.as_str(),
            "capability_bundle": capability_bundle_card.clone(),
            "feedback_rules": feedback_rules_trace.clone(),
            "timeout_secs": timeout_secs_for_status,
            // #894 S2d: same authority receipt as the receipt-first seed above
            // (this write's `extra` is a fresh object, not a merge).
            "authority": authority_receipt.clone(),
            // #878-A: persist the working directory + completion predicate so
            // the complete gate (handler.rs) and the watchdog (execution.rs) can
            // machine-verify self-reported / exit-0 success against a contract.
            "cwd": params.cwd.clone(),
            // #894 S1: record how the working directory was bound so the ledger
            // distinguishes managed (leased) envs from opted-in unmanaged cwds
            // and the daemon default.
            "env": env_stamp,
            "env_id": env_id_stamp,
            "completion_predicate":
                serde_json::to_value(&params.completion_predicate).unwrap_or(Value::Null),
            // #774 round 3: same rationale as the receipt-first seed above —
            // re-stamped here since this write's `extra` is a fresh object,
            // not a merge with the seed's.
            "project": params.project.clone(),
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
            server,
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
            server,
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
            project: params.project.as_deref(),
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
        execution,
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
            close_kanban_row_on_early_exit(
                server,
                &dispatch_id,
                "post-init dispatch stage",
                params.project.as_deref(),
            )
            .await;
            return Err(e);
        }
    };

    // 8. Spawn background task with Watchdog
    let workspace_dir_for_response = workspace_dir.clone();
    spawn_background_dispatch(BackgroundDispatchContext {
        server: server.clone(),
        dispatch_id: dispatch_id.clone(),
        agent: agent_norm.clone(),
        project: params.project.clone(),
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
        authority: &authority_receipt,
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
