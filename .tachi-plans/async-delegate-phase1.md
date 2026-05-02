# Phase 1 实现计划：Tachi 异步 Dispatch + Watchdog + 看板

## 目标
将 `tachi_dispatch` 从同步阻塞改为异步后台执行，添加 Watchdog 兜底，实现看板系统。

## 修改清单

### 1. dispatch_ops.rs — 核心改造

#### 1a. 新增 Kanban 写入函数
在文件顶部 imports 后新增：

```rust
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
    
    crate::memory_ops::handle_save_memory(
        server,
        crate::SaveMemoryParams {
            text,
            summary: format!("Kanban: {} via {}", params.task.chars().take(80).collect::<String>(), params.agent),
            path: format!("/kanban/tasks/{}", dispatch_id),
            importance: 0.7,
            category: "fact".to_string(),
            topic: "kanban".to_string(),
            keywords: vec!["kanban".to_string(), "dispatch".to_string(), params.agent.clone()],
            scope: Some("project".to_string()),
            force: true,
            metadata: Some(metadata),
            retention_policy: Some("durable".to_string()),
            domain: Some("system".to_string()),
            ..Default::default()
        },
    ).await?;
    Ok(())
}
```

#### 1b. 新增 Kanban 状态检查/更新函数

```rust
/// Check if a kanban task has been properly closed (completed/failed)
async fn check_kanban_is_closed(server: &MemoryServer, dispatch_id: &str) -> bool {
    let path = format!("/kanban/tasks/{}", dispatch_id);
    // Search for the kanban entry and check its metadata
    if let Ok(rows) = crate::memory_search_ops::search_memory_rows(
        server,
        crate::SearchMemoryParams {
            query: dispatch_id.to_string(),
            path_prefix: Some(path),
            top_k: 1,
            ..Default::default()
        },
    ).await {
        for row in &rows {
            if let Some(meta) = row.get("metadata") {
                if let Some(state) = meta.get("a2a_state").and_then(|v| v.as_str()) {
                    return matches!(state, "TASK_STATE_COMPLETED" | "TASK_STATE_FAILED" | "TASK_STATE_CANCELED");
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
    // Find the kanban entry
    let path = format!("/kanban/tasks/{}", dispatch_id);
    let rows = crate::memory_search_ops::search_memory_rows(
        server,
        crate::SearchMemoryParams {
            query: dispatch_id.to_string(),
            path_prefix: Some(path.clone()),
            top_k: 1,
            ..Default::default()
        },
    ).await?;
    
    if let Some(row) = rows.first() {
        if let Some(id) = row.get("id").and_then(|v| v.as_str()) {
            // Update the existing entry's metadata
            let mut meta = row.get("metadata").cloned().unwrap_or(json!({}));
            if let Some(obj) = meta.as_object_mut() {
                obj.insert("a2a_state".to_string(), json!(new_state));
                if let Some(eid) = eval_id {
                    obj.insert("eval_ledger_id".to_string(), json!(eid));
                }
                obj.insert("updated_at".to_string(), json!(Utc::now().to_rfc3339()));
            }
            // Save updated entry
            crate::memory_ops::handle_save_memory(
                server,
                crate::SaveMemoryParams {
                    id: Some(id.to_string()),
                    text: row.get("text").and_then(|v| v.as_str()).unwrap_or("").to_string(),
                    summary: format!("Kanban [{}]: {}", new_state, dispatch_id),
                    path,
                    importance: 0.7,
                    category: "fact".to_string(),
                    topic: "kanban".to_string(),
                    keywords: vec!["kanban".to_string()],
                    force: true,
                    metadata: Some(meta),
                    retention_policy: Some("durable".to_string()),
                    domain: Some("system".to_string()),
                    ..Default::default()
                },
            ).await?;
        }
    }
    Ok(())
}
```

#### 1c. 重写 handle_tachi_dispatch — 异步化核心

