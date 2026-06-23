use super::*;

// ─── Action handlers ─────────────────────────────────────────────────────────

/// Top-level dispatcher used by `MemoryServer::tachi_shell`.
pub(crate) async fn handle_tachi_shell(
    server: &MemoryServer,
    params: TachiShellParams,
) -> Result<String, String> {
    let action = params.action.to_ascii_lowercase();
    match action.as_str() {
        "brainstorm" | "plan" | "review" | "ship" => {
            handle_stage_action(server, &action, params).await
        }
        "dispatch" => handle_dispatch_action(server, params).await,
        "kanban" => handle_kanban_action(server, params).await,
        "status" => handle_status_action(params).await,
        _ => Err(format!(
            "Invalid action '{}'. Use 'brainstorm', 'plan', 'dispatch', 'kanban', 'status', 'review', or 'ship'.",
            params.action
        )),
    }
}

async fn handle_stage_action(
    _server: &MemoryServer,
    stage: &str,
    params: TachiShellParams,
) -> Result<String, String> {
    let task = params
        .task
        .clone()
        .ok_or_else(|| format!("'task' is required for action='{}'", stage))?;
    let (flow_id, run_dir, created) = resolve_or_create_flow(&params, &task)?;
    let injection = inject_meta_skill(stage, &run_dir).await;

    let instruction = build_instruction_md(
        &flow_id,
        stage,
        &task,
        &injection,
        params.notes.as_deref(),
        &params.validation,
        &params.allowed_scope,
    );
    let instr_path = run_dir.join("instruction.md");
    tokio::fs::write(&instr_path, instruction)
        .await
        .map_err(|e| format!("write instruction.md: {e}"))?;

    advance_stage(&run_dir, &flow_id, stage, &task, &injection, created)?;
    let required_skills = crate::skill_policy::shell_stage_skills(stage);

    let resp = json!({
        "flow_id": flow_id,
        "stage": stage,
        "run_dir": run_dir.to_string_lossy(),
        "instruction_path": instr_path.to_string_lossy(),
        "injected_skill": injection_to_json(&injection),
        "native_skill_policy": crate::skill_policy::native_policy_summary(stage, &required_skills),
        "async": false,
        "created": created,
    });
    serde_json::to_string(&resp).map_err(|e| format!("serialize: {e}"))
}

