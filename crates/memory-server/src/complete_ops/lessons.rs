use chrono::Utc;
use serde_json::json;

use crate::memory_search_ops::handle_save_memory;
use crate::tool_params::{SaveMemoryParams, TachiCompleteParams};
use crate::MemoryServer;

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

pub(super) async fn run_lesson_post_complete_hook(
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
        emit_continuity: false,
    };
    match handle_save_memory(server, lesson_params).await {
        Ok(_) => json!("lesson_saved"),
        Err(e) => {
            eprintln!("[tachi_complete/post_hook] lesson save failed: {e}");
            json!(format!("lesson_save_failed: {e}"))
        }
    }
}
