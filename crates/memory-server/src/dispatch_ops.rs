use crate::TachiDispatchParams;
use crate::TachiBoardParams;
use crate::MemoryServer;
use crate::SearchMemoryParams;
use crate::SaveMemoryParams;
use chrono::Utc;
use serde_json::json;
use std::path::PathBuf;
use std::time::Duration;
use tokio::process::Command;

// ─── Kanban helpers ────────────────────────────────────────────────────────────

/// Initialize a kanban task entry in the memory DB
async fn init_kanban_task(
    server: &MemoryServer,
    dispatch_id: &str,
    params: &TachiDispatchParams,
    plan_path: Option<&str>,
) -> Result<(), String> {
    let text = format!(
        "Dispatch Task\nAgent: {}\nTask: {}\nPlan: {}",
        params.agent,
        params.task,
        plan_path.unwrap_or("inline"),
    );
    let metadata = json!({
        "type": "a2a_task",
        "dispatch_id": dispatch_id,
        "a2a_state": "TASK_STATE_WORKING",
        "agent": params.agent,
        "plan_file": plan_path,
        "eval_ledger_id": null,
    });

    crate::memory_search_ops::handle_save_memory(
        server,
        SaveMemoryParams {
            text,
            summary: format!(
                "Kanban: {} via {}",
                params.task.chars().take(80).collect::<String>(),
                params.agent
            ),
            path: format!("/kanban/tasks/{}", dispatch_id),
            importance: 0.7,
            category: "fact".to_string(),
            topic: "kanban".to_string(),
            keywords: vec![
                "kanban".to_string(),
                "dispatch".to_string(),
                params.agent.clone(),
            ],
            persons: Vec::new(),
            entities: Vec::new(),
            location: String::new(),
            scope: "project".to_string(),
            vector: None,
            id: None,
            force: true,
            auto_link: true,
            project: None,
            retention_policy: Some("durable".to_string()),
            domain: Some("system".to_string()),
            timestamp: None,
            metadata: Some(metadata),
        },
    )
    .await?;
    Ok(())
}

/// Check if a kanban task has been properly closed (completed/failed)
async fn check_kanban_is_closed(server: &MemoryServer, dispatch_id: &str) -> bool {
    let path = format!("/kanban/tasks/{}", dispatch_id);
    if let Ok(rows) = crate::memory_search_ops::search_memory_rows(
        server,
        SearchMemoryParams {
            query: dispatch_id.to_string(),
            query_vec: None,
            top_k: 1,
            path_prefix: Some(path),
            include_archived: false,
            candidates_per_channel: 20,
            mmr_threshold: Some(0.7),
            graph_expand_hops: 0,
            graph_relation_filter: None,
            weights: None,
            agent_role: None,
            project: None,
            domain: None,
        },
    )
    .await
    {
        for row in &rows {
            if let Some(meta) = row.get("metadata") {
                if let Some(state) = meta.get("a2a_state").and_then(|v| v.as_str()) {
                    return matches!(
                        state,
                        "TASK_STATE_COMPLETED"
                            | "TASK_STATE_FAILED"
                            | "TASK_STATE_CANCELED"
                    );
                }
            }
        }
    }
    false
}

