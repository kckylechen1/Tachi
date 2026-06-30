use super::acp_native::{build_native_acp_run_spec, is_native_acp_transport};
use super::acpx::{
    build_acpx_command, build_acpx_command_spec, is_acpx_transport, prepare_acpx_prompt,
};
use super::dispatch_v2::{
    append_trajectory_event, build_execute_prompt, parse_plan_sections, plan_review_required,
    plan_timeout_secs, run_plan_stage, v2_enabled_from_env, write_status_json, V2Decision,
};
use super::kanban_helpers::init_kanban_task;
use super::mcp_config::generate_mcp_config;
use super::prompt::{assemble_prompt_with_trace, resolve_effective_skills};
use super::subprocess::{
    build_claude_command, build_codex_command, build_custom_command, build_grok_command,
    build_kimi_command,
};
use crate::agent_registry::{
    dispatch_agent_help_list, mcp_inject_supported, resolve_dispatch_agent,
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

pub(crate) struct DispatchResult {
    pub output: String,
    pub exit_code: Option<i32>,
}

mod artifacts;
mod backend;
mod backend_failure;
mod credentials;
mod dedupe;
mod execution;
mod recovery;
mod response_helpers;
mod start;

#[cfg(test)]
mod tests;

use self::artifacts::{write_dispatch_artifacts, DispatchArtifactInputs, DispatchArtifacts};
use self::backend::{prepare_dispatch_backend, DispatchBackendContext, PreparedDispatchBackend};
use self::backend_failure::*;
use self::credentials::*;
use self::dedupe::*;
use self::execution::{spawn_background_dispatch, BackgroundDispatchContext, DispatchExecution};
use self::response_helpers::*;
use self::start::*;

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
        host_adapter,
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
            "host_adapter": host_adapter.clone(),
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
                    "host_adapter": host_adapter.clone(),
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
        execution_backend_metadata: execution_backend_metadata.clone(),
        execution,
        flow_dispatch_slot,
        mcp_config_path,
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
        "host_adapter": host_adapter,
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