将 `handle_tachi_dispatch` 从同步等待改为异步 spawn：

**关键变更点**：
1. 步骤 4 "Execute" — 将 `run_agent_subprocess(cmd, timeout).await?` 替换为 `tokio::task::spawn` 后台执行
2. spawn 内添加 Watchdog 兜底
3. 立即返回 A2A 风格的 `{ task: { id, status: { state: "TASK_STATE_WORKING" } } }`
4. spawn 前调 `init_kanban_task()` 写看板记录

**注意**：保留现有的 `assemble_prompt`、`build_claude_command`、`build_codex_command` 等函数不变。

改造后的 `handle_tachi_dispatch` 核心逻辑：

```rust
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
    init_kanban_task(server, &dispatch_id, &params, Some(&plan_path.to_string_lossy())).await?;

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
                    ("partial".to_string(), format!("Watchdog: Agent exited 0 but did not call tachi_complete. Output tail: {}", &r.output[r.output.len().saturating_sub(500)..]))
                }
                Ok(r) => {
                    ("failure".to_string(), format!("Watchdog: Agent crashed (exit {:?}). Stderr tail: {}", r.exit_code, &r.output[r.output.len().saturating_sub(500)..]))
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
                    ..Default::default()
                },
            ).await;

            // Update kanban to FAILED
            let _ = update_kanban_state(&server_clone, &d_id, "TASK_STATE_FAILED", None).await;
        }

        // Cleanup workspace
        let _ = std::fs::remove_dir_all(workspace_dir);
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
```

**注意：需要保留 `McpCleanup` struct 和 `run_agent_subprocess` 函数不变。**
**注意：需要确认 `complete_ops` 模块路径是否正确 — 检查 tachi_complete 的 handler 在哪个模块中。**

### 2. facade.rs — 新增 tachi_board 参数

在 `TachiCompleteParams` 之后新增：

```rust
// ─── Facade: task board (kanban) ─────────────────────────────────────────────

#[allow(dead_code)]
#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub(crate) struct TachiBoardParams {
    /// Filter by state: "working", "completed", "failed", "all" (default: "all")
    #[serde(default)]
    pub state_filter: Option<String>,

    /// Maximum number of tasks to return (default: 20)
    #[serde(default)]
    pub limit: Option<usize>,

    /// Optional named project DB
    #[serde(default)]
    pub project: Option<String>,
}
```

### 3. tools.rs — 注册 tachi_board tool

找到其他 `#[tool]` 注册的位置（如 `tachi_complete`），在附近新增：

```rust
#[tool(description = "View the task board (kanban) showing all dispatched background tasks and their statuses. Returns a list of tasks with their A2A state (WORKING, COMPLETED, FAILED, etc).")]
async fn tachi_board(&self, #[tool(aggr)] params: TachiBoardParams) -> Result<CallToolResult, McpError> {
    handle_tool!(self, "tachi_board", handle_tachi_board(params))
}
```

### 4. 新增 board handler

在 `dispatch_ops.rs` 或新文件中实现：

```rust
pub(crate) async fn handle_tachi_board(
    server: &MemoryServer,
    params: TachiBoardParams,
) -> Result<String, String> {
    let limit = params.limit.unwrap_or(20);
    
    let rows = crate::memory_search_ops::search_memory_rows(
        server,
        crate::SearchMemoryParams {
            query: "kanban dispatch task".to_string(),
            path_prefix: Some("/kanban/tasks/".to_string()),
            top_k: limit,
            include_archived: false,
            project: params.project,
            ..Default::default()
        },
    ).await?;

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
        rows.iter().filter(|row| {
            row.get("metadata")
                .and_then(|m| m.get("a2a_state"))
                .and_then(|s| s.as_str())
                == Some(target_state)
        }).collect()
    };

    // Build compact board view
    let tasks: Vec<serde_json::Value> = filtered.iter().map(|row| {
        let meta = row.get("metadata").cloned().unwrap_or(json!({}));
        json!({
            "dispatch_id": meta.get("dispatch_id"),
            "agent": meta.get("agent"),
            "state": meta.get("a2a_state"),
            "eval_id": meta.get("eval_ledger_id"),
            "summary": row.get("summary"),
            "updated_at": meta.get("updated_at"),
        })
    }).collect();

    serde_json::to_string(&json!({
        "board": "kanban",
        "filter": state_filter,
        "count": tasks.len(),
        "tasks": tasks,
    })).map_err(|e| format!("serialize board: {e}"))
}
```