/// Update kanban task state
pub(crate) async fn update_kanban_state(
    server: &MemoryServer,
    dispatch_id: &str,
    new_state: &str,
    eval_id: Option<&str>,
) -> Result<(), String> {
    let path = format!("/kanban/tasks/{}", dispatch_id);
    let rows = crate::memory_search_ops::search_memory_rows(
        server,
        SearchMemoryParams {
            query: dispatch_id.to_string(),
            query_vec: None,
            top_k: 1,
            path_prefix: Some(path.clone()),
            include_archived: false,
            candidates_per_channel: 20,
            mmr_threshold: Some(0.7),
            graph_expand_hops: 0,
            graph_relation_filter: None,
            weights: None,
            agent_role: None,
            project: None,
            domain: None,
        },
    )
    .await?;

    if let Some(row) = rows.first() {
        if let Some(id) = row.get("id").and_then(|v| v.as_str()) {
            let mut meta = row.get("metadata").cloned().unwrap_or(json!({}));
            if let Some(obj) = meta.as_object_mut() {
                obj.insert("a2a_state".to_string(), json!(new_state));
                if let Some(eid) = eval_id {
                    obj.insert("eval_ledger_id".to_string(), json!(eid));
                }
                obj.insert(
                    "updated_at".to_string(),
                    json!(Utc::now().to_rfc3339()),
                );
            }
            crate::memory_search_ops::handle_save_memory(
                server,
                SaveMemoryParams {
                    text: row
                        .get("text")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string(),
                    summary: format!("Kanban [{}]: {}", new_state, dispatch_id),
                    path,
                    importance: 0.7,
                    category: "fact".to_string(),
                    topic: "kanban".to_string(),
                    keywords: vec!["kanban".to_string()],
                    persons: Vec::new(),
                    entities: Vec::new(),
                    location: String::new(),
                    scope: "project".to_string(),
                    vector: None,
                    id: Some(id.to_string()),
                    force: true,
                    auto_link: true,
                    project: None,
                    retention_policy: Some("durable".to_string()),
                    domain: Some("system".to_string()),
                    timestamp: None,
                    metadata: Some(meta),
                },
            )
            .await?;
        }
    }
    Ok(())
}

// ─── Dispatch result ─────────────────────────────────────────────────────────

#[allow(dead_code)]
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

    // Prompt MUST come before --mcp-config because --mcp-config <configs...>
    // is a varadic arg that swallows all subsequent positional args.
    cmd.arg(prompt);

    // MCP config injection (after prompt to avoid swallowing)
    if let Some(path) = mcp_config_path {
        cmd.arg("--mcp-config").arg(path);
    }

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

#[allow(dead_code)]
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
    let mcp_config_path = if inject_tachi || inject_hub {
        generate_mcp_config(server, &dispatch_id, inject_tachi, inject_hub).await?
    } else {
        None
    };

    // 3. Assemble prompt & write plan file to workspace
    let prompt = assemble_prompt(server, &params).await;
    let plan_path = workspace_dir.join("plan.md");
    std::fs::write(&plan_path, &prompt)
        .map_err(|e| format!("Failed to write plan file: {e}"))?;

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
    let task_desc = params.task.clone();

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

        // --- WATCHDOG: check if sub-agent properly closed the loop ---
        tokio::time::sleep(Duration::from_secs(2)).await; // grace period for tachi_complete to propagate

        let is_closed = check_kanban_is_closed(&server_clone, &d_id).await;
        if !is_closed {
            let (outcome, note) = match &result {
                Err(e) => ("failure".to_string(), format!("Watchdog: {}", e)),
                Ok(r) if r.exit_code.map(|c| c == 0).unwrap_or(false) => {
                    let tail = &r.output[r.output.len().saturating_sub(500)..];
                    ("partial".to_string(), format!("Watchdog: Agent exited 0 but did not call tachi_complete. Output tail: {}", tail))
                }
                Ok(r) => {
                    let tail = &r.output[r.output.len().saturating_sub(500)..];
                    ("failure".to_string(), format!("Watchdog: Agent crashed (exit {:?}). Stderr tail: {}", r.exit_code, tail))
                }
            };

            // System auto-recovery: force close the loop
            let _ = crate::complete_ops::handle_tachi_complete(
                &server_clone,
                crate::TachiCompleteParams {
                    dispatch_id: Some(d_id.clone()),
                    task: task_desc,
                    agent: format!("watchdog/{}", agent_for_watchdog),
                    outcome,
                    notes: Some(note),
                    task_id: None,
                    duration_ms: None,
                    skills_used: Vec::new(),
                    cost_tokens: None,
                    cost_usd: None,
                    quality_score: None,
                    trajectory: None,
                    diff: None,
                    worktree: None,
                    scope: None,
                    project: None,
                },
            )
            .await;

            // Update kanban to FAILED
            let _ = update_kanban_state(&server_clone, &d_id, "TASK_STATE_FAILED", None).await;
        }

        // Cleanup workspace (keep on failure for debugging)
        if is_closed {
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
    });

    serde_json::to_string(&response).map_err(|e| format!("serialize: {e}"))
}

