use crate::{MemoryServer, TachiArenaParams};
use chrono::Utc;
use serde_json::{json, Value};

use super::dispatch_bridge::{completion_draft_for_mission, dispatch_params_for_mission};
use super::lane::{harness_lane, tracked_worker_prompt};
use super::render::{render_arena_md, render_prompt_md, render_summary_md};
use super::state::{
    active_state, arena_dir, arena_root, dispatch_response_summary, mission_dir, mission_statuses,
    new_arena_id, new_mission_id, nonempty_file, read_arena_artifact, read_json_file,
    read_linked_dispatch_result, refresh_linked_dispatch_fields, update_mission_status,
    validate_arena_id, validate_mission_id, ArenaArtifactRead,
};

fn handle_open(params: TachiArenaParams) -> Result<String, String> {
    let title = params.title.unwrap_or_else(|| "Tachi Arena".to_string());
    let objective = params
        .objective
        .or(params.prompt)
        .ok_or_else(|| "objective or prompt is required for action='open'".to_string())?;
    let arena_id = params
        .arena_id
        .unwrap_or_else(|| new_arena_id(Some(&title), Some(&objective)));
    validate_arena_id(&arena_id)?;
    let dir = arena_dir(&arena_id)?;
    std::fs::create_dir_all(dir.join("missions"))
        .map_err(|e| format!("create arena dir {}: {e}", dir.display()))?;

    let now = Utc::now().to_rfc3339();
    let manifest = json!({
        "arena_id": arena_id,
        "title": title,
        "objective": objective,
        "state": "open",
        "created_at": now,
        "updated_at": now,
        "root": dir,
        "documents": {
            "arena": dir.join("arena.md"),
            "manifest": dir.join("manifest.json"),
            "board": dir.join("board.json"),
            "events": dir.join("events.jsonl"),
        },
    });
    crate::utils::write_json_file_owner_only(&dir.join("manifest.json"), &manifest)?;
    crate::utils::write_json_file_owner_only(
        &dir.join("board.json"),
        &json!({
            "arena_id": arena_id,
            "state": "open",
            "missions": [],
            "updated_at": now,
        }),
    )?;
    crate::utils::write_owner_only_file_atomic(
        &dir.join("arena.md"),
        render_arena_md(
            manifest["arena_id"].as_str().unwrap_or("arena"),
            manifest["title"].as_str().unwrap_or("Tachi Arena"),
            manifest["objective"].as_str().unwrap_or(""),
        )
        .as_bytes(),
    )
    .map_err(|e| format!("write arena.md: {e}"))?;
    crate::utils::append_run_event(
        &dir,
        json!({
            "type": "arena_opened",
            "arena_id": manifest["arena_id"],
            "timestamp": now,
        }),
    )?;
    serde_json::to_string(&json!({
        "tool": "tachi_arena",
        "action": "open",
        "arena_id": manifest["arena_id"],
        "state": "open",
        "arena_dir": dir,
        "manifest_path": dir.join("manifest.json"),
        "board_path": dir.join("board.json"),
        "arena_path": dir.join("arena.md"),
    }))
    .map_err(|e| format!("serialize arena open: {e}"))
}

async fn handle_spawn(server: &MemoryServer, params: TachiArenaParams) -> Result<String, String> {
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
                    update_mission_status(
                        arena_id,
                        &mission_id,
                        json!({
                            "state": "launch_failed",
                            "launched": false,
                            "launch_status": "failed",
                            "launch_error": err,
                        }),
                    )?;
                    return Err(err);
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
        "status": final_status,
    }))
    .map_err(|e| format!("serialize arena spawn: {e}"))
}

fn refresh_board(arena_id: &str) -> Result<Value, String> {
    let dir = arena_dir(arena_id)?;
    let missions = mission_statuses(arena_id)?;
    let board = json!({
        "arena_id": arena_id,
        "state": read_json_file(&dir.join("manifest.json"))
            .ok()
            .and_then(|v| v.get("state").cloned())
            .unwrap_or_else(|| json!("unknown")),
        "missions": missions,
        "updated_at": Utc::now().to_rfc3339(),
    });
    crate::utils::write_json_file_owner_only(&dir.join("board.json"), &board)?;
    Ok(board)
}

