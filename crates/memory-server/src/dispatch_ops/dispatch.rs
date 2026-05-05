use super::*;

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

// ─── Main dispatch handler ───────────────────────────────────────────────────

pub(crate) async fn handle_tachi_dispatch(
    server: &MemoryServer,
    params: TachiDispatchParams,
) -> Result<String, String> {
    let now = Utc::now();
    let dispatch_id = format!(
        "{}-{}",
        now.format("%Y%m%dT%H%M%SZ"),
        params
            .agent
            .replace(|c: char| !c.is_ascii_alphanumeric(), "-")
    );

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
    std::fs::create_dir_all(&workspace_dir)
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
    let prompt = assemble_prompt(server, &params).await;
    let (effective_skills_for_files, _) = resolve_effective_skills(&params);
    let plan_path = workspace_dir.join("plan.md");
    std::fs::write(&plan_path, &prompt).map_err(|e| format!("Failed to write plan file: {e}"))?;

    // Write prompt.md (full assembled prompt for tracked run)
    let prompt_md_path = workspace_dir.join("prompt.md");
    std::fs::write(&prompt_md_path, &prompt)
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
        sections.push(format!("Skills: {:?}", effective_skills_for_files));
        sections.push(String::new());
        sections.push(prompt.clone());
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
            "timestamp": Utc::now().to_rfc3339(),
        });
        let line = serde_json::to_string(&started_event)
            .map_err(|e| format!("Failed to serialize started event: {e}"))?;
        std::fs::write(&trajectory_path, format!("{}\n", line))
            .map_err(|e| format!("Failed to write trajectory.jsonl: {e}"))?;
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
    let cmd = match agent_norm.as_str() {
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

    // 6. Spawn background task with Watchdog
    let server_clone = server.clone();
    let d_id = dispatch_id.clone();
    let agent_for_watchdog = agent_norm.clone();
    let stage_for_traj = params.stage.clone();
    let traj_path_for_spawn = trajectory_path.clone();
    let workspace_dir_for_response = workspace_dir.clone();

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

        let result = run_agent_subprocess(cmd, timeout).await;

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
        "message": "Task dispatched to background. You are unblocked. Use tachi_board to check status.",
        "plan_file": plan_path.to_string_lossy(),
        "prompt_file": prompt_md_path.to_string_lossy(),
        "context_file": context_md_path.to_string_lossy(),
        "trajectory_file": trajectory_path.to_string_lossy(),
        "run_dir": workspace_dir_for_response.to_string_lossy(),
    });

    serde_json::to_string(&response).map_err(|e| format!("serialize: {e}"))
}
