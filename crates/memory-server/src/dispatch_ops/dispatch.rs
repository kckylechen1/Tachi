use super::*;

use super::dispatch_v2::{
    append_trajectory_event, build_execute_prompt, parse_plan_sections, plan_review_required,
    plan_timeout_secs, run_plan_stage, v2_enabled_from_env, write_status_json, V2Decision,
};
use super::kanban_helpers::{
    get_kanban_state, init_kanban_task, should_cleanup_run, update_kanban_state,
};
use super::mcp_config::generate_mcp_config;
use super::prompt::{assemble_prompt, resolve_effective_skills};
use super::subprocess::{
    build_claude_command, build_codex_command, build_custom_command, run_agent_subprocess,
    tail_chars,
};

// ─── Dispatch result ─────────────────────────────────────────────────────────

#[allow(dead_code)]
pub(crate) struct DispatchResult {
    pub output: String,
    pub exit_code: Option<i32>,
    pub duration_ms: u64,
}

pub(crate) fn apply_unlocked_vault_env(cmd: &mut Command, server: &MemoryServer) -> usize {
    let Ok(secrets) = server.unlocked_env_secrets_for_child_env() else {
        return 0;
    };

    let mut injected = 0usize;
    for (name, value) in secrets {
        if std::env::var_os(&name).is_some() {
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

// ─── Main dispatch handler ───────────────────────────────────────────────────

pub(crate) async fn handle_tachi_dispatch(
    server: &MemoryServer,
    params: TachiDispatchParams,
) -> Result<String, String> {
    let now = Utc::now();
    let dispatch_id = new_dispatch_id(now, &params.agent);

    let agent_norm = params.agent.to_ascii_lowercase();
    let timeout = Duration::from_secs(params.timeout_secs);

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

    // 2. Generate MCP config if requested
    let inject_tachi = params.inject_tachi_mcp.unwrap_or(false);
    let inject_hub = params.inject_hub_mcps.unwrap_or(false);
    // Codex's `codex exec` CLI does not consume an external mcp-config file
    // the way Claude Code's `--mcp-config` does (see build_codex_command,
    // where the generated config path is deliberately ignored). Failing
    // loudly here is clearer than silently producing a config file the
    // subprocess will never read.
    if matches!(agent_norm.as_str(), "codex" | "codex-cli" | "openai")
        && (inject_tachi || inject_hub)
    {
        return Err(
            "inject_tachi_mcp / inject_hub_mcps are not supported for the codex backend. \
             Configure MCP servers in ~/.codex/config.toml instead, or dispatch with agent='claude'."
                .to_string(),
        );
    }
    let mcp_config_path = if inject_tachi || inject_hub {
        generate_mcp_config(server, &dispatch_id, inject_tachi, inject_hub).await?
    } else {
        None
    };

    // 3. Assemble prompt & write audit files to workspace
    let base_prompt = assemble_prompt(server, &params).await;
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
    std::fs::write(&plan_path, &base_prompt)
        .map_err(|e| format!("Failed to write plan file: {e}"))?;

    // Write prompt.md (full assembled prompt for tracked run)
    let prompt_md_path = workspace_dir.join("prompt.md");
    std::fs::write(&prompt_md_path, &base_prompt)
        .map_err(|e| format!("Failed to write prompt.md: {e}"))?;

    // Write context.md (summary of injected context/skills — for MVP, same as prompt)
    let context_md_path = workspace_dir.join("context.md");
    let context_summary = {
        let mut sections = Vec::new();
        sections.push(format!("# Dispatch Context: {}", dispatch_id));
        sections.push(format!("Agent: {}", params.agent));
        sections.push(format!(
            "Stage: {}",
            params.stage.as_deref().unwrap_or("none")
        ));
        sections.push(format!("V2: {}", v2));
        sections.push(format!("Skills: {:?}", effective_skills_for_files));
        sections.push(String::new());
        sections.push(base_prompt.clone());
        sections.join("\n\n")
    };
    std::fs::write(&context_md_path, &context_summary)
        .map_err(|e| format!("Failed to write context.md: {e}"))?;

    // Write trajectory.jsonl — initial dispatch_started event
    let trajectory_path = workspace_dir.join("trajectory.jsonl");
    {
        let started_event = json!({
            "event": "dispatch_started",
            "dispatch_id": dispatch_id,
            "agent": params.agent,
            "stage": params.stage,
            "v2": v2,
            "timestamp": Utc::now().to_rfc3339(),
        });
        let line = serde_json::to_string(&started_event)
            .map_err(|e| format!("Failed to serialize started event: {e}"))?;
        std::fs::write(&trajectory_path, format!("{}\n", line))
            .map_err(|e| format!("Failed to write trajectory.jsonl: {e}"))?;
    }

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
        None,
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
                    Some(json!({ "error": e.clone() })),
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
                    Some(json!({ "error": e.clone() })),
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
                None,
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
                "v2": true,
                "plan_review_status": "pending_review",
                "message": "Plan generated. DISPATCH_V2_PLAN_REVIEW=true — execute stage paused. Audit plan.md and re-dispatch with the env var unset to proceed.",
                "plan_file": plan_path.to_string_lossy(),
                "prompt_file": prompt_md_path.to_string_lossy(),
                "context_file": context_md_path.to_string_lossy(),
                "trajectory_file": trajectory_path.to_string_lossy(),
                "run_dir": workspace_dir.to_string_lossy(),
            });
            return serde_json::to_string(&response).map_err(|e| format!("serialize: {e}"));
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

    // 5. Build command
    let mut cmd = match agent_norm.as_str() {
        "claude" | "claude-code" | "claude-cli" => {
            build_claude_command(&params, &prompt, mcp_config_path.as_ref())
        }
        "codex" | "codex-cli" | "openai" => {
            build_codex_command(&params, &prompt, mcp_config_path.as_ref())
        }
        "custom" => build_custom_command(&params, &prompt)?,
        other => {
            return Err(format!(
                "Unknown agent '{}'. Use 'claude', 'codex', or 'custom'.",
                other
            ));
        }
    };
    let _ = apply_unlocked_vault_env(&mut cmd, server);

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

    // Scope guard for MCP config cleanup (moved into spawned task)
    struct McpCleanup(Option<PathBuf>);
    impl Drop for McpCleanup {
        fn drop(&mut self) {
            if let Some(ref path) = self.0 {
                let _ = std::fs::remove_file(path);
            }
        }
    }

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
            }
        }

        // Save full output to result.md for orchestrator eval
        {
            let result_path = workspace_dir.join("result.md");
            let _ = std::fs::write(&result_path, &full_output);
        }

        // --- WATCHDOG: check if sub-agent properly closed the loop ---
        tokio::time::sleep(Duration::from_secs(2)).await; // grace period for tachi_complete to propagate

        let kanban_state = get_kanban_state(&server_clone, &d_id).await;
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

        if !should_cleanup {
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
                None,
            );
        }

        if should_cleanup {
            let _ = std::fs::remove_dir_all(workspace_dir);
        }
    });

    // 7. Immediately return — main agent is unblocked!
    let response = json!({
        "dispatch_id": dispatch_id,
        "task": {
            "id": dispatch_id,
            "status": { "state": "TASK_STATE_WORKING" },
        },
        "agent": agent_norm,
        "v2": v2,
        "plan_review_status": if v2 { "approved" } else { "n/a" },
        "duration_ms_plan": plan_duration_ms,
        "message": "Task dispatched to background. You are unblocked. Use tachi_board to check status.",
        "plan_file": plan_path.to_string_lossy(),
        "prompt_file": prompt_md_path.to_string_lossy(),
        "context_file": context_md_path.to_string_lossy(),
        "trajectory_file": trajectory_path.to_string_lossy(),
        "run_dir": workspace_dir_for_response.to_string_lossy(),
    });

    serde_json::to_string(&response).map_err(|e| format!("serialize: {e}"))
}