fn handle_board(params: TachiArenaParams) -> Result<String, String> {
    if let Some(arena_id) = params.arena_id.as_deref() {
        let board = refresh_board(arena_id)?;
        return serde_json::to_string(&json!({
            "tool": "tachi_arena",
            "action": "board",
            "arena_id": arena_id,
            "result": board,
        }))
        .map_err(|e| format!("serialize arena board: {e}"));
    }

    let root = arena_root();
    let mut arenas = Vec::new();
    if root.exists() {
        for entry in std::fs::read_dir(&root)
            .map_err(|e| format!("read arena root {}: {e}", root.display()))?
        {
            let entry = entry.map_err(|e| format!("read arena entry: {e}"))?;
            let manifest_path = entry.path().join("manifest.json");
            if manifest_path.exists() {
                arenas.push(read_json_file(&manifest_path)?);
            }
        }
    }
    arenas.sort_by(|a, b| {
        a.get("created_at")
            .and_then(Value::as_str)
            .unwrap_or("")
            .cmp(b.get("created_at").and_then(Value::as_str).unwrap_or(""))
    });
    serde_json::to_string(&json!({
        "tool": "tachi_arena",
        "action": "board",
        "arena_root": root,
        "arenas": arenas,
    }))
    .map_err(|e| format!("serialize arena board: {e}"))
}

fn handle_collect(params: TachiArenaParams) -> Result<String, String> {
    let arena_id = params
        .arena_id
        .as_deref()
        .ok_or_else(|| "arena_id is required for action='collect'".to_string())?;
    let mission_ids = if let Some(mission_id) = params.mission_id.as_deref() {
        validate_mission_id(mission_id)?;
        vec![mission_id.to_string()]
    } else {
        mission_statuses(arena_id)?
            .into_iter()
            .filter_map(|s| {
                s.get("mission_id")
                    .and_then(Value::as_str)
                    .map(str::to_string)
            })
            .collect()
    };
    let mut collected = Vec::new();
    for mission_id in mission_ids {
        let dir = mission_dir(arena_id, &mission_id)?;
        let result_path = dir.join("result.md");
        let plan_path = dir.join("plan.md");
        let mut status_before = read_json_file(&dir.join("status.json"))?;
        refresh_linked_dispatch_fields(&mut status_before);
        let mut artifact_read_errors = Vec::new();
        let mut result = match read_arena_artifact(&result_path, "arena mission result") {
            ArenaArtifactRead::Present(raw) => raw,
            ArenaArtifactRead::Missing => String::new(),
            ArenaArtifactRead::Error(err) => {
                artifact_read_errors.push(err);
                String::new()
            }
        };
        let mut result_source = if artifact_read_errors.is_empty() {
            if result.is_empty() {
                "missing"
            } else {
                "mission_result"
            }
        } else {
            "result_read_error"
        };
        if result.trim().is_empty() && artifact_read_errors.is_empty() {
            if let Some(dispatch_id) = status_before.get("dispatch_id").and_then(Value::as_str) {
                let run_dir_hint = status_before.get("run_dir").and_then(Value::as_str);
                if let Some(dispatch_result) =
                    read_linked_dispatch_result(dispatch_id, run_dir_hint)
                {
                    crate::utils::write_owner_only_file_atomic(
                        &result_path,
                        dispatch_result.as_bytes(),
                    )
                    .map_err(|e| format!("write linked dispatch result.md: {e}"))?;
                    result = dispatch_result;
                    result_source = "linked_dispatch_result";
                }
            }
        }
        match read_arena_artifact(&plan_path, "arena mission plan") {
            ArenaArtifactRead::Present(_) | ArenaArtifactRead::Missing => {}
            ArenaArtifactRead::Error(err) => artifact_read_errors.push(err),
        }
        let result_written = nonempty_file(&result_path);
        let plan_written = nonempty_file(&plan_path);
        let artifact_read_error = artifact_read_errors.first().cloned();
        let state = if !artifact_read_errors.is_empty() {
            "artifact_read_error"
        } else if result_written {
            "collected"
        } else {
            "pending_result"
        };
        let status = update_mission_status(
            arena_id,
            &mission_id,
            json!({
                "state": state,
                "collected_at": Utc::now().to_rfc3339(),
                "result_source": result_source,
                "artifact_read_error": artifact_read_error,
                "artifact_read_errors": if artifact_read_errors.is_empty() {
                    Value::Null
                } else {
                    json!(artifact_read_errors.clone())
                },
                "completion_draft": if state == "collected" {
                    completion_draft_for_mission(&status_before, &result_path)
                } else {
                    Value::Null
                },
            }),
        )?;
        collected.push(json!({
            "mission_id": mission_id,
            "state": state,
            "plan_written": plan_written,
            "result_written": result_written,
            "result_source": result_source,
            "plan_path": plan_path,
            "result_path": result_path,
            "result": result,
            "artifact_read_error": artifact_read_error,
            "artifact_read_errors": artifact_read_errors,
            "completion_draft": status.get("completion_draft").cloned().unwrap_or(Value::Null),
            "status": status,
        }));
    }
    refresh_board(arena_id)?;
    crate::utils::append_run_event(
        &arena_dir(arena_id)?,
        json!({
            "type": "missions_collected",
            "arena_id": arena_id,
            "count": collected.len(),
            "timestamp": Utc::now().to_rfc3339(),
        }),
    )?;
    serde_json::to_string(&json!({
        "tool": "tachi_arena",
        "action": "collect",
        "arena_id": arena_id,
        "missions": collected,
    }))
    .map_err(|e| format!("serialize arena collect: {e}"))
}

