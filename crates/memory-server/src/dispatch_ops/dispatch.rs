use super::*;

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
use crate::dispatch_profile::resolve_and_apply_dispatch_profile_for_server;
use crate::vault_ops::read_unlocked_vault_secret;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

// ─── Dispatch result ─────────────────────────────────────────────────────────

pub(crate) struct DispatchResult {
    pub output: String,
    pub exit_code: Option<i32>,
}

pub(crate) fn apply_unlocked_vault_env(
    cmd: &mut Command,
    server: &MemoryServer,
    cwd: Option<&std::path::Path>,
) -> usize {
    let Ok(secrets) = server.unlocked_env_secrets_for_child_env(cwd) else {
        return 0;
    };

    let fill_missing_only = std::env::var("TACHI_VAULT_CHILD_ENV")
        .ok()
        .map(|value| {
            matches!(
                value.trim().to_ascii_lowercase().as_str(),
                "fill_missing" | "missing_only" | "preserve_env"
            )
        })
        .unwrap_or(false);
    let mut injected = 0usize;
    for (name, value) in secrets {
        if fill_missing_only && std::env::var_os(&name).is_some() {
            continue;
        }
        cmd.env(name, value);
        injected += 1;
    }
    injected
}

pub(crate) fn new_dispatch_id(now: chrono::DateTime<Utc>, agent: &str) -> String {
    let timestamp = now.format("%Y%m%dT%H%M%SZ").to_string();
    let sanitized = agent.replace(|c: char| !c.is_ascii_alphanumeric(), "-");
    let suffix = uuid::Uuid::new_v4().as_simple().to_string()[..8].to_string();
    format!("{}-{}-{}", timestamp, sanitized, suffix)
}

fn dispatch_runs_root() -> PathBuf {
    if let Ok(home) = std::env::var("TACHI_HOME") {
        PathBuf::from(home).join("runs")
    } else if let Ok(home) = std::env::var("HOME") {
        PathBuf::from(home).join(".tachi").join("runs")
    } else {
        std::env::temp_dir().join("tachi").join("runs")
    }
}

fn dispatch_status_is_terminal(dispatch_id: &str) -> bool {
    let status_path = dispatch_runs_root().join(dispatch_id).join("status.json");
    let Ok(Some(status)) = crate::task_lifecycle::read_json_file(&status_path) else {
        return false;
    };
    let state = status
        .get("state")
        .and_then(serde_json::Value::as_str)
        .unwrap_or_default();
    matches!(
        state,
        "TASK_STATE_COMPLETED" | "TASK_STATE_FAILED" | "TASK_STATE_CANCELED"
    ) || status.get("exit_code").is_some()
}

fn reserve_flow_dispatch_slot(
    flow_id: Option<&str>,
    task: &str,
    dispatch_id: &str,
) -> Result<Option<PathBuf>, String> {
    let Some(flow_id) = flow_id.filter(|id| !id.trim().is_empty()) else {
        return Ok(None);
    };
    let Ok(run_dir) = crate::shell_ops::run_dir_for_flow_id(flow_id) else {
        return Ok(None);
    };
    let lock_dir = run_dir.join(".dispatch-dedupe");
    std::fs::create_dir_all(&lock_dir).map_err(|e| format!("create dispatch dedupe dir: {e}"))?;
    let task_hash = crate::utils::stable_hash(task);
    let lock_path = lock_dir.join(format!("{task_hash}.json"));
    let payload = json!({
        "flow_id": flow_id,
        "task_hash": task_hash,
        "dispatch_id": dispatch_id,
        "task": task,
        "created_at": Utc::now().to_rfc3339(),
    });
    let payload =
        serde_json::to_vec_pretty(&payload).map_err(|e| format!("serialize dedupe lock: {e}"))?;

    match std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&lock_path)
    {
        Ok(mut file) => {
            use std::io::Write;
            file.write_all(&payload)
                .map_err(|e| format!("write dispatch dedupe lock: {e}"))?;
            Ok(Some(lock_path))
        }
        Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => {
            let existing = crate::task_lifecycle::read_json_file(&lock_path)?.unwrap_or(json!({}));
            let existing_dispatch_id = existing
                .get("dispatch_id")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("<unknown>");
            if dispatch_status_is_terminal(existing_dispatch_id) {
                let _ = std::fs::remove_file(&lock_path);
                return reserve_flow_dispatch_slot(Some(flow_id), task, dispatch_id);
            }
            Err(format!(
                "duplicate dispatch blocked for flow_id '{flow_id}' and same task; active dispatch_id: {existing_dispatch_id}"
            ))
        }
        Err(err) => Err(format!("create dispatch dedupe lock: {err}")),
    }
}