async fn handle_dispatch_action(
    server: &MemoryServer,
    params: TachiShellParams,
) -> Result<String, String> {
    let task = params
        .task
        .clone()
        .ok_or_else(|| "'task' is required for action='dispatch'".to_string())?;
    let (flow_id, run_dir, created) = resolve_or_create_flow(&params, &task)?;
    let injection = inject_meta_skill("dispatch", &run_dir).await;

    let instruction = build_instruction_md(
        &flow_id,
        "dispatch",
        &task,
        &injection,
        params.notes.as_deref(),
        &params.validation,
        &params.allowed_scope,
    );
    let instr_path = run_dir.join("instruction.md");
    tokio::fs::write(&instr_path, &instruction)
        .await
        .map_err(|e| format!("write instruction.md: {e}"))?;

    advance_stage(&run_dir, &flow_id, "dispatch", &task, &injection, created)?;
    let required_skills = crate::skill_policy::shell_stage_skills("dispatch");

    if !params.slices.is_empty() {
        return handle_convoy_dispatch_action(
            server,
            params,
            &flow_id,
            &run_dir,
            created,
            &injection,
            &instr_path,
        )
        .await;
    }

    // Phase 4 hook: optionally invoke the existing async dispatcher.
    let mut dispatch_id: Option<String> = None;
    let mut dispatch_error: Option<String> = None;
    let mut async_fired = false;
    if params.async_dispatch {
        let agent = params.agent.clone().or_else(|| {
            if params.profile.is_some() {
                None
            } else {
                Some("claude".to_string())
            }
        });
        // Prefix the subagent prompt with a pointer to the instruction packet
        // so the clanker reads from disk rather than chat context.
        let prompt = format!(
            "You are executing Tachi flow `{flow_id}`, stage `dispatch`.\n\n\
             Read and follow these injected SOP files before changing code:\n\
             - {injected}\n\n\
             Then read the full instruction packet:\n\
             - {instr}\n\n\
             Original task:\n\n{task}\n",
            flow_id = flow_id,
            injected = injection.injected_path.as_deref().unwrap_or("(none)"),
            instr = instr_path.to_string_lossy(),
            task = task,
        );
        let dp = TachiDispatchParams {
            agent,
            profile: params.profile.clone(),
            task: prompt,
            cwd: params.cwd.clone(),
            skills: Vec::new(),
            context_query: None,
            model: None,
            timeout_secs: 600,
            permission_profile: None,
            allowed_tools: Vec::new(),
            max_turns: None,
            sandbox: None,
            inject_tachi_mcp: None,
            inject_hub_mcps: None,
            command: Vec::new(),
            harness_transport: None,
            harness_server_url: None,
            project: params.project.clone(),
            stage: Some("execute".to_string()),
            credential_profiles: Vec::new(),
            issue_ref: None,
            pr_ref: None,
            flow_id: Some(flow_id.to_string()),
            tool_profile: params.tool_profile.clone(),
            auto_capability_bundle: None,
            mcp_access: params.mcp_access.clone(),
            allowed_mcp_servers: params.allowed_mcp_servers.clone(),
        };
        match crate::dispatch_ops::handle_tachi_dispatch(server, dp).await {
            Ok(s) => {
                async_fired = true;
                if let Ok(v) = serde_json::from_str::<Value>(&s) {
                    if let Some(d) = v.get("dispatch_id").and_then(|d| d.as_str()) {
                        dispatch_id = Some(d.to_string());
                    }
                }
                // Append dispatch_id to flow status
                if let Some(d) = dispatch_id.as_deref() {
                    let mut status = read_status_async(&run_dir).await;
                    let arr = status
                        .as_object_mut()
                        .map(|o| o.entry("dispatch_ids").or_insert_with(|| json!([])));
                    if let Some(v) = arr {
                        if let Some(a) = v.as_array_mut() {
                            a.push(json!(d));
                        }
                    }
                    if let Err(error) = crate::utils::write_run_status_file(&run_dir, &status) {
                        tracing::warn!(
                            error = %error,
                            run_dir = %run_dir.display(),
                            dispatch_id = %d,
                            "failed to persist shell dispatch id in flow status"
                        );
                    }
                    if let Err(error) = crate::utils::append_run_event(
                        &run_dir,
                        json!({
                            "event": "dispatch_spawned",
                            "flow_id": flow_id,
                            "dispatch_id": d,
                            "timestamp": Utc::now().to_rfc3339(),
                        }),
                    ) {
                        tracing::warn!(
                            error = %error,
                            run_dir = %run_dir.display(),
                            dispatch_id = %d,
                            "failed to append shell dispatch event"
                        );
                    }
                }
            }
            Err(e) => {
                dispatch_error = Some(e);
            }
        }
    }

    let resp = json!({
        "flow_id": flow_id,
        "stage": "dispatch",
        "run_dir": run_dir.to_string_lossy(),
        "instruction_path": instr_path.to_string_lossy(),
        "injected_skill": injection_to_json(&injection),
        "native_skill_policy": crate::skill_policy::native_policy_summary("dispatch", &required_skills),
        "async": async_fired,
        "dispatch_id": dispatch_id,
        "dispatch_error": dispatch_error,
        "created": created,
    });
    serde_json::to_string(&resp).map_err(|e| format!("serialize: {e}"))
}