// ─── Task Board (Kanban) handler ──────────────────────────────────────────────

pub(crate) async fn handle_tachi_board(
    server: &MemoryServer,
    params: TachiBoardParams,
) -> Result<String, String> {
    let limit = params.limit.unwrap_or(20);

    let rows = crate::memory_search_ops::search_memory_rows(
        server,
        SearchMemoryParams {
            query: "kanban dispatch task".to_string(),
            query_vec: None,
            top_k: limit,
            path_prefix: Some("/kanban/tasks/".to_string()),
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
    .await?;

    // Filter by state if requested
    let state_filter = params.state_filter.as_deref().unwrap_or("all");
    let filtered: Vec<&serde_json::Value> = if state_filter == "all" {
        rows.iter().collect()
    } else {
        let target_state = match state_filter {
            "working" => "TASK_STATE_WORKING",
            "completed" => "TASK_STATE_COMPLETED",
            "failed" => "TASK_STATE_FAILED",
            "pending" => "TASK_STATE_PENDING",
            "input_required" => "TASK_STATE_INPUT_REQUIRED",
            "canceled" => "TASK_STATE_CANCELED",
            other => other, // allow raw A2A state
        };
        rows.iter()
            .filter(|row| {
                row.get("metadata")
                    .and_then(|m| m.get("a2a_state"))
                    .and_then(|s| s.as_str())
                    == Some(target_state)
            })
            .collect()
    };

    // Build compact board view
    let tasks: Vec<serde_json::Value> = filtered
        .iter()
        .map(|row| {
            let meta = row.get("metadata").cloned().unwrap_or(json!({}));
            json!({
                "dispatch_id": meta.get("dispatch_id"),
                "agent": meta.get("agent"),
                "state": meta.get("a2a_state"),
                "eval_id": meta.get("eval_ledger_id"),
                "summary": row.get("summary"),
                "updated_at": meta.get("updated_at"),
            })
        })
        .collect();

    serde_json::to_string(&json!({
        "board": "kanban",
        "filter": state_filter,
        "count": tasks.len(),
        "tasks": tasks,
    }))
    .map_err(|e| format!("serialize board: {e}"))
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

    if !params.confirm {
        // ── Preview mode: dry-run merge, return diff without committing ──
        let merge_out = Command::new("git")
            .args([
                "-C", &repo_root,
                "merge", "--strategy", strategy,
                "--no-commit", "--no-ff", &branch,
            ])
            .output()
            .await
            .map_err(|e| format!("Merge preview failed: {e}"))?;

        let merge_stdout = String::from_utf8_lossy(&merge_out.stdout).to_string();
        let merge_stderr = String::from_utf8_lossy(&merge_out.stderr).to_string();

        if !merge_out.status.success() {
            return serde_json::to_string(&json!({
                "preview": true,
                "can_merge": false,
                "branch": branch,
                "error": merge_stderr,
            }))
            .map_err(|e| format!("serialize: {e}"));
        }

        // Get the diff of what would be merged
        let diff_out = Command::new("git")
            .args(["-C", &repo_root, "diff", "--stat", "HEAD"])
            .output()
            .await;
        let diff_stat = diff_out
            .map(|o| String::from_utf8_lossy(&o.stdout).to_string())
            .unwrap_or_default();

        // Abort the merge to restore clean state
        let _ = Command::new("git")
            .args(["-C", &repo_root, "merge", "--abort"])
            .output()
            .await;

        return serde_json::to_string(&json!({
            "preview": true,
            "can_merge": true,
            "branch": branch,
            "repo_root": repo_root,
            "merge_output": merge_stdout.trim(),
            "diff_stat": diff_stat.trim(),
            "next_step": "Call approve_merge again with confirm=true to execute the merge.",
        }))
        .map_err(|e| format!("serialize: {e}"));
    }

    // ── Confirm mode: execute the real merge ──
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
