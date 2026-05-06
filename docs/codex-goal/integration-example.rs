// dispatch.rs 集成示例 — 展示如何在现有 dispatch 流程中加入 Goal 支持
//
// 这是现有 handle_tachi_dispatch() 函数的修改版，标注了所有 "GOAL INTEGRATION" 点

pub(crate) async fn handle_tachi_dispatch_with_goal(
    server: &MemoryServer,
    params: TachiDispatchParams,
) -> Result<String, String> {
    let now = Utc::now();
    let dispatch_id = new_dispatch_id(now, &params.agent);
    let agent_norm = params.agent.to_ascii_lowercase();
    let timeout = Duration::from_secs(params.timeout_secs);

    // ─── GOAL INTEGRATION #1: Initialize goal state ──────────────────────
    // Maps to: Codex's ThreadGoal creation when /goal command is issued
    let mut goal = params.goal.clone().unwrap_or_else(|| DispatchGoal {
        task: params.task.clone(),
        status: GoalStatus::Active,
        turn_budget: params.max_turns,
        turns_used: 0,
        elapsed_seconds: 0,
        audit_required: true,
        degradation_chain: vec![
            "skill:superpowers-executing-plans".to_string(),
            "skill:superpowers-verification".to_string(),
            "skill:superpowers-handoff".to_string(),
        ],
        audit_checklist: Vec::new(),
    });

    // Create workspace directory
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

    // ... MCP config generation ...

    // ─── GOAL INTEGRATION #2: Assemble prompt with goal continuation ─────
    // Maps to: Codex's continuation_prompt() + CONTINUATION_PROMPT_TEMPLATE.render()
    let prompt = assemble_prompt_with_goal(server, &params, Some(&goal)).await;

    let (effective_skills_for_files, _) = resolve_effective_skills(&params);
    let plan_path = workspace_dir.join("plan.md");
    std::fs::write(&plan_path, &prompt).map_err(|e| format!("Failed to write plan file: {e}"))?;

    // Write prompt.md and context.md (unchanged)
    // ...

    // ─── GOAL INTEGRATION #3: Enhanced trajectory with goal state ────────
    // Maps to: Codex's SQLite thread_goals table updates
    let trajectory_path = workspace_dir.join("trajectory.jsonl");
    {
        let started_event = json!({
            "event": "dispatch_started",
            "dispatch_id": dispatch_id,
            "agent": params.agent,
            "stage": params.stage,
            "timestamp": Utc::now().to_rfc3339(),
            // NEW: Goal tracking
            "goal_status": goal.status,
            "turn_budget": goal.turn_budget,
            "audit_required": goal.audit_required,
        });
        let line = serde_json::to_string(&started_event)
            .map_err(|e| format!("Failed to serialize started event: {e}"))?;
        std::fs::write(&trajectory_path, format!("{}\n", line))
            .map_err(|e| format!("Failed to write trajectory.jsonl: {e}"))?;
    }

    // Initialize kanban task (enhanced with goal info)
    init_kanban_task_with_goal(
        server,
        &dispatch_id,
        &params,
        Some(&plan_path.to_string_lossy()),
        Some(&goal),
    )
    .await?;

    // Build command (with degradation skill if budget limited)
    let skills_for_cmd = if goal.status == GoalStatus::BudgetLimited {
        // Maps to: Codex switching to budget_limit.md template
        resolve_degradation_skill(&goal, &effective_skills_for_files)
    } else {
        effective_skills_for_files.clone()
    };

    let mut cmd = match agent_norm.as_str() {
        "claude" | "claude-code" | "claude-cli" => {
            build_claude_command(
                &params,
                &prompt,
                mcp_config_path.as_ref(),
                Some(&skills_for_cmd), // GOAL INTEGRATION: pass degraded skills
            )
        } // ... codex, custom ...
    };

    // ... spawn subprocess with Watchdog ...

    // ─── GOAL INTEGRATION #4: Watchdog with goal progress tracking ───────
    // Maps to: Codex's goal accounting on every turn completion
    tokio::task::spawn(async move {
        let result = run_agent_subprocess(cmd, timeout).await;

        // Update goal progress from subprocess result
        let turns_consumed = estimate_turns_from_output(
            &result
                .as_ref()
                .ok()
                .map(|r| r.output.clone())
                .unwrap_or_default(),
        );
        let elapsed = result
            .as_ref()
            .ok()
            .map(|r| r.duration_ms / 1000)
            .unwrap_or(0);
        let new_status = update_goal_progress(&mut goal, turns_consumed, elapsed);

        // Append goal progress event to trajectory
        {
            let progress_event = json!({
                "event": "goal_progress_updated",
                "dispatch_id": dispatch_id,
                "goal_status": new_status,
                "turns_used": goal.turns_used,
                "turn_budget": goal.turn_budget,
                "elapsed_seconds": goal.elapsed_seconds,
                "timestamp": Utc::now().to_rfc3339(),
            });
            if let Ok(line) = serde_json::to_string(&progress_event) {
                use std::io::Write;
                if let Ok(mut f) = std::fs::OpenOptions::new()
                    .append(true)
                    .open(&trajectory_path_for_spawn)
                {
                    let _ = writeln!(f, "{}", line);
                }
            }
        }

        // Watchdog logic (enhanced with goal state awareness)
        tokio::time::sleep(Duration::from_secs(2)).await;

        let kanban_state = get_kanban_state(&server_clone, &d_id).await;
        let is_closed = matches!(
            kanban_state.as_deref(),
            Some("TASK_STATE_COMPLETED" | "TASK_STATE_FAILED" | "TASK_STATE_CANCELED")
        );

        if !is_closed {
            let exited_ok = matches!(&result, Ok(r) if r.exit_code == Some(0)
            );

            if exited_ok {
                // Check if goal requires audit
                if goal.audit_required && goal.status != GoalStatus::Complete {
                    // Goal exited 0 but audit not passed — don't mark complete
                    let tail = tail_chars(&full_output, 500);
                    eprintln!(
                        "[watchdog] dispatch {} exited 0 but goal audit not passed; \
                         marking kanban INPUT_REQUIRED. tail={}",
                        d_id, tail
                    );
                    let _ = update_kanban_state(
                        &server_clone,
                        &d_id,
                        "TASK_STATE_INPUT_REQUIRED",
                        None,
                        Some(false),
                    )
                    .await;
                } else {
                    // Normal path: mark complete
                    let _ = update_kanban_state(
                        &server_clone,
                        &d_id,
                        "TASK_STATE_COMPLETED",
                        None,
                        Some(false),
                    )
                    .await;
                }
            } else {
                // Failure path (unchanged)
                // ...
            }
        }
    });

    // Return response with goal info
    let response = json!({
        "dispatch_id": dispatch_id,
        "task": {
            "id": dispatch_id,
            "status": { "state": goal.status.to_string() },
        },
        "goal": {
            "status": goal.status,
            "turn_budget": goal.turn_budget,
            "audit_required": goal.audit_required,
        },
        "agent": agent_norm,
        "message": "Task dispatched with goal tracking.",
        "plan_file": plan_path.to_string_lossy(),
        "run_dir": workspace_dir.to_string_lossy(),
    });

    serde_json::to_string(&response).map_err(|e| format!("serialize: {e}"))
}

