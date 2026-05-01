use crate::TachiDispatchParams;
use crate::MemoryServer;
use crate::SearchMemoryParams;
use chrono::Utc;
use serde_json::json;
use std::time::Duration;
use tokio::process::Command;

// ─── Dispatch result ─────────────────────────────────────────────────────────

pub(crate) struct DispatchResult {
    pub output: String,
    pub exit_code: Option<i32>,
    pub duration_ms: u64,
}

// ─── Prompt assembly ─────────────────────────────────────────────────────────

pub(crate) async fn assemble_prompt(
    server: &MemoryServer,
    params: &TachiDispatchParams,
) -> String {
    let mut parts: Vec<String> = Vec::new();

    // 1. Context from memory/wiki
    if let Some(ref query) = params.context_query {
        if let Ok(rows) = crate::memory_search_ops::search_memory_rows(
            server,
            SearchMemoryParams {
                query: query.clone(),
                query_vec: None,
                top_k: 5,
                path_prefix: None,
                include_archived: false,
                candidates_per_channel: 20,
                mmr_threshold: Some(0.7),
                graph_expand_hops: 0,
                graph_relation_filter: None,
                weights: None,
                agent_role: None,
                project: params.project.clone(),
                domain: None,
            },
        )
        .await
        {
            if !rows.is_empty() {
                parts.push("## Relevant context from memory/wiki".to_string());
                for row in &rows {
                    if let Some(text) = row.get("text").and_then(|v| v.as_str()) {
                        let path = row
                            .get("path")
                            .and_then(|v| v.as_str())
                            .unwrap_or("unknown");
                        parts.push(format!("### {}\n{}", path, text));
                    }
                }
                parts.push(String::new());
            }
        }
    }

    // 2. Skill definitions
    for skill_id in &params.skills {
        if let Ok(cap) = server.get_capability(skill_id).map_err(|e| format!("{e}")) {
            let def: serde_json::Value =
                serde_json::from_str(&cap.definition).unwrap_or_default();
            if let Some(prompt) = def.get("prompt").and_then(|v| v.as_str()) {
                parts.push(format!(
                    "## Skill: {}\n{}",
                    skill_id, prompt
                ));
            }
        }
    }

    // 3. Task itself
    parts.push(format!("## Task\n{}", params.task));

    parts.join("\n\n")
}

// ─── Agent subprocess builders ───────────────────────────────────────────────

fn build_claude_command(params: &TachiDispatchParams, prompt: &str) -> Command {
    let mut cmd = Command::new("claude");
    cmd.arg("-p"); // print mode
    cmd.arg("--output-format").arg("json");
    if let Some(ref model) = params.model {
        cmd.arg("--model").arg(model);
    }
    cmd.arg(prompt);
    if let Some(ref cwd) = params.cwd {
        cmd.current_dir(cwd);
    }
    cmd
}

fn build_codex_command(params: &TachiDispatchParams, prompt: &str) -> Command {
    let mut cmd = Command::new("codex");
    let policy = params
        .approval_policy
        .as_deref()
        .unwrap_or("never");
    cmd.arg("--approval-policy").arg(policy);
    let sandbox = params.sandbox.as_deref().unwrap_or("workspace-write");
    cmd.arg("--sandbox").arg(sandbox);
    cmd.arg("--quiet");
    if let Some(ref model) = params.model {
        cmd.arg("--model").arg(model);
    }
    cmd.arg(prompt);
    if let Some(ref cwd) = params.cwd {
        cmd.current_dir(cwd);
    }
    cmd
}

fn build_custom_command(params: &TachiDispatchParams, prompt: &str) -> Result<Command, String> {
    if params.command.is_empty() {
        return Err("agent='custom' requires a non-empty 'command' array".to_string());
    }
    let mut cmd = Command::new(&params.command[0]);
    for arg in &params.command[1..] {
        cmd.arg(arg);
    }
    cmd.arg(prompt);
    if let Some(ref cwd) = params.cwd {
        cmd.current_dir(cwd);
    }
    Ok(cmd)
}

// ─── Execute subprocess ──────────────────────────────────────────────────────

async fn run_agent_subprocess(
    mut cmd: Command,
    timeout: Duration,
) -> Result<DispatchResult, String> {
    cmd.stdout(std::process::Stdio::piped());
    cmd.stderr(std::process::Stdio::piped());

    let start = std::time::Instant::now();

    let child = cmd
        .spawn()
        .map_err(|e| format!("Failed to spawn agent process: {e}"))?;

    let output = tokio::time::timeout(timeout, child.wait_with_output())
        .await
        .map_err(|_| {
            format!(
                "Agent process timed out after {}s",
                timeout.as_secs()
            )
        })?
        .map_err(|e| format!("Agent process error: {e}"))?;

    let duration_ms = start.elapsed().as_millis() as u64;
    let exit_code = output.status.code();
    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();

    let output_text = if stdout.is_empty() && !stderr.is_empty() {
        stderr
    } else if !stderr.is_empty() {
        format!("{}\n\n--- stderr ---\n{}", stdout, stderr)
    } else {
        stdout
    };

    Ok(DispatchResult {
        output: output_text,
        exit_code,
        duration_ms,
    })
}

// ─── Parse Claude JSON output ────────────────────────────────────────────────

fn parse_claude_output(raw: &str) -> serde_json::Value {
    if let Ok(parsed) = serde_json::from_str::<serde_json::Value>(raw) {
        if let Some(result) = parsed.get("result") {
            return json!({
                "parsed": true,
                "result": result,
                "cost": parsed.get("cost_usd"),
                "duration_ms": parsed.get("duration_ms"),
                "num_turns": parsed.get("num_turns"),
            });
        }
        return json!({"parsed": true, "raw_json": parsed});
    }
    json!({"parsed": false, "text": raw})
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
        params.agent.replace(|c: char| !c.is_ascii_alphanumeric(), "-")
    );

    let agent_norm = params.agent.to_ascii_lowercase();
    let timeout_secs = params.timeout_secs.unwrap_or(300);
    let timeout = Duration::from_secs(timeout_secs);

    // 1. Assemble prompt
    let prompt = assemble_prompt(server, &params).await;

    // 2. Build command
    let cmd = match agent_norm.as_str() {
        "claude" | "claude-code" | "claude-cli" => build_claude_command(&params, &prompt),
        "codex" | "codex-cli" | "openai" => build_codex_command(&params, &prompt),
        "custom" => build_custom_command(&params, &prompt)?,
        other => {
            return Err(format!(
                "Unknown agent '{}'. Use 'claude', 'codex', or 'custom'.",
                other
            ));
        }
    };

    // 3. Execute
    let result = run_agent_subprocess(cmd, timeout).await?;

    // 4. Parse output
    let parsed_output = match agent_norm.as_str() {
        "claude" | "claude-code" | "claude-cli" => parse_claude_output(&result.output),
        _ => json!({"text": result.output}),
    };

    let success = result
        .exit_code
        .map(|c| c == 0)
        .unwrap_or(false);

    // 5. Build response
    let response = json!({
        "dispatch_id": dispatch_id,
        "agent": agent_norm,
        "task": params.task,
        "success": success,
        "exit_code": result.exit_code,
        "duration_ms": result.duration_ms,
        "output": parsed_output,
        "skills_injected": params.skills,
        "context_query": params.context_query,
        "cwd": params.cwd,
        "next_steps": [
            format!("Call tachi_complete with dispatch_id='{}' to record the eval.", dispatch_id),
            "Review output and decide if work is acceptable.",
        ],
    });

    serde_json::to_string(&response).map_err(|e| format!("serialize dispatch response: {e}"))
}