fn handle_abort(params: TachiArenaParams) -> Result<String, String> {
    let arena_id = params
        .arena_id
        .as_deref()
        .ok_or_else(|| "arena_id is required for action='abort'".to_string())?;
    let mission_id = params
        .mission_id
        .as_deref()
        .ok_or_else(|| "mission_id is required for action='abort'".to_string())?;
    let reason = params
        .reason
        .unwrap_or_else(|| "aborted by leader".to_string());
    let status = update_mission_status(
        arena_id,
        mission_id,
        json!({
            "state": "aborted",
            "completed_at": Utc::now().to_rfc3339(),
            "abort_reason": reason,
        }),
    )?;
    refresh_board(arena_id)?;
    crate::utils::append_run_event(
        &arena_dir(arena_id)?,
        json!({
            "type": "mission_aborted",
            "arena_id": arena_id,
            "mission_id": mission_id,
            "timestamp": Utc::now().to_rfc3339(),
        }),
    )?;
    serde_json::to_string(&json!({
        "tool": "tachi_arena",
        "action": "abort",
        "arena_id": arena_id,
        "mission_id": mission_id,
        "status": status,
    }))
    .map_err(|e| format!("serialize arena abort: {e}"))
}

fn handle_reap(params: TachiArenaParams) -> Result<String, String> {
    let dry_run = params.dry_run.unwrap_or(true);
    let arena_filter = params.arena_id.clone();
    let arenas = if let Some(arena_id) = arena_filter.as_deref() {
        vec![arena_id.to_string()]
    } else {
        let root = arena_root();
        if !root.exists() {
            Vec::new()
        } else {
            std::fs::read_dir(&root)
                .map_err(|e| format!("read arena root: {e}"))?
                .filter_map(Result::ok)
                .filter_map(|entry| entry.file_name().into_string().ok())
                .filter(|name| validate_arena_id(name).is_ok())
                .collect()
        }
    };
    let mut stale = Vec::new();
    for arena_id in arenas {
        let mut changed = false;
        for status in mission_statuses(&arena_id)? {
            let state = status.get("state").and_then(Value::as_str).unwrap_or("");
            if !active_state(state) {
                continue;
            }
            let Some(mission_id) = status.get("mission_id").and_then(Value::as_str) else {
                continue;
            };
            stale.push(json!({
                "arena_id": arena_id,
                "mission_id": mission_id,
                "state": state,
            }));
            if !dry_run {
                update_mission_status(
                    &arena_id,
                    mission_id,
                    json!({
                        "state": "reaped",
                        "completed_at": Utc::now().to_rfc3339(),
                        "reap_reason": params.reason.as_deref().unwrap_or("arena reap"),
                    }),
                )?;
                changed = true;
            }
        }
        if changed {
            refresh_board(&arena_id)?;
        }
    }
    serde_json::to_string(&json!({
        "tool": "tachi_arena",
        "action": "reap",
        "dry_run": dry_run,
        "stale_missions": stale,
    }))
    .map_err(|e| format!("serialize arena reap: {e}"))
}