// ═══════════════════════════════════════════════════════════════════════════════
// prompt.rs 集成示例
// ═══════════════════════════════════════════════════════════════════════════════

/// Enhanced assemble_prompt that supports goal continuation templates
/// Maps to: Codex's continuation_prompt() integration in the turn loop
pub(crate) async fn assemble_prompt_with_goal(
    server: &MemoryServer,
    params: &TachiDispatchParams,
    goal: Option<&DispatchGoal>,
) -> String {
    let mut parts: Vec<String> = Vec::new();

    // Resolve skills with stage defaults
    let (effective_skills, extra_instruction) = resolve_effective_skills(params);

    // 1. Context from memory/wiki (unchanged)
    // ...

    // 2. Skill definitions (unchanged)
    // ...

    // 3. Avoidance notes (unchanged)
    // ...

    // ─── GOAL INTEGRATION: Inject continuation template ────────────────────
    // Maps to: Codex's CONTINUATION_PROMPT_TEMPLATE.render() at turn start
    if let Some(g) = goal {
        match g.status {
            GoalStatus::Active => {
                let continuation = render_continuation_template(
                    g,
                    params.stage.as_deref().unwrap_or("none"),
                    &effective_skills,
                );
                parts.push("## Goal Context".to_string());
                parts.push(continuation);
                parts.push(String::new());
            }
            GoalStatus::BudgetLimited => {
                let budget_limit =
                    render_budget_limit_template(g, params.stage.as_deref().unwrap_or("none"));
                parts.push("## Budget Limit Reached".to_string());
                parts.push(budget_limit);
                parts.push(String::new());

                // Also inject degradation skill instructions
                parts.push("## Degraded Mode".to_string());
                parts.push(
                    "You are now in degraded mode due to budget exhaustion. \
                     Focus on summarizing progress and identifying next steps. \
                     Do not start new work."
                        .to_string(),
                );
                parts.push(String::new());
            }
            _ => {}
        }
    }

    // 4. Operating instructions (enhanced with audit requirement)
    parts.push("## Operating instructions".to_string());
    parts.push("- Use Tachi MCP tools if available for additional context.".to_string());
    parts.push("- Call `tachi_complete` when done, including dispatch_id if provided.".to_string());

    // NEW: Enforce audit if goal requires it
    if goal.map(|g| g.audit_required).unwrap_or(false) {
        parts.push(
            "- **AUDIT REQUIRED**: Before calling `tachi_complete`, you MUST perform \
             a completion audit. Verify every requirement against concrete evidence. \
             Include the audit checklist in your completion notes."
                .to_string(),
        );
    }

    parts.push(String::new());

    // 5. Extra instruction from stage (unchanged)
    if let Some(ref instr) = extra_instruction {
        parts.push(instr.clone());
        parts.push(String::new());
    }

    // 6. Task itself (enhanced with goal framing)
    if let Some(g) = goal {
        parts.push(format!(
            "## Task (Goal: {})",
            g.status.to_string().to_uppercase()
        ));
    } else {
        parts.push("## Task".to_string());
    }
    parts.push(params.task.clone());

    parts.join("\n\n")
}

