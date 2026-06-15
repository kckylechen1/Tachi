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

fn scrub_eval_string(text: &str, redactions: &mut usize) -> String {
    let (safe, count) = crate::memory_search_ops::scrub_secrets(text);
    *redactions += count;
    safe
}

fn eval_json_key_is_secretish(key: &str) -> bool {
    let key = key.to_ascii_lowercase();
    key.contains("api_key")
        || key.contains("apikey")
        || key.contains("token")
        || key.contains("secret")
        || key.contains("password")
        || key.contains("authorization")
}

fn scrub_eval_json_value(
    key: Option<&str>,
    value: serde_json::Value,
    redactions: &mut usize,
) -> serde_json::Value {
    match value {
        serde_json::Value::String(text) => {
            let safe = scrub_eval_string(&text, redactions);
            if safe != text {
                serde_json::Value::String(safe)
            } else if key.is_some_and(eval_json_key_is_secretish) && !text.trim().is_empty() {
                *redactions += 1;
                serde_json::Value::String("[REDACTED]".to_string())
            } else {
                serde_json::Value::String(text)
            }
        }
        serde_json::Value::Array(items) => serde_json::Value::Array(
            items
                .into_iter()
                .map(|item| scrub_eval_json_value(None, item, redactions))
                .collect(),
        ),
        serde_json::Value::Object(map) => serde_json::Value::Object(
            map.into_iter()
                .map(|(key, value)| {
                    let value = scrub_eval_json_value(Some(&key), value, redactions);
                    (key, value)
                })
                .collect(),
        ),
        other => other,
    }
}

fn scrub_eval_json(value: serde_json::Value, redactions: &mut usize) -> serde_json::Value {
    scrub_eval_json_value(None, value, redactions)
}

fn scrub_eval_strings(values: &[String], redactions: &mut usize) -> Vec<String> {
    values
        .iter()
        .map(|value| scrub_eval_string(value, redactions))
        .collect()
}

fn lesson_task_matches(text: &str, expected_task: &str) -> bool {
    let expected = expected_task.trim();
    if expected.is_empty() {
        return false;
    }

    text.lines()
        .next()
        .and_then(|line| line.trim().strip_prefix("Task:"))
        .map(|task| task.trim().eq_ignore_ascii_case(expected))
        .unwrap_or(false)
}

#[derive(Debug)]
struct KanbanSnapshot {
    scope: &'static str,
    state: Option<String>,
    eval_ledger_id: Option<String>,
    reviewed: Option<bool>,
}

fn read_kanban_snapshot(
    server: &MemoryServer,
    dispatch_id: &str,
) -> Result<Option<KanbanSnapshot>, String> {
    let path = format!("/kanban/tasks/{dispatch_id}");
    let project_entries = if server.has_project_db() {
        server.with_project_store_read(|store| {
            store
                .list_by_path(&path, 1, false)
                .map_err(|e| format!("kanban list_by_path: {e}"))
        })?
    } else {
        Vec::new()
    };
    if let Some(entry) = project_entries.first() {
        return Ok(Some(KanbanSnapshot {
            scope: "project",
            state: entry
                .metadata
                .get("a2a_state")
                .and_then(serde_json::Value::as_str)
                .map(str::to_string),
            eval_ledger_id: entry
                .metadata
                .get("eval_ledger_id")
                .and_then(serde_json::Value::as_str)
                .map(str::to_string),
            reviewed: entry
                .metadata
                .get("reviewed")
                .and_then(serde_json::Value::as_bool),
        }));
    }

    let global_entries = server.with_global_store_read(|store| {
        store
            .list_by_path(&path, 1, false)
            .map_err(|e| format!("kanban list_by_path (global): {e}"))
    })?;
    let Some(entry) = global_entries.first() else {
        return Ok(None);
    };
    Ok(Some(KanbanSnapshot {
        scope: "global",
        state: entry
            .metadata
            .get("a2a_state")
            .and_then(serde_json::Value::as_str)
            .map(str::to_string),
        eval_ledger_id: entry
            .metadata
            .get("eval_ledger_id")
            .and_then(serde_json::Value::as_str)
            .map(str::to_string),
        reviewed: entry
            .metadata
            .get("reviewed")
            .and_then(serde_json::Value::as_bool),
    }))
}

