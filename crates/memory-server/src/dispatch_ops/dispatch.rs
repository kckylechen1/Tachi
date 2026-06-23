use super::acp_native::{
    build_native_acp_run_spec, is_native_acp_transport, run_native_acp_dispatch, NativeAcpRunSpec,
};
use super::acpx::{
    build_acpx_command, build_acpx_command_spec, is_acpx_transport, persist_acpx_events_and_map,
    prepare_acpx_prompt,
};
use super::dispatch_v2::{
    append_trajectory_event, build_execute_prompt, parse_plan_sections, plan_review_required,
    plan_timeout_secs, run_plan_stage, v2_enabled_from_env, write_status_json, V2Decision,
};
use super::kanban_helpers::{
    get_kanban_state, init_kanban_task, should_cleanup_run, update_kanban_state,
};
use super::mcp_config::generate_mcp_config;
use super::prompt::{assemble_prompt_with_trace, resolve_effective_skills};
use super::subprocess::{
    build_claude_command, build_codex_command, build_custom_command, build_grok_command,
    build_kimi_command, run_agent_subprocess, tail_chars,
};
use crate::agent_registry::{
    dispatch_agent_help_list, mcp_inject_supported, resolve_dispatch_agent,
};
use crate::credential_profile::{
    apply_credential_materialization, cleanup_ephemeral_credential_materializations,
    credential_materialize_report_json, default_credentials_dir, find_credential_profile,
    plan_credential_materialization_with_run_dir, profile_secret_names, CredentialApplyOptions,
    CredentialMaterializeReport,
};
use crate::dispatch_profile::{
    resolve_and_apply_dispatch_profile_for_server, ResolvedDispatchProfile,
};
use crate::tool_params::TachiDispatchParams;
use crate::vault_ops::read_unlocked_vault_secret;
use crate::{MemoryServer, SaveMemoryParams};
use chrono::Utc;
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::time::Duration;
use tokio::process::Command;

const DISPATCH_DEDUPE_STALE_LOCK_SECS: i64 = 300;

struct DispatchStart {
    dispatch_id: String,
    agent_norm: String,
    resolved_profile: ResolvedDispatchProfile,
    profile_payload: Value,
    timeout_secs_for_status: u64,
    timeout: Duration,
    inject_tachi: bool,
    inject_hub: bool,
    workspace_dir: PathBuf,
}

// ─── Dispatch result ─────────────────────────────────────────────────────────

pub(crate) struct DispatchResult {
    pub output: String,
    pub exit_code: Option<i32>,
}

enum DispatchExecution {
    Subprocess(Command),
    NativeAcp(NativeAcpRunSpec),
}

struct ExecutionBackendPrepareFailure<'a> {
    trajectory_path: &'a Path,
    workspace_dir: &'a Path,
    dispatch_id: &'a str,
    agent_norm: &'a str,
    params: &'a TachiDispatchParams,
    backend: &'a str,
    error: &'a str,
    v2: bool,
    plan_generated_at: Option<&'a str>,
    plan_duration_ms: Option<u64>,
    harness_transport: &'a str,
    harness_server_url: &'a Option<String>,
    capability_bundle_card: &'a Value,
    timeout_secs_for_status: u64,
}

fn record_execution_backend_prepare_failure(ctx: ExecutionBackendPrepareFailure<'_>) {
    append_trajectory_event(
        ctx.trajectory_path,
        json!({
            "event": "execution_backend_prepare_failed",
            "dispatch_id": ctx.dispatch_id,
            "agent": ctx.agent_norm,
            "execution_backend": ctx.backend,
            "error": ctx.error,
            "timestamp": Utc::now().to_rfc3339(),
        }),
    );
    write_status_json(
        ctx.workspace_dir,
        ctx.dispatch_id,
        ctx.v2,
        ctx.plan_generated_at,
        None,
        if ctx.v2 { "approved" } else { "n/a" },
        Some(1),
        ctx.plan_duration_ms,
        None,
        ctx.plan_duration_ms,
        Some(json!({
            "agent": ctx.agent_norm,
            "task": ctx.params.task.clone(),
            "state": "TASK_STATE_FAILED",
            "updated_at": Utc::now().to_rfc3339(),
            "run_dir": ctx.workspace_dir.to_string_lossy(),
            "result_written": false,
            "harness_transport": ctx.harness_transport,
            "harness_server_url": ctx.harness_server_url,
            "execution_backend": ctx.backend,
            "capability_bundle": ctx.capability_bundle_card,
            "timeout_secs": ctx.timeout_secs_for_status,
            "error": ctx.error,
        })),
    );
}

