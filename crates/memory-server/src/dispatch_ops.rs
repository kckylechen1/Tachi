use crate::TachiDispatchParams;
use crate::MemoryServer;
use crate::SearchMemoryParams;
use chrono::Utc;
use serde_json::json;
use std::path::PathBuf;
use std::time::Duration;
use tokio::process::Command;

// ─── Dispatch result ─────────────────────────────────────────────────────────

pub(crate) struct DispatchResult {
    pub output: String,
    pub exit_code: Option<i32>,
    pub duration_ms: u64,
}

// ─── MCP config generation ───────────────────────────────────────────────────

/// Generate a temporary MCP config JSON file for the dispatched agent subprocess.
/// Queries the Hub for registered MCP servers and writes a Claude Code / Codex
/// compatible mcpServers config.
///
/// When `inject_tachi` is true, adds a "tachi" entry pointing at the running
/// daemon's stdio transport. When `inject_hub` is true, walks all Hub-registered
/// MCP capabilities and adds them.
///
/// Returns the path to the temp file (caller should clean up after subprocess exits).
async fn generate_mcp_config(
    server: &MemoryServer,
    dispatch_id: &str,
    inject_tachi: bool,
    inject_hub: bool,
) -> Result<Option<PathBuf>, String> {
    let mut mcp_servers = serde_json::Map::new();

    if inject_tachi {
        // Point at the Tachi binary in stdio mode
        mcp_servers.insert(
            "tachi".to_string(),
            json!({
                "command": "tachi",
                "args": ["serve"]
            }),
        );
    }

    if inject_hub {
        // Walk Hub for MCP-type capabilities with a stdio transport definition
        let caps = server
            .with_global_store(|store| {
                store
                    .hub_list(Some("mcp"), true)
                    .map_err(|e| format!("hub list for mcp config: {e}"))
            })
            .unwrap_or_default();

        for cap in caps {
            if !cap.enabled {
                continue;
            }
            let def: serde_json::Value =
                serde_json::from_str(&cap.definition).unwrap_or_default();
            let transport = def.get("transport").and_then(|t| t.as_str()).unwrap_or("");
            if transport != "stdio" {
                continue;
            }
            let command = match def.get("command").and_then(|c| c.as_str()) {
                Some(c) => c,
                None => continue,
            };
            let args = def
                .get("args")
                .and_then(|a| a.as_array())
                .map(|arr| {
                    arr.iter()
                        .filter_map(|v| v.as_str().map(String::from))
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default();

            // Derive server key from capability id: "mcp:context7" → "context7"
            let key = cap
                .id
                .strip_prefix("mcp:")
                .unwrap_or(&cap.id)
                .to_string();

            let mut entry = json!({ "command": command });
            if !args.is_empty() {
                entry["args"] = json!(args);
            }
            mcp_servers.insert(key, entry);
        }
    }

    if mcp_servers.is_empty() {
        return Ok(None);
    }

    let config = json!({ "mcpServers": mcp_servers });

    // Write to temp file under $TACHI_HOME/tmp or $HOME/.tachi/tmp
    let tmp_dir = if let Ok(home) = std::env::var("TACHI_HOME") {
        PathBuf::from(home)
    } else if let Ok(home) = std::env::var("HOME") {
        PathBuf::from(home).join(".tachi")
    } else {
        std::env::temp_dir().join("tachi")
    };
    let tmp_dir = tmp_dir.join("tmp");
    std::fs::create_dir_all(&tmp_dir)
        .map_err(|e| format!("Failed to create tmp dir for MCP config: {e}"))?;

    let config_path = tmp_dir.join(format!("dispatch-{dispatch_id}-mcp.json"));
    let config_str = serde_json::to_string_pretty(&config)
        .map_err(|e| format!("Failed to serialize MCP config: {e}"))?;
    std::fs::write(&config_path, config_str)
        .map_err(|e| format!("Failed to write MCP config: {e}"))?;

    Ok(Some(config_path))
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
                parts.push(format!("## Skill: {}\n{}", skill_id, prompt));
            }
        }
    }

    // 3. Task itself
    parts.push(format!("## Task\n{}", params.task));

    parts.join("\n\n")
}