fn release_flow_dispatch_slot(lock_path: Option<PathBuf>) {
    if let Some(path) = lock_path {
        let _ = std::fs::remove_file(path);
    }
}

fn credential_search_dirs(cwd: Option<&Path>) -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    let mut seen = HashSet::new();

    if let Some(cwd) = cwd {
        for ancestor in cwd.ancestors() {
            let dir = ancestor.join(default_credentials_dir());
            let key = dir.to_string_lossy().to_string();
            if seen.insert(key) {
                dirs.push(dir);
            }
        }
    }

    let default_dir = default_credentials_dir();
    let key = default_dir.to_string_lossy().to_string();
    if seen.insert(key) {
        dirs.push(default_dir);
    }

    dirs
}

fn find_dispatch_credential_profile(
    profile_name: &str,
    cwd: Option<&Path>,
) -> Result<(PathBuf, crate::credential_profile::CredentialProfile), String> {
    let mut searched = Vec::new();
    let mut skipped = Vec::new();
    for dir in credential_search_dirs(cwd) {
        searched.push(dir.display().to_string());
        if !dir.exists() {
            continue;
        }
        match find_credential_profile(&dir, profile_name) {
            Ok(found) => return Ok(found),
            Err(err) => skipped.push(err),
        }
    }

    let mut msg = format!(
        "Credential profile '{}' not found. Searched: {}",
        profile_name,
        searched.join(", ")
    );
    if !skipped.is_empty() {
        msg.push_str(&format!("; skipped: {}", skipped.join(" | ")));
    }
    Err(msg)
}

fn dispatch_credential_consumer(
    agent_norm: &str,
    selected_profile: Option<&str>,
    profile: &crate::credential_profile::CredentialProfile,
) -> String {
    let allowed = &profile.allowed_consumers;
    if allowed.agents.is_empty() && allowed.profiles.is_empty() {
        return agent_norm.to_string();
    }
    if allowed.agents.iter().any(|agent| agent == agent_norm) {
        return agent_norm.to_string();
    }
    if let Some(selected_profile) = selected_profile {
        if allowed
            .profiles
            .iter()
            .any(|profile| profile == selected_profile)
        {
            return selected_profile.to_string();
        }
    }
    selected_profile.unwrap_or(agent_norm).to_string()
}

struct DispatchCredentialMaterialization {
    reports: Vec<CredentialMaterializeReport>,
    env: HashMap<String, String>,
}

fn credential_report_ready(report: &CredentialMaterializeReport) -> bool {
    report.allowed
        && report.missing_secrets.is_empty()
        && report.denied_secrets.is_empty()
        && report.steps.iter().all(|step| step.status == "ready")
}

fn materialize_dispatch_credentials(
    server: &MemoryServer,
    params: &TachiDispatchParams,
    agent_norm: &str,
    selected_profile: Option<&str>,
    run_dir: &Path,
) -> Result<DispatchCredentialMaterialization, String> {
    if params.credential_profiles.is_empty() {
        return Ok(DispatchCredentialMaterialization {
            reports: Vec::new(),
            env: HashMap::new(),
        });
    }

    let cwd = params.cwd.as_deref().map(Path::new);
    let mut profile_names = params
        .credential_profiles
        .iter()
        .map(|profile| profile.trim().to_string())
        .filter(|profile| !profile.is_empty())
        .collect::<Vec<_>>();
    crate::skill_policy::dedupe_preserve_order(&mut profile_names);

    let mut reports = Vec::new();
    let mut env = HashMap::new();
    for profile_name in profile_names {
        let (_, profile) = find_dispatch_credential_profile(&profile_name, cwd)?;
        let consumer = dispatch_credential_consumer(agent_norm, selected_profile, &profile);
        let plan = server.with_global_store(|store| {
            plan_credential_materialization_with_run_dir(
                &profile_name,
                &profile,
                &consumer,
                store,
                Some(run_dir),
            )
        })?;
        if !credential_report_ready(&plan) {
            let plan_json = serde_json::to_string(&credential_materialize_report_json(&plan))
                .map_err(|e| format!("serialize credential plan: {e}"))?;
            return Err(format!(
                "Credential profile '{}' is not ready for consumer '{}': {}",
                profile_name, consumer, plan_json
            ));
        }

        let secret_names = profile_secret_names(&profile);
        let mut secret_values = HashMap::new();
        for secret_name in secret_names {
            let value = read_unlocked_vault_secret(server, &secret_name, Some(&consumer), false)
                .map_err(|err| {
                    format!(
                        "Credential profile '{}' requires unlocked Vault secret '{}' for consumer '{}': {}",
                        profile_name, secret_name, consumer, err
                    )
                })?;
            secret_values.insert(secret_name, value);
        }

        let result = server.with_global_store(|store| {
            apply_credential_materialization(
                &profile_name,
                &profile,
                &consumer,
                store,
                &secret_values,
                &CredentialApplyOptions {
                    allow_existing: false,
                    run_dir: Some(run_dir.to_path_buf()),
                },
            )
        })?;
        env.extend(result.env);
        reports.push(result.report);
    }

    Ok(DispatchCredentialMaterialization { reports, env })
}