async fn run_lesson_post_complete_hook(
    server: &MemoryServer,
    params: &TachiCompleteParams,
    outcome_norm: &str,
    safe_notes: Option<&str>,
    safe_task: &str,
    safe_agent: &str,
    safe_skills_used: &[String],
    date: &str,
    task_id: &str,
) -> serde_json::Value {
    if !matches!(outcome_norm, "failure" | "partial") {
        return json!("skipped (outcome not failure/partial)");
    }

    let Some(notes) = safe_notes.filter(|notes| !notes.is_empty()) else {
        return json!("skipped (no notes)");
    };

    let lesson_scope_str = params
        .scope
        .clone()
        .unwrap_or_else(|| "project".to_string());
    let (lesson_db, _) = server.resolve_write_scope(&lesson_scope_str);
    let task_ref = safe_task.to_string();
    let skills_set: std::collections::HashSet<&str> =
        safe_skills_used.iter().map(|s| s.as_str()).collect();
    let outcome_ref = outcome_norm.to_string();

    let dedup_hit = server
        .with_store_for_scope_read(lesson_db, |store| {
            let conn = store.connection();
            let mut stmt = conn
                .prepare(
                    "SELECT id, text, metadata FROM memories \
                     WHERE path LIKE '/eval/lessons/%' \
                       AND id NOT LIKE 'foundry:%' \
                     ORDER BY created_at DESC LIMIT 30",
                )
                .map_err(|e| format!("lesson dedup query: {e}"))?;
            let rows = stmt
                .query_map([], |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                    ))
                })
                .map_err(|e| format!("lesson dedup iter: {e}"))?;
            for row in rows {
                let (id, text, meta_str) = row.map_err(|e| format!("{e}"))?;
                let meta: serde_json::Value = serde_json::from_str(&meta_str).unwrap_or(json!({}));
                let same_outcome = meta
                    .get("outcome")
                    .and_then(|v| v.as_str())
                    .map(|o| o == outcome_ref)
                    .unwrap_or(false);
                let task_match = lesson_task_matches(&text, &task_ref);
                let skill_overlap = same_outcome
                    && !skills_set.is_empty()
                    && meta
                        .get("skills_used")
                        .and_then(|v| v.as_array())
                        .map(|arr| {
                            arr.iter()
                                .filter_map(|v| v.as_str())
                                .any(|s| skills_set.contains(s))
                        })
                        .unwrap_or(false);
                if task_match || skill_overlap {
                    return Ok(Some((id, meta)));
                }
            }
            Ok(None)
        })
        .unwrap_or(None);

    if let Some((existing_id, mut existing_meta)) = dedup_hit {
        let count = existing_meta
            .get("count")
            .and_then(|v| v.as_u64())
            .unwrap_or(1)
            + 1;
        existing_meta["count"] = json!(count);
        existing_meta["last_seen"] = json!(Utc::now().to_rfc3339());
        let updated_meta = serde_json::to_string(&existing_meta).unwrap_or_default();
        let eid = existing_id.clone();
        let update_result = server.with_store_for_scope(lesson_db, |store| {
            store
                .connection()
                .execute(
                    "UPDATE memories SET metadata = ?1 WHERE id = ?2",
                    rusqlite::params![updated_meta, eid],
                )
                .map_err(|e| format!("lesson dedup update: {e}"))
        });
        return match update_result {
            Ok(_) => json!(format!("lesson_deduped (id={existing_id}, count={count})")),
            Err(e) => {
                eprintln!("[tachi_complete/post_hook] lesson dedup update failed: {e}");
                json!(format!("lesson_dedup_failed: {e}"))
            }
        };
    }

    let lesson_path = format!("/eval/lessons/{}/{}", date, task_id);
    let lesson_text = format!(
        "Task: {}\nOutcome: {}\nAgent: {}\nSkills: {}\nDispatch ID: {}\n\nNotes:\n{}",
        safe_task,
        outcome_norm,
        safe_agent,
        safe_skills_used.join(", "),
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
        safe_task.chars().take(80).collect::<String>()
    );
    let lesson_params = SaveMemoryParams {
        text: lesson_text,
        summary: lesson_summary,
        path: lesson_path,
        importance: 0.75,
        category: "lesson".to_string(),
        topic: safe_task.to_string(),
        keywords: {
            let mut kw = vec!["lesson".to_string(), outcome_norm.to_string()];
            kw.extend(safe_skills_used.iter().cloned());
            kw
        },
        persons: Vec::new(),
        entities: safe_skills_used.to_vec(),
        location: String::new(),
        scope: lesson_scope_str,
        vector: None,
        id: None,
        force: true,
        auto_link: true,
        project: params.project.clone(),
        retention_policy: Some("durable".to_string()),
        domain: None,
        timestamp: None,
        valid_from: None,
        valid_until: None,
        metadata: Some(json!({
            "lesson": true,
            "dispatch_id": params.dispatch_id.clone(),
            "outcome": outcome_norm,
            "count": 1,
            "last_seen": Utc::now().to_rfc3339(),
            "skills_used": safe_skills_used,
        })),
    };
    match handle_save_memory(server, lesson_params).await {
        Ok(_) => json!("lesson_saved"),
        Err(e) => {
            eprintln!("[tachi_complete/post_hook] lesson save failed: {e}");
            json!(format!("lesson_save_failed: {e}"))
        }
    }
}

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
    let mut secret_redactions = 0usize;
    let safe_task = scrub_eval_string(&params.task, &mut secret_redactions);
    let safe_agent = scrub_eval_string(&params.agent, &mut secret_redactions);
    let safe_notes = params
        .notes
        .as_ref()
        .map(|notes| scrub_eval_string(notes, &mut secret_redactions));
    let safe_skills_used = scrub_eval_strings(&params.skills_used, &mut secret_redactions);
    let safe_evidence_refs = scrub_eval_strings(&params.evidence_refs, &mut secret_redactions);
    let safe_tests_run = scrub_eval_strings(&params.tests_run, &mut secret_redactions);
    let safe_trajectory = params
        .trajectory
        .as_ref()
        .map(|trajectory| scrub_eval_json(trajectory.clone(), &mut secret_redactions));
    let safe_diff = params
        .diff
        .as_ref()
        .map(|diff| scrub_eval_string(diff, &mut secret_redactions));
    let safe_subagents = scrub_eval_json(json!(&params.subagents), &mut secret_redactions);
    let safe_feedback_rules =
        scrub_eval_strings(&params.feedback_rules_applied, &mut secret_redactions);

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
        outcome_emoji, safe_agent, duration_display, cost_display
    )];
    summary_lines.push(format!("Task: {}", safe_task));
    if !safe_skills_used.is_empty() {
        summary_lines.push(format!("Skills: {}", safe_skills_used.join(", ")));
    }
    if let Some(profile) = params.profile.as_deref().filter(|s| !s.is_empty()) {
        summary_lines.push(format!("Profile: {}", profile));
    }
    if let Some(issue_ref) = params.issue_ref.as_deref().filter(|s| !s.is_empty()) {
        summary_lines.push(format!("Issue: {}", issue_ref));
    }
    if let Some(pr_ref) = params.pr_ref.as_deref().filter(|s| !s.is_empty()) {
        summary_lines.push(format!("PR: {}", pr_ref));
    }
    if !params.tests_run.is_empty() {
        summary_lines.push(format!("Tests: {}", params.tests_run.join("; ")));
    }
    if let Some(q) = params.quality_score {
        summary_lines.push(format!("Quality: {:.2}", q));
    }
    if let Some(notes) = &safe_notes {
        if !notes.is_empty() {
            summary_lines.push(format!("Notes: {}", notes));
        }
    }
    let text = summary_lines.join("\n");

    let mut keywords: Vec<String> = Vec::new();
    keywords.push(safe_agent.clone());
    keywords.push(outcome_norm.clone());
    keywords.push("eval".to_string());
    for skill in &safe_skills_used {
        keywords.push(skill.clone());
    }
    if let Some(profile) = params.profile.as_deref().filter(|s| !s.is_empty()) {
        keywords.push(profile.to_string());
        keywords.push("dispatch_profile".to_string());
    }
    if let Some(risk) = params.risk.as_deref().filter(|s| !s.is_empty()) {
        keywords.push(format!("risk:{risk}"));
    }
    let entities = safe_skills_used.clone();
    if !params.subagents.is_empty() {
        keywords.push("subagent_eval".to_string());
    }

    let mut metadata_map = serde_json::Map::new();
    metadata_map.insert("task_id".into(), serde_json::json!(task_id));
    metadata_map.insert("agent".into(), serde_json::json!(safe_agent.clone()));
    metadata_map.insert("outcome".into(), serde_json::json!(outcome_norm));
    if let Some(task_type) = &params.task_type {
        if !task_type.is_empty() {
            metadata_map.insert("task_type".into(), serde_json::json!(task_type));
        }
    }
    if let Some(profile) = &params.profile {
        if !profile.is_empty() {
            metadata_map.insert("profile".into(), serde_json::json!(profile));
        }
    }
    if let Some(risk) = &params.risk {
        if !risk.is_empty() {
            metadata_map.insert("risk".into(), serde_json::json!(risk));
        }
    }
    if let Some(ms) = params.duration_ms {
        metadata_map.insert("duration_ms".into(), serde_json::json!(ms));
    }
    if !safe_skills_used.is_empty() {
        metadata_map.insert(
            "skills_used".into(),
            serde_json::json!(safe_skills_used.clone()),
        );
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
    if let Some(traj) = &safe_trajectory {
        metadata_map.insert("trajectory".into(), traj.clone());
    }
    if let Some(diff) = &safe_diff {
        if !diff.is_empty() {
            metadata_map.insert("diff".into(), serde_json::json!(diff));
        }
    }
    let diff_present = params.diff_present.unwrap_or_else(|| {
        params
            .diff
            .as_deref()
            .is_some_and(|diff| !diff.trim().is_empty())
    });
    metadata_map.insert("diff_present".into(), serde_json::json!(diff_present));
    let verification_present =
        !params.tests_run.is_empty() || !params.evidence_refs.is_empty() || diff_present;
    metadata_map.insert(
        "verification_present".into(),
        serde_json::json!(verification_present),
    );
    if let Some(wt) = &params.worktree {
        metadata_map.insert(
            "worktree".into(),
            serde_json::json!(scrub_eval_string(wt, &mut secret_redactions)),
        );
    }
    if !params.subagents.is_empty() {
        let roles: Vec<String> = params.subagents.iter().map(|s| s.role.clone()).collect();
        let models: Vec<String> = params
            .subagents
            .iter()
            .filter_map(|s| s.model.clone())
            .filter(|s| !s.is_empty())
            .collect();
        metadata_map.insert("subagent_eval".into(), serde_json::json!(true));
        metadata_map.insert(
            "subagent_count".into(),
            serde_json::json!(params.subagents.len()),
        );
        metadata_map.insert("subagent_roles".into(), serde_json::json!(roles));
        if !models.is_empty() {
            metadata_map.insert("subagent_models".into(), serde_json::json!(models));
        }
        metadata_map.insert("subagents".into(), safe_subagents.clone());
    }
    if !safe_feedback_rules.is_empty() {
        metadata_map.insert(
            "feedback_rules_applied".into(),
            serde_json::json!(safe_feedback_rules.clone()),
        );
    }
    if let Some(did) = &params.dispatch_id {
        metadata_map.insert("dispatch_id".into(), serde_json::json!(did));
    }
    if let Some(flow_id) = &params.flow_id {
        if !flow_id.is_empty() {
            metadata_map.insert("flow_id".into(), serde_json::json!(flow_id));
        }
    }
    if let Some(issue_ref) = &params.issue_ref {
        if !issue_ref.is_empty() {
            metadata_map.insert("issue_ref".into(), serde_json::json!(issue_ref));
        }
    }
    if let Some(pr_ref) = &params.pr_ref {
        if !pr_ref.is_empty() {
            metadata_map.insert("pr_ref".into(), serde_json::json!(pr_ref));
        }
    }
    if !safe_evidence_refs.is_empty() {
        metadata_map.insert(
            "evidence_refs".into(),
            serde_json::json!(safe_evidence_refs.clone()),
        );
    }
    if !safe_tests_run.is_empty() {
        metadata_map.insert(
            "tests_run".into(),
            serde_json::json!(safe_tests_run.clone()),
        );
    }
    if secret_redactions > 0 {
        metadata_map.insert(
            "secret_redactions".into(),
            serde_json::json!(secret_redactions),
        );
        metadata_map.insert(
            "secret_redaction_warning".into(),
            serde_json::json!("Potential secrets were redacted before eval persistence."),
        );
    }

    let mem_params = SaveMemoryParams {
        text,
        summary: format!("[{}] {} / {}", outcome_emoji, safe_agent, safe_task),
        path: path.clone(),
        importance: match outcome_norm.as_str() {
            "success" => 0.55,
            "failure" => 0.75,
            "partial" => 0.6,
            "aborted" => 0.5,
            _ => 0.5,
        },
        category: "eval".to_string(),
        topic: safe_task.clone(),
        keywords,
        persons: Vec::new(),
        entities,
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
        valid_from: None,
        valid_until: None,
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
        "kanban_update": "skipped (no dispatch_id)",
        "distill_trajectory": "skipped (no trajectory data)",
        "skill_evolve": "skipped",
        "post_complete_hooks": "pending",
    });
    let mut completion_warning: Option<String> = None;

    if let Some(ref trajectory) = safe_trajectory {
        if let Some(trace_arr) = trajectory.as_array() {
            if !trace_arr.is_empty() && outcome_norm == "success" {
                let server_clone = server.clone();
                let task_desc = safe_task.clone();
                let agent = safe_agent.clone();
                let trace = trace_arr.clone();
                let skills_used = safe_skills_used.clone();
                let skill_path = if skills_used.is_empty() {
                    format!("/skills/auto/{}", task_id)
                } else {
                    skills_used[0].clone()
                };
                pipeline_status["distill_trajectory"] = json!("enqueued");
                pipeline_status["skill_evolve"] = json!("will follow distill if successful");
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
        // Explicit tachi_complete represents a deliberate close — mark the
        // kanban row reviewed so the status dashboard stops flagging it as
        // an auto-closed, unreviewed dispatch. Watchdog auto-close keeps
        // reviewed=false.
        match crate::dispatch_ops::update_kanban_state(
            server,
            did,
            new_state,
            Some(&eval_memory_id),
            Some(true),
        )
        .await
        {
            Ok(()) => match read_kanban_snapshot(server, did) {
                Ok(Some(snapshot))
                    if snapshot.state.as_deref() == Some(new_state)
                        && snapshot.eval_ledger_id.as_deref() == Some(eval_memory_id.as_str())
                        && snapshot.reviewed == Some(true) =>
                {
                    pipeline_status["kanban_update"] = json!({
                        "status": "updated",
                        "dispatch_id": did,
                        "scope": snapshot.scope,
                        "state": new_state,
                        "eval_memory_id": eval_memory_id,
                        "reviewed": true,
                    });
                }
                Ok(Some(snapshot)) => {
                    let warning = format!(
                            "kanban update verification failed after eval persistence for dispatch_id={did}, task_id={}, task={}: expected state={new_state}, eval_memory_id={}, reviewed=true but found scope={}, state={:?}, eval_memory_id={:?}, reviewed={:?}",
                            task_id,
                            safe_task,
                            eval_memory_id,
                            snapshot.scope,
                            snapshot.state,
                            snapshot.eval_ledger_id,
                            snapshot.reviewed
                        );
                    eprintln!("[tachi_complete] {warning}");
                    completion_warning = Some(warning.clone());
                    pipeline_status["kanban_update"] = json!({
                        "status": "stale",
                        "dispatch_id": did,
                        "scope": snapshot.scope,
                        "state": snapshot.state,
                        "eval_memory_id": snapshot.eval_ledger_id,
                        "reviewed": snapshot.reviewed,
                        "expected_state": new_state,
                        "expected_eval_memory_id": eval_memory_id,
                    });
                }
                Ok(None) => {
                    let warning = format!(
                            "kanban card missing after eval persistence for dispatch_id={did}, task_id={}, task={}",
                            task_id, safe_task
                        );
                    eprintln!("[tachi_complete] {warning}");
                    completion_warning = Some(warning.clone());
                    pipeline_status["kanban_update"] = json!({
                        "status": "missing",
                        "dispatch_id": did,
                        "expected_state": new_state,
                        "expected_eval_memory_id": eval_memory_id,
                        "reviewed": true,
                    });
                }
                Err(error) => {
                    let warning = format!(
                            "kanban update readback failed after eval persistence for dispatch_id={did}, task_id={}, task={}: {error}",
                            task_id, safe_task
                        );
                    eprintln!("[tachi_complete] {warning}");
                    completion_warning = Some(warning.clone());
                    pipeline_status["kanban_update"] = json!({
                        "status": "readback_failed",
                        "dispatch_id": did,
                        "expected_state": new_state,
                        "expected_eval_memory_id": eval_memory_id,
                        "reviewed": true,
                        "error": error,
                    });
                }
            },
            Err(error) => {
                let warning = format!(
                    "kanban update failed after eval persistence for dispatch_id={did}, task_id={}, task={}: {error}",
                    task_id, safe_task
                );
                eprintln!("[tachi_complete] {warning}");
                completion_warning = Some(warning.clone());
                pipeline_status["kanban_update"] = json!({
                    "status": "failed",
                    "dispatch_id": did,
                    "state": new_state,
                    "eval_memory_id": eval_memory_id,
                    "reviewed": true,
                    "error": error,
                });
            }
        }
    }

    let dispatch_completion_link = match (
        params.flow_id.as_deref().filter(|flow| !flow.is_empty()),
        params
            .dispatch_id
            .as_deref()
            .filter(|dispatch_id| !dispatch_id.is_empty()),
    ) {
        (Some(flow_id), Some(dispatch_id)) => {
            let completion_payload = json!({
                "task_id": task_id.clone(),
                "task": safe_task.clone(),
                "agent": safe_agent.clone(),
                "outcome": outcome_norm.clone(),
                "profile": params.profile.clone(),
                "risk": params.risk.clone(),
                "eval_memory_id": eval_memory_id.clone(),
                "eval_path": path.clone(),
                "verification_present": verification_present,
                "diff_present": diff_present,
                "evidence_refs": safe_evidence_refs.clone(),
                "tests_run": safe_tests_run.clone(),
                "subagent_count": params.subagents.len(),
                "feedback_rules_applied": safe_feedback_rules.clone(),
                "skills_used": safe_skills_used.clone(),
                "issue_ref": params.issue_ref.clone(),
                "pr_ref": params.pr_ref.clone(),
                "duration_ms": params.duration_ms,
                "cost_tokens": params.cost_tokens,
                "cost_usd": params.cost_usd,
                "quality_score": params.quality_score,
            });
            match crate::task_lifecycle::mark_task_dispatch_completion(
                flow_id,
                dispatch_id,
                completion_payload,
            ) {
                Ok(value) => value,
                Err(error) => json!({
                    "recorded": false,
                    "flow_id": flow_id,
                    "dispatch_id": dispatch_id,
                    "error": error,
                }),
            }
        }
        _ => json!({
            "recorded": false,
            "reason": "missing flow_id or dispatch_id",
        }),
    };
    pipeline_status["dispatch_completion_link"] = dispatch_completion_link;

    pipeline_status["post_complete_hooks"] = run_lesson_post_complete_hook(
        server,
        &params,
        &outcome_norm,
        safe_notes.as_deref(),
        &safe_task,
        &safe_agent,
        &safe_skills_used,
        &date,
        &task_id,
    )
    .await;

    let mut next_steps =
        vec!["Use tachi_search with 'eval' keyword to find related outcomes.".to_string()];
    if params
        .worktree
        .as_deref()
        .is_some_and(|worktree| !worktree.trim().is_empty())
    {
        next_steps.push("For worktree-based dispatch, run approve_merge when ready.".to_string());
    } else {
        next_steps.push(
            "No worktree was recorded; no approve_merge step is implied by this completion."
                .to_string(),
        );
    }

    let mut review_bundle = serde_json::json!({
        "recorded": true,
        "task_id": task_id,
        "task": safe_task,
        "agent": safe_agent,
        "path": path,
        "outcome": outcome_norm,
        "dispatch_id": params.dispatch_id,
        "profile": params.profile,
        "risk": params.risk,
        "quality_score": params.quality_score,
        "flow_id": params.flow_id,
        "issue_ref": params.issue_ref,
        "pr_ref": params.pr_ref,
        "evidence_refs": safe_evidence_refs,
        "tests_run": safe_tests_run,
        "diff_present": diff_present,
        "subagent_count": params.subagents.len(),
        "subagents": safe_subagents,
        "eval_entry": save_json,
        "next_steps": next_steps,
        "pipeline": pipeline_status,
        "secret_redactions": secret_redactions,
    });
    if let (Some(warning), Some(obj)) = (completion_warning, review_bundle.as_object_mut()) {
        crate::mcp_proxy::append_warning(obj, warning);
    }

    serde_json::to_string(&review_bundle)
        .map_err(|e| format!("Failed to serialize review bundle: {}", e))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn read_kanban_snapshot_surfaces_project_read_failures() {
        let dir = tempfile::tempdir().expect("temp dir");
        let global_db = dir.path().join("global").join("memory.db");
        let project_db = dir.path().join("project").join("memory.db");
        std::fs::create_dir_all(global_db.parent().expect("global parent"))
            .expect("create global parent");
        std::fs::create_dir_all(project_db.parent().expect("project parent"))
            .expect("create project parent");
        let server =
            MemoryServer::new(global_db, Some(project_db)).expect("server with project db");

        server
            .with_project_store(|store| {
                store
                    .connection()
                    .execute("DROP TABLE memories", [])
                    .map_err(|e| format!("drop memories: {e}"))?;
                Ok(())
            })
            .expect("break project memories table");

        let err = read_kanban_snapshot(&server, "dispatch-readback-failure")
            .expect_err("project read failure should not be treated as a missing card");
        assert!(err.contains("kanban list_by_path"), "{err}");
    }
}