// ─── Agent subprocess builders ───────────────────────────────────────────────

/// Resolve the effective permission profile: explicit param → "full" as default
/// for dispatched agents (the whole point of dispatch is autonomous execution).
fn resolve_permission_profile(params: &TachiDispatchParams) -> &str {
    params
        .permission_profile
        .as_deref()
        .unwrap_or("full")
}

fn build_claude_command(
    params: &TachiDispatchParams,
    prompt: &str,
    mcp_config_path: Option<&PathBuf>,
) -> Command {
    let mut cmd = Command::new("claude");
    cmd.arg("-p"); // print mode
    cmd.arg("--output-format").arg("json");

    // Permission profile
    let profile = resolve_permission_profile(params);
    match profile {
        "full" => {
            cmd.arg("--dangerously-skip-permissions");
        }
        "allowlist" if !params.allowed_tools.is_empty() => {
            for tool in &params.allowed_tools {
                cmd.arg("--allowedTools").arg(tool);
            }
        }
        _ => {} // "default" — no permission flags
    }

    // Max turns
    if let Some(turns) = params.max_turns {
        cmd.arg("--max-turns").arg(turns.to_string());
    }

    // Model override
    if let Some(ref model) = params.model {
        cmd.arg("--model").arg(model);
    }

    // MCP config injection
    if let Some(path) = mcp_config_path {
        cmd.arg("--mcp-config").arg(path);
    }

    cmd.arg(prompt);

    if let Some(ref cwd) = params.cwd {
        cmd.current_dir(cwd);
    }
    cmd
}

fn build_codex_command(
    params: &TachiDispatchParams,
    prompt: &str,
    _mcp_config_path: Option<&PathBuf>,
) -> Command {
    let mut cmd = Command::new("codex");
    cmd.arg("exec"); // non-interactive subcommand

    // Permission profile
    let profile = resolve_permission_profile(params);
    if profile == "full" {
        cmd.arg("--dangerously-bypass-approvals-and-sandbox");
    } else {
        let sandbox = params.sandbox.as_deref().unwrap_or("workspace-write");
        cmd.arg("--sandbox").arg(sandbox);
    }

    cmd.arg("--json");

    if let Some(turns) = params.max_turns {
        // Codex doesn't have a direct --max-turns; use -c config override
        cmd.arg("-c")
            .arg(format!("max_turns={turns}"));
    }

    if let Some(ref model) = params.model {
        cmd.arg("-m").arg(model);
    }

    cmd.arg(prompt);

    if let Some(ref cwd) = params.cwd {
        cmd.arg("-C").arg(cwd);
    }
    cmd
}