fn handle_close(params: TachiArenaParams) -> Result<String, String> {
    let arena_id = params
        .arena_id
        .as_deref()
        .ok_or_else(|| "arena_id is required for action='close'".to_string())?;
    let force = params.force;
    let require_collected = params.require_collected.unwrap_or(true);
    let dir = arena_dir(arena_id)?;
    let missions = mission_statuses(arena_id)?;
    let mut blockers = Vec::new();
    for mission in &missions {
        let state = mission.get("state").and_then(Value::as_str).unwrap_or("");
        let result_written = mission
            .get("result_written")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        if active_state(state) {
            blockers.push(json!({
                "mission_id": mission.get("mission_id"),
                "reason": "mission still active",
                "state": state,
            }));
        } else if require_collected && result_written && state != "collected" {
            blockers.push(json!({
                "mission_id": mission.get("mission_id"),
                "reason": "result written but not collected",
                "state": state,
            }));
        }
    }
    if !force && !blockers.is_empty() {
        return serde_json::to_string(&json!({
            "tool": "tachi_arena",
            "action": "close",
            "arena_id": arena_id,
            "state": "blocked",
            "blockers": blockers,
            "message": "abort/reap active missions or collect written results before close; pass force=true to override",
        }))
        .map_err(|e| format!("serialize arena close blocked: {e}"));
    }

    let mut manifest = read_json_file(&dir.join("manifest.json"))?;
    if let Some(obj) = manifest.as_object_mut() {
        obj.insert("state".to_string(), json!("closed"));
        obj.insert("closed_at".to_string(), json!(Utc::now().to_rfc3339()));
        obj.insert("updated_at".to_string(), json!(Utc::now().to_rfc3339()));
    }
    crate::utils::write_json_file_owner_only(&dir.join("manifest.json"), &manifest)?;
    let summary = render_summary_md(arena_id, &missions);
    crate::utils::write_owner_only_file_atomic(&dir.join("summary.md"), summary.as_bytes())
        .map_err(|e| format!("write summary.md: {e}"))?;
    crate::utils::append_run_event(
        &dir,
        json!({
            "type": "arena_closed",
            "arena_id": arena_id,
            "timestamp": Utc::now().to_rfc3339(),
            "force": force,
        }),
    )?;
    refresh_board(arena_id)?;
    serde_json::to_string(&json!({
        "tool": "tachi_arena",
        "action": "close",
        "arena_id": arena_id,
        "state": "closed",
        "summary_path": dir.join("summary.md"),
    }))
    .map_err(|e| format!("serialize arena close: {e}"))
}

pub(crate) async fn handle_tachi_arena(
    _server: &MemoryServer,
    params: TachiArenaParams,
) -> Result<String, String> {
    match params.action.to_ascii_lowercase().as_str() {
        "open" => handle_open(params),
        "spawn" => handle_spawn(_server, params).await,
        "board" => handle_board(params),
        "collect" => handle_collect(params),
        "abort" => handle_abort(params),
        "reap" => handle_reap(params),
        "close" => handle_close(params),
        other => Err(format!(
            "Invalid action '{other}'. Use open, spawn, board, collect, abort, reap, or close."
        )),
    }
}