fn suggested_complete_payload(
    dispatch_id: &str,
    agent: &str,
    params: &TachiDispatchParams,
) -> serde_json::Value {
    json!({
        "tool": "tachi_task",
        "arguments": {
            "action": "complete",
            "dispatch_id": dispatch_id,
            "task": params.task,
            "agent": agent,
            "outcome": "success|failure|partial|aborted",
            "profile": params.profile,
            "flow_id": params.flow_id,
            "issue_ref": params.issue_ref,
            "pr_ref": params.pr_ref,
            "evidence_refs": [],
            "tests_run": [],
            "diff_present": null,
        }
    })
}

fn capability_bundle_summary(
    trace: &serde_json::Value,
    artifact_file: Option<&str>,
) -> serde_json::Value {
    json!({
        "status": trace.get("status").cloned().unwrap_or(serde_json::Value::Null),
        "requested": trace.get("requested").and_then(|value| value.as_bool()).unwrap_or(false),
        "disabled": trace.get("disabled").and_then(|value| value.as_bool()).unwrap_or(false),
        "injected": trace.get("injected").and_then(|value| value.as_bool()).unwrap_or(false),
        "host": trace.get("host").cloned().unwrap_or(serde_json::Value::Null),
        "query": trace.get("query").cloned().unwrap_or(serde_json::Value::Null),
        "source": trace.get("source").cloned().unwrap_or(serde_json::Value::Null),
        "primary_skill": trace.get("primary_skill").cloned().unwrap_or(serde_json::Value::Null),
        "supporting_capabilities_count": trace
            .get("supporting_capabilities")
            .and_then(|value| value.as_array())
            .map(|items| items.len())
            .unwrap_or(0),
        "packs_count": trace
            .get("packs")
            .and_then(|value| value.as_array())
            .map(|items| items.len())
            .unwrap_or(0),
        "host_tools_count": trace
            .get("host_tools")
            .and_then(|value| value.as_array())
            .map(|items| items.len())
            .unwrap_or(0),
        "reason": trace.get("reason").cloned().unwrap_or(serde_json::Value::Null),
        "error": trace.get("error").cloned().unwrap_or(serde_json::Value::Null),
        "artifact_file": artifact_file,
    })
}

/// Scope guard that deletes a temporary MCP config file when dropped.
/// Logs a warning if cleanup fails so leaking temp files is observable.
struct McpCleanup(Option<PathBuf>);

impl Drop for McpCleanup {
    fn drop(&mut self) {
        let Some(path) = self.0.take() else {
            return;
        };
        if !path.exists() {
            return;
        }
        if let Err(e) = std::fs::remove_file(&path) {
            tracing::warn!(
                "failed to remove temporary MCP config file {}: {}",
                path.display(),
                e
            );
            return;
        }
        if path.exists() {
            tracing::warn!(
                "temporary MCP config file {} still exists after removal",
                path.display()
            );
        }
    }
}

// ─── Main dispatch handler ───────────────────────────────────────────────────

