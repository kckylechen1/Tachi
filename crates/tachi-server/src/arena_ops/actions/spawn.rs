use super::*;

pub(super) async fn handle_spawn(
    server: &MemoryServer,
    params: TachiArenaParams,
) -> Result<String, String> {
    let arena_id = params
        .arena_id
        .as_deref()
        .ok_or_else(|| "arena_id is required for action='spawn'".to_string())?;
    let prompt = params
        .prompt
        .as_deref()
        .ok_or_else(|| "prompt is required for action='spawn'".to_string())?;
    let arena = arena_dir(arena_id)?;
    if !arena.join("manifest.json").exists() {
        return Err(format!("arena not found: {arena_id}"));
    }
    let mission_id = params
        .mission_id
        .clone()
        .unwrap_or_else(|| new_mission_id(params.role.as_deref(), Some(prompt)));
    validate_mission_id(&mission_id)?;
    let dir = mission_dir(arena_id, &mission_id)?;
    tokio::fs::create_dir_all(&dir)
        .await
        .map_err(|e| format!("create mission dir: {e}"))?;

    let prompt_path = dir.join("prompt.md");
    let plan_path = dir.join("plan.md");
    let result_path = dir.join("result.md");
    let stdout_path = dir.join("stdout.log");
    let stderr_path = dir.join("stderr.log");
    let status_path = dir.join("status.json");
    let now = Utc::now().to_rfc3339();
    let lane = harness_lane(params.harness.as_deref());
    let requested_harness = params.harness.as_deref().unwrap_or("manual");
    let feedback_rules = crate::feedback_rule_ops::applicable_feedback_rules(
        server,
        crate::feedback_rule_ops::FeedbackRuleQuery {
            task: prompt.to_string(),
            task_type: params.role.clone(),
            profile: params.profile.clone(),
            stage: params.role.clone(),
            keywords: params.skills.clone(),
            project: params.project.clone(),
        },
    )
    .await;
    let feedback_rules_section =
        crate::feedback_rule_ops::render_feedback_rules_section(&feedback_rules);
    let feedback_rules_trace = crate::feedback_rule_ops::feedback_rules_trace(&feedback_rules);
    crate::utils::write_owner_only_file_atomic(
        &prompt_path,
        render_prompt_md(
            &params,
            arena_id,
            &mission_id,
            feedback_rules_section.as_deref(),
        )
        .as_bytes(),
    )
    .map_err(|e| format!("write prompt.md: {e}"))?;
    let status = json!({
        "arena_id": arena_id,
        "mission_id": mission_id,
        "state": "ready",
        "task": prompt,
        "harness": lane.id,
        "requested_harness": requested_harness,
        "harness_lane": {
            "id": lane.id,
            "label": lane.label,
            "kind": lane.kind,
            "launch_mode": lane.launch_mode,
            "command_hint": lane.command_hint,
            "mcp_support": lane.mcp_support,
            "artifact_contract": lane.artifact_contract,
            "notes": lane.notes,
        },
        "role": params.role.as_deref().unwrap_or("worker"),
        "cwd": params.cwd.clone(),
        "skills": params.skills.clone(),
        "scope": params.scope.clone(),
        "permissions": params.permissions.clone(),
        "timeout_secs": params.timeout_secs,
        "launch_requested": params.launch,
        "profile": params.profile.clone(),
        "model": params.model.clone(),
        "project": params.project.clone(),
        "flow_id": params.flow_id.clone(),
        "issue_ref": params.issue_ref.clone(),
        "pr_ref": params.pr_ref.clone(),
        "credential_profiles": params.credential_profiles.clone(),
        "tool_profile": params.tool_profile.clone(),
        "auto_capability_bundle": params.auto_capability_bundle,
        "feedback_rules": feedback_rules_trace,
        "created_at": now,
        "updated_at": now,
        "prompt_path": prompt_path,
        "plan_path": plan_path,
        "result_path": result_path,
        "stdout_path": stdout_path,
        "stderr_path": stderr_path,
        "plan_written": false,
        "result_written": false,
        "launched": false,
        "launch_mode": lane.launch_mode,
        "launch_status": if params.launch { "pending" } else { "not_requested" },
    });
    crate::utils::write_json_file_owner_only(&status_path, &status)?;
    crate::utils::append_run_event(
        &arena,
        json!({
            "type": "mission_spawned",
            "arena_id": arena_id,
            "mission_id": mission_id,
            "timestamp": now,
            "harness": lane.id,
            "requested_harness": requested_harness,
            "launch_mode": lane.launch_mode,
        }),
    )?;
    refresh_board(arena_id)?;
    let tracked_prompt = tracked_worker_prompt(&lane, &prompt_path, &plan_path, &result_path);
    let launch_result = if params.launch {
        if let Some(dispatch_params) = dispatch_params_for_mission(&params, &lane, &tracked_prompt)
        {
            match crate::dispatch_ops::handle_tachi_dispatch(server, dispatch_params).await {
                Ok(raw) => {
                    let response =
                        serde_json::from_str::<Value>(&raw).unwrap_or_else(|_| json!({"raw": raw}));
                    let dispatch_id = response
                        .get("dispatch_id")
                        .and_then(Value::as_str)
                        .map(str::to_string);
                    let run_dir = response
                        .get("run_dir")
                        .and_then(Value::as_str)
                        .map(str::to_string);
                    let dispatch_agent = response
                        .get("agent")
                        .and_then(Value::as_str)
                        .map(str::to_string);
                    let dispatch_profile_name = response
                        .get("selected_profile")
                        .and_then(Value::as_str)
                        .map(str::to_string)
                        .or_else(|| params.profile.clone());
                    update_mission_status(
                        arena_id,
                        &mission_id,
                        json!({
                            "state": "running",
                            "launched": true,
                            "launch_status": "launched",
                            "dispatch_id": dispatch_id,
                            "run_dir": run_dir,
                            "dispatch_agent": dispatch_agent,
                            "dispatch_profile_name": dispatch_profile_name,
                            "dispatch_link": dispatch_response_summary(&response),
                        }),
                    )?;
                    Some(json!({
                        "status": "launched",
                        "dispatch_id": dispatch_id,
                        "run_dir": run_dir,
                    }))
                }
                Err(err) => {
                    let status = update_mission_status(
                        arena_id,
                        &mission_id,
                        json!({
                            "state": "launch_failed",
                            "launched": false,
                            "launch_status": "failed",
                            "launch_error": err,
                        }),
                    )?;
                    Some(json!({
                        "status": "failed",
                        "error": status
                            .get("launch_error")
                            .and_then(Value::as_str)
                            .unwrap_or("launch failed"),
                        "recoverable": true,
                        "message": "Mission documents were created; inspect prompt_path/status_path, fix the launcher, then respawn or run tracked_prompt manually.",
                    }))
                }
            }
        } else {
            update_mission_status(
                arena_id,
                &mission_id,
                json!({
                    "launch_status": "document_only",
                    "launch_message": "Harness lane is document/advisor only; use tracked_prompt manually.",
                }),
            )?;
            Some(json!({
                "status": "document_only",
                "message": "Harness lane is document/advisor only; use tracked_prompt manually.",
            }))
        }
    } else {
        None
    };
    let final_status = read_json_file(&status_path)?;
    let compact_status = compact_mission_status(&final_status);

    serde_json::to_string(&json!({
        "tool": "tachi_arena",
        "action": "spawn",
        "arena_id": arena_id,
        "mission_id": mission_id,
        "state": final_status.get("state").cloned().unwrap_or_else(|| json!("ready")),
        "harness": lane.id,
        "requested_harness": requested_harness,
        "launch": launch_result,
        "harness_lane": {
            "id": lane.id,
            "label": lane.label,
            "kind": lane.kind,
            "launch_mode": lane.launch_mode,
            "command_hint": lane.command_hint,
            "mcp_support": lane.mcp_support,
            "artifact_contract": lane.artifact_contract,
            "notes": lane.notes,
        },
        "mission_dir": dir,
        "prompt_path": prompt_path,
        "plan_path": plan_path,
        "result_path": result_path,
        "status_path": status_path,
        "tracked_prompt": tracked_prompt,
        "status": compact_status,
    }))
    .map_err(|e| format!("serialize arena spawn: {e}"))
}
