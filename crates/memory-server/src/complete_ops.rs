//! Task completion + eval ledger handler.
//!
//! Extracted from `tools.rs` so the watchdog in `dispatch_ops` can call
//! `handle_tachi_complete` directly without going through the MCP tool layer.

use chrono::Utc;

use crate::hub_ops::handle_distill_trajectory;
use crate::memory_search_ops::handle_save_memory;
use crate::tool_params::{DistillTrajectoryParams, SaveMemoryParams, TachiCompleteParams};
use crate::MemoryServer;
use serde_json::json;

pub(crate) async fn handle_tachi_complete(
    server: &MemoryServer,
    params: TachiCompleteParams,
) -> Result<String, String> {
    let now = Utc::now();
    let date = now.format("%Y-%m-%d").to_string();
    let ts = now.format("%Y%m%dT%H%M%SZ").to_string();

    let task_id = params.task_id.clone().unwrap_or_else(|| {
        let agent_slug = params
            .agent
            .replace(|c: char| !c.is_ascii_alphanumeric(), "-")
            .to_ascii_lowercase();
        format!("{}-{}", ts, agent_slug)
    });

    let path = format!("/eval/{}/{}", date, task_id);

    let outcome_norm = params.outcome.to_ascii_lowercase();
    let outcome_emoji = match outcome_norm.as_str() {
        "success" => "✓",
        "failure" => "✗",
        "partial" => "~",
        "aborted" => "⊘",
        _ => "?",
    };

    let duration_display = params
        .duration_ms
        .map(|ms| {
            if ms < 1000 {
                format!("{}ms", ms)
            } else if ms < 60_000 {
                format!("{:.1}s", (ms as f64) / 1000.0)
            } else {
                format!("{:.1}min", (ms as f64) / 60_000.0)
            }
        })
        .unwrap_or_else(|| "?".to_string());

    let cost_display = match (params.cost_tokens, params.cost_usd) {
        (Some(t), Some(u)) => format!(" | {} tok | ${:.4}", t, u),
        (Some(t), None) => format!(" | {} tok", t),
        (None, Some(u)) => format!(" | ${:.4}", u),
        _ => String::new(),
    };

    let mut summary_lines = vec![format!(
        "[{}] {} completed task in {}{}",
        outcome_emoji, params.agent, duration_display, cost_display
    )];
    summary_lines.push(format!("Task: {}", params.task));
    if !params.skills_used.is_empty() {
        summary_lines.push(format!("Skills: {}", params.skills_used.join(", ")));
    }
    if let Some(q) = params.quality_score {
        summary_lines.push(format!("Quality: {:.2}", q));
    }
    if let Some(notes) = &params.notes {
        if !notes.is_empty() {
            summary_lines.push(format!("Notes: {}", notes));
        }
    }
    let text = summary_lines.join("\n");

    let mut keywords: Vec<String> = Vec::new();
    keywords.push(params.agent.clone());
    keywords.push(outcome_norm.clone());
    keywords.push("eval".to_string());
    for skill in &params.skills_used {
        keywords.push(skill.clone());
    }

    let mut metadata_map = serde_json::Map::new();
    metadata_map.insert("task_id".into(), serde_json::json!(task_id));
    metadata_map.insert("agent".into(), serde_json::json!(params.agent));
    metadata_map.insert("outcome".into(), serde_json::json!(outcome_norm));
    if let Some(ms) = params.duration_ms {
        metadata_map.insert("duration_ms".into(), serde_json::json!(ms));
    }
    if !params.skills_used.is_empty() {
        metadata_map.insert("skills_used".into(), serde_json::json!(params.skills_used));
    }
    if let Some(t) = params.cost_tokens {
        metadata_map.insert("cost_tokens".into(), serde_json::json!(t));
    }
    if let Some(u) = params.cost_usd {
        metadata_map.insert("cost_usd".into(), serde_json::json!(u));
    }
    if let Some(q) = params.quality_score {
        metadata_map.insert("quality_score".into(), serde_json::json!(q));
    }
    if let Some(traj) = &params.trajectory {
        metadata_map.insert("trajectory".into(), traj.clone());
    }
    if let Some(diff) = &params.diff {
        if !diff.is_empty() {
            metadata_map.insert("diff".into(), serde_json::json!(diff));
        }
    }
    if let Some(wt) = &params.worktree {
        metadata_map.insert("worktree".into(), serde_json::json!(wt));
    }
    if let Some(did) = &params.dispatch_id {
        metadata_map.insert("dispatch_id".into(), serde_json::json!(did));
    }

    let mem_params = SaveMemoryParams {
        text,
        summary: format!("[{}] {} / {}", outcome_emoji, params.agent, params.task),
        path: path.clone(),
        importance: match outcome_norm.as_str() {
            "success" => 0.55,
            "failure" => 0.75,
            "partial" => 0.6,
            "aborted" => 0.5,
            _ => 0.5,
        },
        category: "experience".to_string(),
        topic: params.task.clone(),
        keywords,
        persons: Vec::new(),
        entities: params.skills_used.clone(),
        location: String::new(),
        scope: params
            .scope
            .clone()
            .unwrap_or_else(|| "project".to_string()),
        vector: None,
        id: None,
        force: false,
        auto_link: true,
        project: params.project.clone(),
        retention_policy: None,
        domain: None,
        timestamp: None,
        metadata: Some(serde_json::Value::Object(metadata_map)),
    };

    let save_result = handle_save_memory(server, mem_params).await?;
    let save_json: serde_json::Value = serde_json::from_str(&save_result)
        .unwrap_or_else(|_| serde_json::json!({"raw": save_result}));

    // Extract the eval memory ID for the kanban hook
    let eval_memory_id = save_json
        .get("id")
        .and_then(|v| v.as_str())
        .unwrap_or(&task_id)
        .to_string();

    let mut pipeline_status = serde_json::json!({
        "distill_trajectory": "skipped (no trajectory data)",
        "skill_evolve": "skipped",
        "post_complete_hooks": "pending",
    });

    if let Some(ref trajectory) = params.trajectory {
        if let Some(trace_arr) = trajectory.as_array() {
            if !trace_arr.is_empty() && outcome_norm == "success" {
                let server_clone = server.clone();
                let task_desc = params.task.clone();
                let agent = params.agent.clone();
                let trace = trace_arr.clone();
                let skills_used = params.skills_used.clone();
                let skill_path = if skills_used.is_empty() {
                    format!("/skills/auto/{}", task_id)
                } else {
                    skills_used[0].clone()
                };
                pipeline_status = serde_json::json!({
                    "distill_trajectory": "enqueued",
                    "skill_evolve": "will follow distill if successful",
                });
                tokio::spawn(async move {
                    let distill_params = DistillTrajectoryParams {
                        task_description: task_desc,
                        execution_trace: trace,
                        final_outcome: serde_json::json!({"outcome": "success", "agent": agent}),
                        agent_id: agent.clone(),
                        skill_path,
                        skill_id: None,
                        importance: None,
                        domain: None,
                        project: None,
                        scope: "project".to_string(),
                    };
                    match handle_distill_trajectory(&server_clone, distill_params).await {
                        Ok(r) => eprintln!(
                            "[tachi_complete/worker] distill OK: {}",
                            &r[..r.len().min(200)]
                        ),
                        Err(e) => eprintln!("[tachi_complete/worker] distill failed: {e}"),
                    }
                });
            }
        }
    }

    // --- Kanban Hook: auto-update task board ---
    if let Some(ref did) = params.dispatch_id {
        let new_state = match params.outcome.as_str() {
            "success" => "TASK_STATE_COMPLETED",
            "failure" => "TASK_STATE_FAILED",
            "partial" => "TASK_STATE_INPUT_REQUIRED",
            "aborted" => "TASK_STATE_CANCELED",
            _ => "TASK_STATE_FAILED",
        };
        let _ =
            crate::dispatch_ops::update_kanban_state(server, did, new_state, Some(&eval_memory_id))
                .await;
    }

    // --- Post-complete hooks MVP ---
    // On failure/partial with non-empty notes, auto-save a lesson learned entry
    if matches!(outcome_norm.as_str(), "failure" | "partial") {
        if let Some(ref notes) = params.notes {
            if !notes.is_empty() {
                let lesson_date = date.clone();
                let lesson_task_id = task_id.clone();
                let lesson_path = format!("/eval/lessons/{}/{}", lesson_date, lesson_task_id);
                let lesson_text = format!(
                    "Task: {}\nOutcome: {}\nAgent: {}\nSkills: {}\nDispatch ID: {}\n\nNotes:\n{}",
                    params.task,
                    outcome_norm,
                    params.agent,
                    params.skills_used.join(", "),
                    params.dispatch_id.as_deref().unwrap_or("n/a"),
                    notes,
                );
                let lesson_summary = format!(
                    "[{}] Lesson: {}",
                    if outcome_norm == "failure" {
                        "✗"
                    } else {
                        "~"
                    },
                    params.task.chars().take(80).collect::<String>()
                );
                let lesson_params = SaveMemoryParams {
                    text: lesson_text,
                    summary: lesson_summary,
                    path: lesson_path,
                    importance: 0.75,
                    category: "lesson".to_string(),
                    topic: params.task.clone(),
                    keywords: {
                        let mut kw = vec!["lesson".to_string(), outcome_norm.clone()];
                        kw.extend(params.skills_used.iter().cloned());
                        kw
                    },
                    persons: Vec::new(),
                    entities: params.skills_used.clone(),
                    location: String::new(),
                    scope: params
                        .scope
                        .clone()
                        .unwrap_or_else(|| "project".to_string()),
                    vector: None,
                    id: None,
                    force: true,
                    auto_link: true,
                    project: params.project.clone(),
                    retention_policy: Some("durable".to_string()),
                    domain: None,
                    timestamp: None,
                    metadata: Some(json!({
                        "lesson": true,
                        "dispatch_id": params.dispatch_id,
                        "outcome": outcome_norm,
                    })),
                };
                match handle_save_memory(server, lesson_params).await {
                    Ok(_) => {
                        pipeline_status["post_complete_hooks"] = json!("lesson_saved");
                    }
                    Err(e) => {
                        eprintln!("[tachi_complete/post_hook] lesson save failed: {e}");
                        pipeline_status["post_complete_hooks"] =
                            json!(format!("lesson_save_failed: {e}"));
                    }
                }
            } else {
                pipeline_status["post_complete_hooks"] = json!("skipped (no notes)");
            }
        } else {
            pipeline_status["post_complete_hooks"] = json!("skipped (no notes)");
        }
    } else {
        pipeline_status["post_complete_hooks"] = json!("skipped (outcome not failure/partial)");
    }

    let review_bundle = serde_json::json!({
        "recorded": true,
        "task_id": task_id,
        "path": path,
        "outcome": outcome_norm,
        "eval_entry": save_json,
        "next_steps": [
            "Use tachi_search with 'eval' keyword to find related outcomes.",
            "For worktree-based dispatch, run approve_merge when ready.",
        ],
        "pipeline": pipeline_status,
    });

    serde_json::to_string(&review_bundle)
        .map_err(|e| format!("Failed to serialize review bundle: {}", e))
}