pub(crate) async fn handle_tachi_dispatch(
    server: &MemoryServer,
    mut params: TachiDispatchParams,
) -> Result<String, String> {
    let now = Utc::now();
    let resolved_profile = resolve_and_apply_dispatch_profile_for_server(server, &mut params)?;
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

    // 1. Create isolated workspace directory
    let workspace_dir = {
        let base = if let Ok(home) = std::env::var("TACHI_HOME") {
            PathBuf::from(home)
        } else if let Ok(home) = std::env::var("HOME") {
            PathBuf::from(home).join(".tachi")
        } else {
            std::env::temp_dir().join("tachi")
        };
        base.join("runs").join(&dispatch_id)
    };
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
    tokio::fs::write(&plan_path, &base_prompt)
        .await
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
        std::fs::write(&plan_path, &plan_outcome.plan_md)
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

    // 5. Build command
    let mut cmd = match agent_norm.as_str() {
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
    let legacy_vault_env_count = apply_unlocked_vault_env(
        &mut cmd,
        server,
        params.cwd.as_deref().map(std::path::Path::new),
    );
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
                    "capability_bundle": capability_bundle_card.clone(),
                    "timeout_secs": timeout_secs_for_status,
                    "error": err.clone(),
                })),
            );
            return Err(err);
        }
    };
    for (name, value) in &dispatch_credentials.env {
        cmd.env(name, value);
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
        reserve_flow_dispatch_slot(params.flow_id.as_deref(), &params.task, &dispatch_id)?;

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

        let result = run_agent_subprocess(cmd, timeout).await;
        let execute_duration_ms = execute_started_instant.elapsed().as_millis() as u64;

        // Append subprocess_finished event to trajectory.jsonl
        let full_output = match &result {
            Ok(r) => r.output.clone(),
            Err(e) => e.clone(),
        };
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
                let progress_path = workspace_dir.join("progress.jsonl");
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
            let _ = std::fs::write(&result_path, &full_output);
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
                let _ = update_kanban_state(
                    &server_clone,
                    &d_id,
                    "TASK_STATE_COMPLETED",
                    None,
                    Some(false),
                )
                .await;
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
                let _ =
                    crate::memory_search_ops::handle_save_memory(&server_clone, save_params).await;

                let _ = update_kanban_state(
                    &server_clone,
                    &d_id,
                    "TASK_STATE_FAILED",
                    Some(&eval_id),
                    Some(false),
                )
                .await;
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

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    #[allow(clippy::await_holding_lock)]
    async fn generate_mcp_config_sets_owner_only_permissions() {
        let _guard = crate::utils::global_test_lock()
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let temp_home = tempfile::tempdir().expect("temp home");
        let original_home = std::env::var_os("HOME");
        let original_tachi_home = std::env::var_os("TACHI_HOME");
        std::env::set_var("HOME", temp_home.path());
        std::env::remove_var("TACHI_HOME");

        let server = crate::tests::make_server();
        let path = generate_mcp_config(&server, "test-perms", true, false, None, &[])
            .await
            .expect("generate mcp config")
            .expect("config path");

        assert!(path.exists());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o600, "MCP config mode should be 0o600, got {:#o}", mode);
        }

        if let Some(value) = original_home {
            std::env::set_var("HOME", value);
        } else {
            std::env::remove_var("HOME");
        }
        if let Some(value) = original_tachi_home {
            std::env::set_var("TACHI_HOME", value);
        } else {
            std::env::remove_var("TACHI_HOME");
        }
    }

    #[test]
    fn mcp_cleanup_removes_temp_config_on_drop() {
        let temp_home = tempfile::tempdir().expect("temp home");
        let path = temp_home.path().join("dispatch-test-mcp.json");
        std::fs::write(&path, b"{}").expect("write temp config");
        assert!(path.exists());
        {
            let _cleanup = McpCleanup(Some(path.clone()));
        }
        assert!(!path.exists(), "MCP config should be removed on drop");
    }

    #[test]
    fn flow_dispatch_slot_blocks_duplicate_active_task() {
        let _guard = crate::utils::global_test_lock()
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let temp_home = tempfile::tempdir().expect("temp home");
        let original_home = std::env::var_os("HOME");
        let original_tachi_home = std::env::var_os("TACHI_HOME");
        std::env::set_var("HOME", temp_home.path());
        std::env::remove_var("TACHI_HOME");

        let flow_id = format!(
            "flow_20260610T000000Z_duplicate_slot_{}",
            uuid::Uuid::new_v4().as_simple()
        );
        let run_dir = crate::shell_ops::run_dir_for_flow_id(&flow_id).expect("flow run dir");
        std::fs::create_dir_all(&run_dir).expect("create flow dir");

        let first = reserve_flow_dispatch_slot(Some(&flow_id), "same task", "dispatch-one")
            .expect("first reserve")
            .expect("slot path");
        let duplicate = reserve_flow_dispatch_slot(Some(&flow_id), "same task", "dispatch-two")
            .expect_err("duplicate active task should be blocked");
        assert!(
            duplicate.contains("duplicate dispatch blocked"),
            "unexpected error: {duplicate}"
        );

        release_flow_dispatch_slot(Some(first));
        assert!(
            reserve_flow_dispatch_slot(Some(&flow_id), "same task", "dispatch-three")
                .expect("reserve after release")
                .is_some(),
            "slot should be reusable after release"
        );

        if let Some(value) = original_home {
            std::env::set_var("HOME", value);
        } else {
            std::env::remove_var("HOME");
        }
        if let Some(value) = original_tachi_home {
            std::env::set_var("TACHI_HOME", value);
        } else {
            std::env::remove_var("TACHI_HOME");
        }
    }
}