async fn handle_convoy_dispatch_action(
    server: &MemoryServer,
    params: TachiShellParams,
    flow_id: &str,
    run_dir: &Path,
    created: bool,
    injection: &InjectionResult,
    parent_instr_path: &Path,
) -> Result<String, String> {
    let parent_task = params
        .task
        .as_deref()
        .ok_or_else(|| "'task' is required for action='dispatch'".to_string())?;
    let convoy_superpowers = vec![
        crate::skill_policy::SUPERPOWER_SUBAGENT_DRIVEN_DEVELOPMENT.to_string(),
        crate::skill_policy::SUPERPOWER_EXECUTING_PLANS.to_string(),
        crate::skill_policy::SUPERPOWER_REQUESTING_CODE_REVIEW.to_string(),
    ];
    let parent_worker_skills = crate::skill_policy::worker_skills_for_convoy_slice(parent_task, "");
    let mut convoy_worker_skills = convoy_superpowers.clone();
    convoy_worker_skills.extend(parent_worker_skills);
    crate::skill_policy::dedupe_preserve_order(&mut convoy_worker_skills);
    let mut parent_instruction = build_instruction_md(
        flow_id,
        "dispatch",
        parent_task,
        injection,
        params.notes.as_deref(),
        &params.validation,
        &params.allowed_scope,
    );
    parent_instruction.push_str("## Required Worker Skills\n\n");
    for contract in &convoy_worker_skills {
        parent_instruction.push_str(&format!("- `{}`\n", contract));
    }
    parent_instruction.push('\n');
    parent_instruction.push_str("## Worker Factory Contract\n\n");
    parent_instruction.push_str("- Split only independent, bounded, verifiable slices; keep dependent or same-file work serial.\n");
    parent_instruction.push_str("- Prefer read-only sidecars for inventory/review, and writable workers only with explicit scope.\n");
    parent_instruction.push_str(
        "- Require each worker to report back; leader reviews results before integration.\n\n",
    );
    tokio::fs::write(parent_instr_path, parent_instruction)
        .await
        .map_err(|e| format!("write convoy parent instruction.md: {e}"))?;
    let mut seen = std::collections::HashSet::new();
    let mut slice_records = Vec::new();
    let mut dispatch_ids = Vec::new();
    let mut async_fired = false;

    for (idx, slice) in params.slices.iter().enumerate() {
        let slice_id = resolve_slice_id(idx, slice)?;
        if !seen.insert(slice_id.clone()) {
            return Err(format!("Duplicate convoy slice id: '{}'", slice_id));
        }

        let slice_task = slice
            .task
            .as_deref()
            .filter(|s| !s.trim().is_empty())
            .unwrap_or(parent_task);
        let slice_worker_skills =
            crate::skill_policy::worker_skills_for_convoy_slice(parent_task, slice_task);
        let slice_agent = slice
            .agent
            .clone()
            .or_else(|| params.agent.clone())
            .or_else(|| {
                if slice.profile.is_some() || params.profile.is_some() {
                    None
                } else {
                    Some("claude".to_string())
                }
            });
        let slice_profile = slice.profile.clone().or_else(|| params.profile.clone());
        let slice_cwd = slice.cwd.clone().or_else(|| params.cwd.clone());
        let slice_tool_profile = slice
            .tool_profile
            .clone()
            .or_else(|| params.tool_profile.clone());
        let slice_mcp_access = slice
            .mcp_access
            .clone()
            .or_else(|| params.mcp_access.clone());
        let slice_allowed_mcp_servers = if slice.allowed_mcp_servers.is_empty() {
            params.allowed_mcp_servers.clone()
        } else {
            slice.allowed_mcp_servers.clone()
        };
        let slice_validation = if slice.validation.is_empty() {
            params.validation.clone()
        } else {
            slice.validation.clone()
        };
        let slice_allowed_scope = if slice.allowed_scope.is_empty() {
            params.allowed_scope.clone()
        } else {
            slice.allowed_scope.clone()
        };
        let slice_notes = slice.notes.as_deref().or(params.notes.as_deref());
        let slice_dir = run_dir.join("slices").join(&slice_id);
        tokio::fs::create_dir_all(slice_dir.join("artifacts"))
            .await
            .map_err(|e| format!("create convoy slice dir: {e}"))?;

        let task_packet = match slice.title.as_deref() {
            Some(title) if !title.trim().is_empty() => {
                format!(
                    "Convoy slice `{}` — {}\n\n{}",
                    slice_id,
                    title.trim(),
                    slice_task
                )
            }
            _ => format!("Convoy slice `{}`\n\n{}", slice_id, slice_task),
        };
        let mut instruction = build_instruction_md(
            flow_id,
            "dispatch",
            &task_packet,
            injection,
            slice_notes,
            &slice_validation,
            &slice_allowed_scope,
        );
        instruction.push_str("## Required Worker Skills\n\n");
        for contract in &slice_worker_skills {
            instruction.push_str(&format!("- `{}`\n", contract));
        }
        instruction.push('\n');
        instruction.push_str("## Worker Report-Back Contract\n\n");
        instruction.push_str("- Start final output with `Using skills: <ids>`.\n");
        instruction.push_str("- Report changed files, verification commands and outcomes, blockers, and recommended handoff.\n");
        instruction.push_str("- Do not claim the parent flow is complete; the leader owns integration and final verification.\n\n");
        let slice_instr_path = slice_dir.join("instruction.md");
        tokio::fs::write(&slice_instr_path, instruction)
            .await
            .map_err(|e| format!("write convoy slice instruction.md: {e}"))?;

        crate::utils::append_run_event(
            run_dir,
            json!({
                "event": "convoy_slice_prepared",
                "flow_id": flow_id,
                "slice_id": slice_id,
                "agent": slice_agent.clone(),
                "profile": slice_profile.clone(),
                "cwd": slice_cwd,
                "instruction_path": slice_instr_path.to_string_lossy(),
                "timestamp": Utc::now().to_rfc3339(),
            }),
        )?;

        let mut dispatch_id = None;
        let mut dispatch_error = None;
        if params.async_dispatch {
            let prompt = format!(
                "You are executing Tachi flow `{flow_id}`, convoy slice `{slice_id}`.\n\n\
                 Read and follow these injected SOP files before changing code:\n\
                 - {injected}\n\n\
                 Then read the full slice instruction packet:\n\
                 - {instr}\n\n\
                 Parent flow instruction packet:\n\
                 - {parent_instr}\n\n\
                 Original slice task:\n\n{task}\n",
                flow_id = flow_id,
                slice_id = slice_id,
                injected = injection.injected_path.as_deref().unwrap_or("(none)"),
                instr = slice_instr_path.to_string_lossy(),
                parent_instr = parent_instr_path.to_string_lossy(),
                task = slice_task,
            );
            let dp = TachiDispatchParams {
                agent: slice_agent.clone(),
                profile: slice_profile.clone(),
                task: prompt,
                cwd: slice_cwd.clone(),
                skills: Vec::new(),
                context_query: None,
                model: None,
                timeout_secs: 600,
                permission_profile: None,
                allowed_tools: Vec::new(),
                max_turns: None,
                sandbox: None,
                inject_tachi_mcp: None,
                inject_hub_mcps: None,
                command: Vec::new(),
                harness_transport: None,
                harness_server_url: None,
                project: params.project.clone(),
                stage: Some(format!("execute:{}", slice_id)),
                credential_profiles: Vec::new(),
                issue_ref: None,
                pr_ref: None,
                flow_id: Some(flow_id.to_string()),
                tool_profile: slice_tool_profile.clone(),
                auto_capability_bundle: None,
                mcp_access: slice_mcp_access.clone(),
                allowed_mcp_servers: slice_allowed_mcp_servers.clone(),
            };
            match crate::dispatch_ops::handle_tachi_dispatch(server, dp).await {
                Ok(s) => {
                    async_fired = true;
                    if let Ok(v) = serde_json::from_str::<Value>(&s) {
                        if let Some(d) = v.get("dispatch_id").and_then(|d| d.as_str()) {
                            dispatch_ids.push(d.to_string());
                            dispatch_id = Some(d.to_string());
                        }
                    }
                }
                Err(e) => {
                    dispatch_error = Some(e);
                }
            }
        }

        if let Some(d) = dispatch_id.as_deref() {
            crate::utils::append_run_event(
                run_dir,
                json!({
                    "event": "convoy_dispatch_spawned",
                    "flow_id": flow_id,
                    "slice_id": slice_id,
                    "dispatch_id": d,
                    "timestamp": Utc::now().to_rfc3339(),
                }),
            )?;
        }

        slice_records.push(json!({
            "slice_id": slice_id,
            "title": slice.title,
            "agent": slice_agent.clone(),
            "profile": slice_profile.clone(),
            "tool_profile": slice_tool_profile.clone(),
            "cwd": slice_cwd,
            "instruction_path": slice_instr_path.to_string_lossy(),
            "required_superpowers": convoy_superpowers.clone(),
            "required_worker_skills": slice_worker_skills,
            "dispatch_id": dispatch_id,
            "dispatch_error": dispatch_error,
        }));
    }

    let mut status = read_status_async(run_dir).await;
    if let Some(obj) = status.as_object_mut() {
        let arr = obj.entry("dispatch_ids").or_insert_with(|| json!([]));
        if let Some(a) = arr.as_array_mut() {
            for d in &dispatch_ids {
                a.push(json!(d));
            }
        }
        obj.insert(
            "convoy".to_string(),
            json!({
                "mode": "parallel",
                "slice_count": slice_records.len(),
                "required_superpowers": convoy_superpowers.clone(),
                "required_worker_skills": convoy_worker_skills.clone(),
                "native_skill_policy": crate::skill_policy::native_policy_summary("dispatch", &convoy_worker_skills),
                "slices": slice_records,
                "updated_at": Utc::now().to_rfc3339(),
            }),
        );
    }
    crate::utils::write_run_status_file(run_dir, &status)?;

    let resp = json!({
        "flow_id": flow_id,
        "stage": "dispatch",
        "run_dir": run_dir.to_string_lossy(),
        "instruction_path": parent_instr_path.to_string_lossy(),
        "injected_skill": injection_to_json(injection),
        "native_skill_policy": crate::skill_policy::native_policy_summary("dispatch", &convoy_worker_skills),
        "async": async_fired,
        "convoy": true,
        "dispatch_ids": dispatch_ids,
        "slices": status.get("convoy").and_then(|v| v.get("slices")).cloned().unwrap_or_else(|| json!([])),
        "created": created,
    });
    serde_json::to_string(&resp).map_err(|e| format!("serialize: {e}"))
}