fn resolve_dispatch_start(
    server: &MemoryServer,
    params: &mut TachiDispatchParams,
    now: chrono::DateTime<Utc>,
) -> Result<DispatchStart, String> {
    let resolved_profile = resolve_and_apply_dispatch_profile_for_server(server, params)?;
    let mut agent_norm = resolved_profile.agent.clone();
    let dispatch_id = new_dispatch_id(now, &agent_norm);

    agent_norm = if agent_norm.eq_ignore_ascii_case("custom") {
        "custom".to_string()
    } else if let Some(def) = resolve_dispatch_agent(&agent_norm) {
        def.name.to_string()
    } else {
        let agent = params.agent.as_deref().unwrap_or("");
        return Err(format!(
            "Unknown agent '{}'. Supported: {}",
            agent.trim(),
            dispatch_agent_help_list()
        ));
    };
    params.agent = Some(agent_norm.clone());

    let profile_payload =
        serde_json::to_value(&resolved_profile).unwrap_or_else(|_| json!({"agent": agent_norm}));
    let timeout_secs_for_status = params.timeout_secs;
    let timeout = Duration::from_secs(timeout_secs_for_status);
    let inject_tachi = params.inject_tachi_mcp.unwrap_or(false);
    let inject_hub = params.inject_hub_mcps.unwrap_or(false);

    // Validate backend/MCP compatibility before creating the run ledger. A
    // rejected dispatch should not leave an empty run directory with no status.
    if inject_tachi || inject_hub {
        if agent_norm == "custom" {
            return Err(
                "inject_tachi_mcp / inject_hub_mcps are not supported for the custom backend."
                    .to_string(),
            );
        }
        let def = resolve_dispatch_agent(&agent_norm).expect("resolved agent");
        if !mcp_inject_supported(def) {
            let hint = match def.name {
                "codex" => "Configure MCP servers in ~/.codex/config.toml instead, or dispatch with agent='claude' or 'grok'.",
                "kimi" => "Dispatch with agent='claude' or 'grok' for Tachi MCP injection.",
                _ => "Use an agent that supports --mcp-config.",
            };
            return Err(format!(
                "inject_tachi_mcp / inject_hub_mcps are not supported for the {} backend. {}",
                def.name, hint
            ));
        }
    }

    let workspace_dir = dispatch_runs_root().join(&dispatch_id);

    Ok(DispatchStart {
        dispatch_id,
        agent_norm,
        resolved_profile,
        profile_payload,
        timeout_secs_for_status,
        timeout,
        inject_tachi,
        inject_hub,
        workspace_dir,
    })
}

mod credentials;
mod dedupe;
mod recovery;
mod response_helpers;

#[cfg(test)]
mod tests;

use self::credentials::*;
use self::dedupe::*;
use self::response_helpers::*;

#[cfg(test)]
pub(crate) use self::credentials::apply_unlocked_vault_env;
pub(crate) use self::dedupe::new_dispatch_id;
pub(crate) use self::recovery::recover_orphaned_dispatch_runs;