// ═══════════════════════════════════════════════════════════════════════════════
// kanban_helpers.rs 集成示例
// ═══════════════════════════════════════════════════════════════════════════════

/// Enhanced init_kanban_task that stores goal metadata
/// Maps to: Codex's SQLite thread_goals table insert
pub(super) async fn init_kanban_task_with_goal(
    server: &MemoryServer,
    dispatch_id: &str,
    params: &TachiDispatchParams,
    plan_path: Option<&str>,
    goal: Option<&DispatchGoal>,
) -> Result<(), String> {
    let text = format!(
        "Dispatch Task\nAgent: {}\nTask: {}\nPlan: {}",
        params.agent,
        params.task,
        plan_path.unwrap_or("inline"),
    );

    let mut metadata = json!({
        "type": "a2a_task",
        "dispatch_id": dispatch_id,
        "a2a_state": "TASK_STATE_WORKING",
        "agent": params.agent,
        "plan_file": plan_path,
        "eval_ledger_id": null,
    });

    // GOAL INTEGRATION: Add goal metadata to kanban row
    if let Some(g) = goal {
        if let Some(obj) = metadata.as_object_mut() {
            obj.insert("goal_task".to_string(), json!(g.task));
            obj.insert("goal_audit_required".to_string(), json!(g.audit_required));
            obj.insert("goal_turn_budget".to_string(), json!(g.turn_budget));
            obj.insert("goal_turns_used".to_string(), json!(g.turns_used));
        }
    }

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

// ═══════════════════════════════════════════════════════════════════════════════
// complete_ops.rs 集成示例
// ═══════════════════════════════════════════════════════════════════════════════

/// Enhanced tachi_complete handler with audit validation
/// Maps to: Codex's implicit audit enforcement via prompt
pub(crate) async fn handle_tachi_complete_with_audit(
    server: &MemoryServer,
    params: TachiCompleteParams,
) -> Result<String, String> {
    // Retrieve goal from kanban or dispatch context
    let goal = load_goal_for_dispatch(server, params.dispatch_id.as_deref())
        .await
        .unwrap_or_else(|_| DispatchGoal {
            task: params.task.clone(),
            status: GoalStatus::Active,
            turn_budget: None,
            turns_used: 0,
            elapsed_seconds: 0,
            audit_required: false,
            degradation_chain: Vec::new(),
            audit_checklist: Vec::new(),
        });

    // Validate audit if required
    if goal.audit_required && params.outcome == "success" {
        validate_completion_audit(&goal, &params.outcome, params.notes.as_deref())
            .map_err(|e| format!("Audit validation failed: {e}"))?;
    }

    // Update goal status based on outcome
    let final_goal_status = match params.outcome.as_str() {
        "success" => GoalStatus::Complete,
        "failure" => GoalStatus::Failed,
        "partial" => GoalStatus::BudgetLimited,
        _ => goal.status,
    };

    // Record eval with goal metadata
    let eval_metadata = json!({
        "agent": params.agent,
        "outcome": params.outcome,
        "dispatch_id": params.dispatch_id,
        "goal_status": final_goal_status,
        "audit_required": goal.audit_required,
        "audit_checklist": goal.audit_checklist,
    });

    // ... save eval ledger ...

    // Update kanban with goal status
    if let Some(ref dispatch_id) = params.dispatch_id {
        let kanban_state = match final_goal_status {
            GoalStatus::Complete => "TASK_STATE_COMPLETED",
            GoalStatus::Failed => "TASK_STATE_FAILED",
            GoalStatus::BudgetLimited => "TASK_STATE_INPUT_REQUIRED",
            _ => "TASK_STATE_WORKING",
        };

        let _ = update_kanban_state(
            server,
            dispatch_id,
            kanban_state,
            None,
            Some(true), // reviewed = true (explicit tachi_complete)
        )
        .await;
    }

    Ok(json!({
        "recorded": true,
        "task_id": params.task_id,
        "outcome": params.outcome,
        "goal_status": final_goal_status,
    })
    .to_string())
}