pub(super) fn resolve_slice_id(
    idx: usize,
    slice: &TachiShellDispatchSliceParams,
) -> Result<String, String> {
    let basis = slice
        .id
        .as_deref()
        .or(slice.title.as_deref())
        .or(slice.task.as_deref())
        .unwrap_or("slice");
    let mut id = slugify(basis);
    if id == "flow" || id == "slice" {
        id = format!("slice-{}", idx + 1);
    }
    validate_slice_id(&id)?;
    Ok(id)
}

async fn handle_kanban_action(
    server: &MemoryServer,
    params: TachiShellParams,
) -> Result<String, String> {
    let bp = TachiBoardParams {
        state_filter: params.state_filter.clone(),
        limit: params.limit,
        project: params.project.clone(),
        flow_id: params.flow_id.clone(),
    };
    crate::dispatch_ops::handle_tachi_board(server, bp).await
}

pub(super) async fn handle_status_action(params: TachiShellParams) -> Result<String, String> {
    let runs_root = shell_runs_root();
    if let Some(flow_id) = params.flow_id.as_deref() {
        validate_flow_id(flow_id)?;
        let run_dir = runs_root.join(flow_id);
        if !run_dir.exists() {
            return serde_json::to_string(&json!({
                "flow_id": flow_id,
                "found": false,
                "message": "no run directory exists for this flow_id",
            }))
            .map_err(|e| format!("serialize: {e}"));
        }
        let status = read_status_async(&run_dir).await;
        return serde_json::to_string(&json!({
            "flow_id": flow_id,
            "found": true,
            "run_dir": run_dir.to_string_lossy(),
            "status": status,
        }))
        .map_err(|e| format!("serialize: {e}"));
    }
    // List recent flows
    let limit = params.limit.unwrap_or(20);
    let mut flows = Vec::new();
    match tokio::fs::read_dir(&runs_root).await {
        Ok(mut read_dir) => {
            while let Some(entry) = read_dir
                .next_entry()
                .await
                .map_err(|e| format!("read shell runs root {}: {e}", runs_root.display()))?
            {
                let name = entry.file_name().to_string_lossy().to_string();
                if !name.starts_with("flow_") {
                    continue;
                }
                let status = read_status_async(&entry.path()).await;
                flows.push(json!({
                    "flow_id": name,
                    "stage": status.get("stage"),
                    "state": status.get("state"),
                    "updated_at": status.get("updated_at"),
                }));
            }
        }
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
        Err(err) => {
            return Err(format!(
                "read shell runs root {}: {err}",
                runs_root.display()
            ))
        }
    }
    flows.sort_by(|a, b| {
        let ta = a.get("updated_at").and_then(|v| v.as_str()).unwrap_or("");
        let tb = b.get("updated_at").and_then(|v| v.as_str()).unwrap_or("");
        tb.cmp(ta)
    });
    flows.truncate(limit);
    serde_json::to_string(&json!({
        "flows": flows,
        "count": flows.len(),
        "runs_root": runs_root.to_string_lossy(),
    }))
    .map_err(|e| format!("serialize: {e}"))
}