### 5. tachi_complete Hook — 自动更新看板

在 `handle_tachi_complete` 函数的**末尾**（成功写入 eval 记录之后），添加看板更新 Hook：

```rust
// --- Kanban Hook: auto-update task board ---
if let Some(ref did) = params.dispatch_id {
    let new_state = match params.outcome.as_str() {
        "success" => "TASK_STATE_COMPLETED",
        "failure" => "TASK_STATE_FAILED",
        "partial" => "TASK_STATE_INPUT_REQUIRED",
        "aborted" => "TASK_STATE_CANCELED",
        _ => "TASK_STATE_FAILED",
    };
    let _ = crate::dispatch_ops::update_kanban_state(
        server, did, new_state, Some(&eval_memory_id),
    ).await;
}
```

这里 `eval_memory_id` 是 `handle_tachi_complete` 内已生成的 eval 记录 ID。需要确认这个变量名在现有代码中的实际名称。

### 6. implement-plan Skill — 注册到 Hub

创建 Hub skill 定义（可通过 tachi CLI 或直接 DB 写入）：

```json
{
  "id": "skill:implement-plan",
  "type": "skill",
  "name": "implement-plan",
  "description": "Forces sub-agent to read a plan file, execute it, and self-close via tachi_complete",
  "definition": {
    "prompt": "# SKILL: implement-plan (CRITICAL SYSTEM PROTOCOL)\n\nYou are an autonomous sub-agent operating headlessly in the background. NO human is watching your output. You MUST complete the task and self-terminate through the standard protocol.\n\n## MANDATORY WORKFLOW:\n1. READ: Read your execution plan file as specified in the task.\n2. EXECUTE: Follow the plan step-by-step using your available tools.\n3. REPORT: Call tachi_save (kind=\"note\") to save a structured summary.\n4. CLOSE LOOP: Call tachi_complete with the dispatch_id from the task, outcome=\"success\"|\"failure\"|\"partial\".\n\n## STRICT CONSTRAINTS:\n- DO NOT output conversational filler. Just call tools and exit.\n- Exiting without calling tachi_complete is a fatal system breach.\n- After 3 unrecoverable errors, call tachi_complete(outcome=\"failure\").\n- If you need human authorization, call tachi_complete(outcome=\"partial\", notes=\"INPUT_REQUIRED: [reason]\").",
    "transport": "inline"
  }
}
```

## 执行顺序

1. 先读 `dispatch_ops.rs` 完整内容确认现有结构
2. 先读 `tools.rs` 找到 tool 注册模式和 `tachi_complete` 的 handler 位置
3. 修改 `dispatch_ops.rs`：添加 kanban 函数 + 重写 handle_tachi_dispatch
4. 修改 `facade.rs`：添加 TachiBoardParams
5. 修改 `tools.rs`：注册 tachi_board
6. 找到 tachi_complete handler 所在模块，添加看板 Hook
7. 编译测试 `cargo check`

## 注意事项

- `MemoryServer` 是 `Clone` 的（Arc 内部），可以安全移入 tokio::spawn
- 保留所有现有函数签名和测试
- `SaveMemoryParams` 需要检查 Default 是否已实现，如果没有需要手动填充所有字段
- 不要修改 `handle_approve_merge`，它与此次变更无关