fn build_custom_command(params: &TachiDispatchParams, prompt: &str) -> Result<Command, String> {
    if params.command.is_empty() {
        return Err("agent='custom' requires a non-empty 'command' array".to_string());
    }
    let binary = &params.command[0];
    if !crate::utils::is_trusted_command(binary) {
        return Err(format!(
            "Command '{}' is not in the trusted allowlist. Allowed: npx, node, bun, deno, python3, python, uv, cargo, rustup, docker, podman, tachi, or paths under /opt/homebrew/, /usr/local/bin/, ~/.cargo/bin/, ~/.local/bin/",
            binary
        ));
    }
    let mut cmd = Command::new(binary);
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
    let timeout = Duration::from_secs(params.timeout_secs);

    // 1. Generate MCP config if requested
    let inject_tachi = params.inject_tachi_mcp.unwrap_or(false);
    let inject_hub = params.inject_hub_mcps.unwrap_or(false);
    let mcp_config_path = if inject_tachi || inject_hub {
        generate_mcp_config(server, &dispatch_id, inject_tachi, inject_hub).await?
    } else {
        None
    };

    // 2. Assemble prompt
    let prompt = assemble_prompt(server, &params).await;

    // Scope guard: ensure MCP config cleanup on all exit paths (including errors)
    struct McpCleanup(Option<PathBuf>);
    impl Drop for McpCleanup {
        fn drop(&mut self) {
            if let Some(ref path) = self.0 {
                let _ = std::fs::remove_file(path);
            }
        }
    }
    let _mcp_cleanup = McpCleanup(mcp_config_path.clone());

    // 3. Build command
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

    // 4. Execute
    let result = run_agent_subprocess(cmd, timeout).await?;

    // 5. Parse output
    let parsed_output = match agent_norm.as_str() {
        "claude" | "claude-code" | "claude-cli" => parse_claude_output(&result.output),
        _ => json!({"text": result.output}),
    };

    let success = result.exit_code.map(|c| c == 0).unwrap_or(false);

    // 7. Build response
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
        "permission_profile": resolve_permission_profile(&params),
        "mcp_injected": mcp_config_path.is_some(),
        "next_steps": [
            format!("Call tachi_complete with dispatch_id='{}' to record the eval.", dispatch_id),
            "Review output and decide if work is acceptable.",
        ],
    });

    serde_json::to_string(&response).map_err(|e| format!("serialize dispatch response: {e}"))
}

// ─── Worktree merge handler ──────────────────────────────────────────────────

pub(crate) async fn handle_approve_merge(
    params: crate::TachiApproveMergeParams,
) -> Result<String, String> {
    let worktree = &params.worktree;

    let branch = if let Some(ref b) = params.branch {
        b.clone()
    } else {
        let out = Command::new("git")
            .args(["-C", worktree, "rev-parse", "--abbrev-ref", "HEAD"])
            .output()
            .await
            .map_err(|e| format!("Failed to get branch from worktree: {e}"))?;
        if !out.status.success() {
            return Err(format!(
                "Not a valid git worktree: {}",
                String::from_utf8_lossy(&out.stderr)
            ));
        }
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    };

    let repo_root_out = Command::new("git")
        .args(["-C", worktree, "rev-parse", "--path-format=absolute", "--git-common-dir"])
        .output()
        .await
        .map_err(|e| format!("Failed to find repo root: {e}"))?;
    let git_common_dir = String::from_utf8_lossy(&repo_root_out.stdout).trim().to_string();
    let repo_root = std::path::Path::new(&git_common_dir)
        .parent()
        .unwrap_or(std::path::Path::new(&git_common_dir))
        .to_string_lossy()
        .to_string();

    let strategy = params.strategy.as_deref().unwrap_or("recursive");
    let merge_out = Command::new("git")
        .args(["-C", &repo_root, "merge", "--strategy", strategy, &branch])
        .output()
        .await
        .map_err(|e| format!("Merge command failed: {e}"))?;

    let merge_stdout = String::from_utf8_lossy(&merge_out.stdout).to_string();
    let merge_stderr = String::from_utf8_lossy(&merge_out.stderr).to_string();

    if !merge_out.status.success() {
        return serde_json::to_string(&json!({
            "merged": false,
            "branch": branch,
            "error": merge_stderr,
            "stdout": merge_stdout,
        }))
        .map_err(|e| format!("serialize: {e}"));
    }

    let mut worktree_removed = false;
    if params.delete_worktree {
        let rm_out = Command::new("git")
            .args(["-C", &repo_root, "worktree", "remove", worktree])
            .output()
            .await;
        worktree_removed = rm_out.map(|o| o.status.success()).unwrap_or(false);
    }

    serde_json::to_string(&json!({
        "merged": true,
        "branch": branch,
        "repo_root": repo_root,
        "worktree_removed": worktree_removed,
        "merge_output": merge_stdout.trim(),
    }))
    .map_err(|e| format!("serialize: {e}"))
}