// ─── Main dispatch handler ───────────────────────────────────────────────────

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
    } = resolve_dispatch_start(server, &mut params, now)?;

    // 1. Create isolated workspace directory
    tokio::fs::create_dir_all(&workspace_dir)
        .await
        .map_err(|e| format!("Failed to create workspace dir: {e}"))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        tokio::fs::set_permissions(&workspace_dir, std::fs::Permissions::from_mode(0o700))
            .await
            .map_err(|e| format!("Failed to set workspace dir permissions: {e}"))?;
    }

    // 2. Generate MCP config if requested
    let mcp_config_path = if inject_tachi || inject_hub {
        generate_mcp_config(
            server,
            &dispatch_id,
            inject_tachi,
            inject_hub,
            params.tool_profile.as_deref(),
            &params.allowed_mcp_servers,
        )
        .await?
    } else {
        None
    };

    // 3. Assemble prompt & write audit files to workspace
    let prompt_assembly = assemble_prompt_with_trace(server, &params).await;
    let base_prompt = prompt_assembly.prompt.clone();
    let (effective_skills_for_files, _) = resolve_effective_skills(&params);

    // ─── Dispatch V2 decision ────────────────────────────────────────────
    // V2 is opt-in. Default behaviour stays V1 (legacy single-stage).
    let v2_decision = v2_enabled_from_env(params.stage.as_deref());
    let v2 = matches!(v2_decision, V2Decision::Enabled);

    // The actual prompt fed to the executing agent. In V1 this is just the
    // assembled prompt. In V2 it is rewritten after Stage 1 succeeds.
    let mut prompt = base_prompt.clone();

    let plan_path = workspace_dir.join("plan.md");
    // V1 writes the assembled prompt as a placeholder plan.md (legacy);
    // V2 will overwrite this with the real LLM-generated plan below.
    crate::utils::write_owner_only_file_atomic(&plan_path, base_prompt.as_bytes())
        .map_err(|e| format!("Failed to write plan file: {e}"))?;

    // Write prompt.md (full assembled prompt for tracked run)
    let prompt_md_path = workspace_dir.join("prompt.md");
    tokio::fs::write(&prompt_md_path, &base_prompt)
        .await
        .map_err(|e| format!("Failed to write prompt.md: {e}"))?;

    let capability_bundle_path = workspace_dir.join("capability_bundle.json");
    let mut capability_bundle_trace = prompt_assembly.capability_bundle.clone();
    if let Some(obj) = capability_bundle_trace.as_object_mut() {
        obj.insert(
            "feedback_rules".to_string(),
            prompt_assembly.feedback_rules.clone(),
        );
    }
    let feedback_rules_trace = prompt_assembly.feedback_rules.clone();
    let capability_bundle_artifact = serde_json::to_string_pretty(&capability_bundle_trace)
        .map_err(|e| format!("Failed to serialize capability bundle artifact: {e}"))?;
    tokio::fs::write(&capability_bundle_path, capability_bundle_artifact)
        .await
        .map_err(|e| format!("Failed to write capability_bundle.json: {e}"))?;
    let capability_bundle_file = capability_bundle_path.to_string_lossy().to_string();
    let capability_bundle_card =
        capability_bundle_summary(&capability_bundle_trace, Some(&capability_bundle_file));

    // Write context.md (summary of injected context/skills — for MVP, same as prompt)
    let context_md_path = workspace_dir.join("context.md");
    let context_summary = {
        let mut sections = Vec::new();
        sections.push(format!("# Dispatch Context: {}", dispatch_id));
        sections.push(format!("Agent: {}", agent_norm));
        sections.push(format!(
            "Dispatch profile: {}",
            params.profile.as_deref().unwrap_or("none")
        ));
        sections.push(format!(
            "Tool profile: {}",
            params.tool_profile.as_deref().unwrap_or("none")
        ));
        if let Some(flow_id) = params.flow_id.as_deref() {
            sections.push(format!("Flow: {}", flow_id));
        }
        if let Some(issue_ref) = params.issue_ref.as_deref() {
            sections.push(format!("Issue: {}", issue_ref));
        }
        if let Some(pr_ref) = params.pr_ref.as_deref() {
            sections.push(format!("PR: {}", pr_ref));
        }
        sections.push(format!(
            "Stage: {}",
            params.stage.as_deref().unwrap_or("none")
        ));
        sections.push(format!("V2: {}", v2));
        sections.push(format!("Skills: {:?}", effective_skills_for_files));
        sections.push(format!(
            "Capability bundle: status={} requested={} injected={} artifact={}",
            capability_bundle_trace
                .get("status")
                .and_then(|value| value.as_str())
                .unwrap_or("unknown"),
            capability_bundle_trace
                .get("requested")
                .and_then(|value| value.as_bool())
                .unwrap_or(false),
            capability_bundle_trace
                .get("injected")
                .and_then(|value| value.as_bool())
                .unwrap_or(false),
            capability_bundle_file
        ));
        sections.push(String::new());
        sections.push(base_prompt.clone());
        sections.join("\n\n")
    };
    tokio::fs::write(&context_md_path, &context_summary)
        .await
        .map_err(|e| format!("Failed to write context.md: {e}"))?;

    // Write trajectory.jsonl — initial dispatch_started event
    let trajectory_path = workspace_dir.join("trajectory.jsonl");
    {
        let started_event = json!({
            "event": "dispatch_started",
            "dispatch_id": dispatch_id,
            "agent": agent_norm,
            "stage": params.stage,
            "profile": params.profile,
            "tool_profile": params.tool_profile,
            "mcp_access": params.mcp_access,
            "allowed_mcp_servers": params.allowed_mcp_servers,
            "issue_ref": params.issue_ref,
            "pr_ref": params.pr_ref,
            "flow_id": params.flow_id,
            "auto_capability_bundle": params.auto_capability_bundle,
            "capability_bundle": capability_bundle_card.clone(),
            "feedback_rules": feedback_rules_trace.clone(),
            "v2": v2,
            "timestamp": Utc::now().to_rfc3339(),
        });
        let line = serde_json::to_string(&started_event)
            .map_err(|e| format!("Failed to serialize started event: {e}"))?;
        tokio::fs::write(&trajectory_path, format!("{}\n", line))
            .await
            .map_err(|e| format!("Failed to write trajectory.jsonl: {e}"))?;
        let progress_path = workspace_dir.join("progress.jsonl");
        tokio::fs::write(&progress_path, format!("{}\n", line))
            .await
            .map_err(|e| format!("Failed to write progress.jsonl: {e}"))?;
    }

    let harness_transport = params.harness_transport.clone().unwrap_or_else(|| {
        if agent_norm == "custom"
            && params.command.first().is_some_and(|cmd| cmd == "opencode")
            && params.command.iter().any(|arg| arg == "--attach")
        {
            "opencode_serve".to_string()
        } else {
            "cli".to_string()
        }
    });
    let harness_server_url = params.harness_server_url.clone();

    // Seed status.json so external pollers see something immediately.
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
            "capability_bundle": capability_bundle_card.clone(),
            "feedback_rules": feedback_rules_trace.clone(),
            "timeout_secs": timeout_secs_for_status,
        })),
    );

    // ─── Stage 1 (V2 only): generate plan via ClaudePool ────────────────
    let mut plan_duration_ms: Option<u64> = None;
    let mut plan_generated_at: Option<String> = None;

    if v2 {
        let label = format!("dispatch-plan-{}", &dispatch_id);
        let plan_timeout = Duration::from_secs(plan_timeout_secs());
        let plan_fut = run_plan_stage(server, &params.task, &label);
        let plan_outcome = match tokio::time::timeout(plan_timeout, plan_fut).await {
            Ok(Ok(p)) => p,
            Ok(Err(e)) => {
                append_trajectory_event(
                    &trajectory_path,
                    json!({
                        "event": "plan_failed",
                        "dispatch_id": dispatch_id,
                        "timestamp": Utc::now().to_rfc3339(),
                        "error": e,
                    }),
                );
                write_status_json(
                    &workspace_dir,
                    &dispatch_id,
                    true,
                    None,
                    None,
                    "failed",
                    Some(1),
                    None,
                    None,
                    None,
                    Some(json!({
                        "error": e,
                        "capability_bundle": capability_bundle_card.clone(),
                    })),
                );
                return Err(e);
            }
            Err(_) => {
                let e = format!(
                    "dispatch v2 stage1 (plan) timed out after {}s",
                    plan_timeout.as_secs()
                );
                append_trajectory_event(
                    &trajectory_path,
                    json!({
                        "event": "plan_failed",
                        "dispatch_id": dispatch_id,
                        "timestamp": Utc::now().to_rfc3339(),
                        "error": e,
                    }),
                );
                write_status_json(
                    &workspace_dir,
                    &dispatch_id,
                    true,
                    None,
                    None,
                    "failed",
                    Some(1),
                    None,
                    None,
                    None,
                    Some(json!({
                        "error": e,
                        "capability_bundle": capability_bundle_card.clone(),
                    })),
                );
                return Err(e);
            }
        };

        // Persist plan.md (overwrites the V1 placeholder).
        crate::utils::write_owner_only_file_atomic(&plan_path, plan_outcome.plan_md.as_bytes())
            .map_err(|e| format!("Failed to write plan.md: {e}"))?;
        let sections = parse_plan_sections(&plan_outcome.plan_md);

        plan_duration_ms = Some(plan_outcome.duration_ms);
        let pgen_at = Utc::now().to_rfc3339();
        plan_generated_at = Some(pgen_at.clone());
        append_trajectory_event(
            &trajectory_path,
            json!({
                "event": "plan_generated",
                "dispatch_id": dispatch_id,
                "timestamp": pgen_at,
                "duration_ms": plan_outcome.duration_ms,
                "bytes": plan_outcome.plan_md.len(),
                "sections_complete": sections.is_complete(),
            }),
        );

        // ─── Optional review gate ────────────────────────────────────
        if plan_review_required() {
            write_status_json(
                &workspace_dir,
                &dispatch_id,
                true,
                plan_generated_at.as_deref(),
                None,
                "pending_review",
                None,
                plan_duration_ms,
                None,
                plan_duration_ms,
                Some(json!({
                    "capability_bundle": capability_bundle_card.clone(),
                })),
            );
            append_trajectory_event(
                &trajectory_path,
                json!({
                    "event": "plan_pending_review",
                    "dispatch_id": dispatch_id,
                    "timestamp": Utc::now().to_rfc3339(),
                }),
            );
            let response = json!({
                "dispatch_id": dispatch_id,
                "task": {
                    "id": dispatch_id,
                    "status": { "state": "TASK_STATE_PENDING_REVIEW" },
                },
                "agent": agent_norm,
                "profile": profile_payload,
                "selected_profile": resolved_profile.selected_profile,
                "tool_access": resolved_profile.mcp_access,
                "dispatch_profile": resolved_profile.mbit_card,
                "route_explanation": resolved_profile.route_explanation,
                "fallback_chain": resolved_profile.fallback_chain,
                "issue_ref": params.issue_ref,
                "pr_ref": params.pr_ref,
                "flow_id": params.flow_id,
                "auto_capability_bundle": resolved_profile.auto_capability_bundle,
                "capability_bundle": capability_bundle_card,
                "capability_bundle_file": capability_bundle_file,
                "feedback_rules": feedback_rules_trace.clone(),
                "v2": true,
                "plan_review_status": "pending_review",
                "message": "Plan generated. DISPATCH_V2_PLAN_REVIEW=true — execute stage paused. Audit plan.md and re-dispatch with the env var unset to proceed.",
                "suggested_complete_command": suggested_complete_payload(&dispatch_id, &agent_norm, &params),
                "plan_file": plan_path.to_string_lossy(),
                "prompt_file": prompt_md_path.to_string_lossy(),
                "context_file": context_md_path.to_string_lossy(),
                "trajectory_file": trajectory_path.to_string_lossy(),
                "run_dir": workspace_dir.to_string_lossy(),
            });
            return Ok(
                serde_json::to_string(&response).unwrap_or_else(|e| format!("serialize: {e}"))
            );
        }

        // Auto-approve: rewrite the prompt fed to the executing agent so
        // it contains the plan + implement-plan skill.
        append_trajectory_event(
            &trajectory_path,
            json!({
                "event": "plan_approved",
                "dispatch_id": dispatch_id,
                "timestamp": Utc::now().to_rfc3339(),
                "auto": true,
            }),
        );
        prompt = build_execute_prompt(&plan_outcome.plan_md, &params.task, &base_prompt);
    }

    // 4. Initialize kanban task
    init_kanban_task(
        server,
        &dispatch_id,
        &params,
        Some(&plan_path.to_string_lossy()),
    )
    .await?;
    if let Some(flow_id) = params.flow_id.as_deref().filter(|id| !id.trim().is_empty()) {
        if let Err(error) = crate::task_lifecycle::mark_task_dispatch(
            flow_id,
            &dispatch_id,
            json!({
                "agent": agent_norm.clone(),
                "profile": params.profile.clone(),
                "tool_profile": params.tool_profile.clone(),
                "stage": params.stage.clone(),
                "task": params.task.clone(),
                "issue_ref": params.issue_ref.clone(),
                "pr_ref": params.pr_ref.clone(),
                "run_dir": workspace_dir.to_string_lossy(),
                "prompt_file": prompt_md_path.to_string_lossy(),
                "context_file": context_md_path.to_string_lossy(),
                "trajectory_file": trajectory_path.to_string_lossy(),
                "plan_file": plan_path.to_string_lossy(),
                "capability_bundle": capability_bundle_card.clone(),
                "capability_bundle_file": capability_bundle_file.clone(),
                "evidence_required": resolved_profile.evidence_required.clone(),
                "route_explanation": resolved_profile.route_explanation.clone(),
                "suggested_complete": suggested_complete_payload(&dispatch_id, &agent_norm, &params),
            }),
        ) {
            append_trajectory_event(
                &trajectory_path,
                json!({
                    "event": "flow_dispatch_marker_failed",
                    "dispatch_id": dispatch_id,
                    "flow_id": flow_id,
                    "error": error,
                    "timestamp": Utc::now().to_rfc3339(),
                }),
            );
        }
    }

    // 5. Build execution backend
    let acpx_enabled = is_acpx_transport(&harness_transport);
    let native_acp_enabled = is_native_acp_transport(&harness_transport);
    let mut execution_backend_metadata: Option<serde_json::Value> = None;
    let execution_backend_name = if acpx_enabled {
        Some("acpx")
    } else if native_acp_enabled {
        Some("acp_native")
    } else {
        None
    };
    let record_backend_prepare_failure = |backend: &str, err: &str| {
        record_execution_backend_prepare_failure(ExecutionBackendPrepareFailure {
            trajectory_path: &trajectory_path,
            workspace_dir: &workspace_dir,
            dispatch_id: &dispatch_id,
            agent_norm: &agent_norm,
            params: &params,
            backend,
            error: err,
            v2,
            plan_generated_at: plan_generated_at.as_deref(),
            plan_duration_ms,
            harness_transport: &harness_transport,
            harness_server_url: &harness_server_url,
            capability_bundle_card: &capability_bundle_card,
            timeout_secs_for_status,
        });
    };
    let mut execution = if acpx_enabled {
        let acpx_prompt_path = match prepare_acpx_prompt(&prompt_md_path, &prompt) {
            Ok(path) => path,
            Err(err) => {
                record_backend_prepare_failure("acpx", &err);
                return Err(err);
            }
        };
        let acpx_spec = match build_acpx_command_spec(&params, &agent_norm, &acpx_prompt_path) {
            Ok(spec) => spec,
            Err(err) => {
                record_backend_prepare_failure("acpx", &err);
                return Err(err);
            }
        };
        append_trajectory_event(
            &trajectory_path,
            json!({
                "event": "execution_backend_prepared",
                "dispatch_id": dispatch_id,
                "agent": agent_norm.clone(),
                "execution_backend": "acpx",
                "acpx": acpx_spec.metadata.clone(),
                "timestamp": Utc::now().to_rfc3339(),
            }),
        );
        execution_backend_metadata = Some(acpx_spec.metadata.clone());
        DispatchExecution::Subprocess(build_acpx_command(&acpx_spec))
    } else if native_acp_enabled {
        let native_spec = match build_native_acp_run_spec(&params, &agent_norm, &prompt) {
            Ok(spec) => spec,
            Err(err) => {
                record_backend_prepare_failure("acp_native", &err);
                return Err(err);
            }
        };
        append_trajectory_event(
            &trajectory_path,
            json!({
                "event": "execution_backend_prepared",
                "dispatch_id": dispatch_id,
                "agent": agent_norm.clone(),
                "execution_backend": "acp_native",
                "acp_native": native_spec.metadata.clone(),
                "timestamp": Utc::now().to_rfc3339(),
            }),
        );
        execution_backend_metadata = Some(native_spec.metadata.clone());
        DispatchExecution::NativeAcp(native_spec)
    } else {
        let cmd = match agent_norm.as_str() {
            "claude" => build_claude_command(&params, &prompt, mcp_config_path.as_ref())?,
            "codex" => build_codex_command(&params, &prompt, mcp_config_path.as_ref())?,
            "grok" => build_grok_command(&params, &prompt, mcp_config_path.as_ref())?,
            "kimi" => build_kimi_command(&params, &prompt)?,
            "custom" => build_custom_command(&params, &prompt)?,
            other => {
                return Err(format!(
                    "Internal error: unhandled dispatch agent '{}'. {}",
                    other,
                    dispatch_agent_help_list()
                ));
            }
        };
        DispatchExecution::Subprocess(cmd)
    };
    let legacy_vault_env =
        unlocked_vault_child_env_map(server, params.cwd.as_deref().map(std::path::Path::new));
    let legacy_vault_env_count = legacy_vault_env.len();
    if legacy_vault_env_count > 0 {
        match &mut execution {
            DispatchExecution::Subprocess(cmd) => {
                for (name, value) in &legacy_vault_env {
                    cmd.env(name, value);
                }
            }
            DispatchExecution::NativeAcp(spec) => {
                spec.env.extend(legacy_vault_env.clone());
            }
        }
    }
    if legacy_vault_env_count > 0 {
        append_trajectory_event(
            &trajectory_path,
            json!({
                "event": "legacy_vault_env_injected",
                "dispatch_id": dispatch_id,
                "agent": agent_norm.clone(),
                "count": legacy_vault_env_count,
                "timestamp": Utc::now().to_rfc3339(),
            }),
        );
    }
    let dispatch_credentials = match materialize_dispatch_credentials(
        server,
        &params,
        &agent_norm,
        resolved_profile.selected_profile.as_deref(),
        &workspace_dir,
    ) {
        Ok(materialized) => materialized,
        Err(err) => {
            append_trajectory_event(
                &trajectory_path,
                json!({
                    "event": "credentials_materialization_failed",
                    "dispatch_id": dispatch_id,
                    "agent": agent_norm.clone(),
                    "credential_profiles": params.credential_profiles.clone(),
                    "error": err.clone(),
                    "timestamp": Utc::now().to_rfc3339(),
                }),
            );
            write_status_json(
                &workspace_dir,
                &dispatch_id,
                v2,
                plan_generated_at.as_deref(),
                None,
                if v2 { "approved" } else { "n/a" },
                Some(1),
                plan_duration_ms,
                None,
                plan_duration_ms,
                Some(json!({
                    "agent": agent_norm.clone(),
                    "task": params.task.clone(),
                    "state": "TASK_STATE_FAILED",
                    "updated_at": Utc::now().to_rfc3339(),
                    "run_dir": workspace_dir.to_string_lossy(),
                    "result_written": false,
                    "harness_transport": harness_transport.clone(),
                    "harness_server_url": harness_server_url.clone(),
                    "execution_backend": execution_backend_name,
                    "acpx": if acpx_enabled { execution_backend_metadata.clone() } else { None },
                    "acp_native": if native_acp_enabled { execution_backend_metadata.clone() } else { None },
                    "capability_bundle": capability_bundle_card.clone(),
                    "timeout_secs": timeout_secs_for_status,
                    "error": err.clone(),
                })),
            );
            return Err(err);
        }
    };
    for (name, value) in &dispatch_credentials.env {
        match &mut execution {
            DispatchExecution::Subprocess(cmd) => {
                cmd.env(name, value);
            }
            DispatchExecution::NativeAcp(spec) => {
                spec.env.insert(name.clone(), value.clone());
            }
        }
    }
    if !dispatch_credentials.reports.is_empty() {
        append_trajectory_event(
            &trajectory_path,
            json!({
                "event": "credentials_materialized",
                "dispatch_id": dispatch_id,
                "agent": agent_norm.clone(),
                "credential_profiles": params.credential_profiles.clone(),
                "reports": dispatch_credentials
                    .reports
                    .iter()
                    .map(credential_materialize_report_json)
                    .collect::<Vec<_>>(),
                "timestamp": Utc::now().to_rfc3339(),
            }),
        );
    }
    let credential_reports_json = dispatch_credentials
        .reports
        .iter()
        .map(credential_materialize_report_json)
        .collect::<Vec<_>>();
    let flow_dispatch_slot =
        reserve_dispatch_slot(params.flow_id.as_deref(), &params.task, &dispatch_id)?;

    // 6. Spawn background task with Watchdog
    let server_clone = server.clone();
    let d_id = dispatch_id.clone();
    let agent_for_watchdog = agent_norm.clone();
    let stage_for_traj = params.stage.clone();
    let traj_path_for_spawn = trajectory_path.clone();
    let workspace_dir_for_response = workspace_dir.clone();
    let workspace_dir_for_spawn = workspace_dir.clone();
    let v2_for_spawn = v2;
    let plan_generated_at_for_spawn = plan_generated_at.clone();
    let plan_duration_ms_for_spawn = plan_duration_ms;
    let timeout_secs_for_spawn = timeout_secs_for_status;
    let capability_bundle_card_for_spawn = capability_bundle_card.clone();
    let feedback_rules_trace_for_spawn = feedback_rules_trace.clone();
    let harness_transport_for_spawn = harness_transport.clone();
    let harness_server_url_for_spawn = harness_server_url.clone();
    let execution_backend_metadata_for_spawn = execution_backend_metadata.clone();
    let execution_for_spawn = execution;
    let flow_dispatch_slot_for_spawn = flow_dispatch_slot.clone();

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
                "timestamp": execute_started_at.to_rfc3339(),
            }),
        );

        let result = match execution_for_spawn {
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
        let mut kanban_state = None;
        for _ in 0..10 {
            tokio::time::sleep(Duration::from_millis(300)).await;
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
                // close the loop, but we have no real evaluation. Do NOT
                // synthesize a `success` eval — that would poison the nightly
                // routing analysis with records whose agent is "watchdog/*"
                // and whose quality/trajectory/diff are empty. Instead just
                // close the kanban row as COMPLETED but leave `reviewed=false`
                // so the status dashboard surfaces it as "unreviewed" and
                // operators can decide whether to write a real eval.
                let tail = tail_chars(&full_output, 500);
                eprintln!(
                    "[watchdog] dispatch {} exited 0 without tachi_complete; marking kanban COMPLETED as unreviewed. tail={}",
                    d_id, tail
                );
                if let Err(error) = update_kanban_state(
                    &server_clone,
                    &d_id,
                    "TASK_STATE_COMPLETED",
                    None,
                    Some(false),
                )
                .await
                {
                    eprintln!(
                        "[watchdog] failed to mark dispatch {} COMPLETED in kanban: {}",
                        d_id, error
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
                    retention_policy: Some("durable".to_string()),
                    domain: Some("system".to_string()),
                    timestamp: None,
                    valid_from: None,
                    valid_until: None,
                    metadata: Some(metadata),
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
                "harness_transport": harness_transport_for_spawn.clone(),
                "harness_server_url": harness_server_url_for_spawn.clone(),
                "execution_backend": if is_acpx_transport(&harness_transport_for_spawn) {
                    Some("acpx")
                } else if is_native_acp_transport(&harness_transport_for_spawn) {
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

    // 7. Immediately return — main agent is unblocked!
    let response = json!({
        "dispatch_id": dispatch_id,
        "task": {
            "id": dispatch_id,
            "status": { "state": "TASK_STATE_WORKING" },
        },
        "agent": agent_norm,
        "profile": profile_payload,
        "selected_profile": resolved_profile.selected_profile,
        "tool_access": resolved_profile.mcp_access,
        "dispatch_profile": resolved_profile.mbit_card,
        "credentials": credential_reports_json,
        "route_explanation": resolved_profile.route_explanation,
        "fallback_chain": resolved_profile.fallback_chain,
        "issue_ref": params.issue_ref,
        "pr_ref": params.pr_ref,
        "flow_id": params.flow_id,
        "auto_capability_bundle": resolved_profile.auto_capability_bundle,
        "capability_bundle": capability_bundle_card,
        "capability_bundle_file": capability_bundle_file,
        "feedback_rules": feedback_rules_trace,
        "harness_transport": harness_transport,
        "harness_server_url": harness_server_url,
        "execution_backend": execution_backend_name,
        "acpx": if acpx_enabled { execution_backend_metadata.clone() } else { None },
        "acp_native": if native_acp_enabled { execution_backend_metadata.clone() } else { None },
        "v2": v2,
        "plan_review_status": if v2 { "approved" } else { "n/a" },
        "duration_ms_plan": plan_duration_ms,
        "message": "Task dispatched to background. You are unblocked. Use tachi_task(action='board') to check status.",
        "suggested_complete_command": suggested_complete_payload(&dispatch_id, &agent_norm, &params),
        "plan_file": plan_path.to_string_lossy(),
        "prompt_file": prompt_md_path.to_string_lossy(),
        "context_file": context_md_path.to_string_lossy(),
        "trajectory_file": trajectory_path.to_string_lossy(),
        "run_dir": workspace_dir_for_response.to_string_lossy(),
    });

    serde_json::to_string(&response).map_err(|e| format!("serialize: {e}"))
}
