use chrono::Utc;
use serde_json::json;

use crate::memory_search_ops::handle_save_memory;
use crate::tool_params::{SaveMemoryParams, TachiCompleteParams};
use crate::MemoryServer;

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

    let dedup_hit = match server.with_store_for_scope(lesson_db, |store| {
        store
            .record_lesson_dedup_seen(
                safe_task,
                outcome_norm,
                safe_skills_used,
                &Utc::now().to_rfc3339(),
                30,
            )
            .map_err(|e| format!("lesson dedup update: {e}"))
    }) {
        Ok(hit) => hit,
        Err(e) => {
            eprintln!("[tachi_complete/post_hook] lesson dedup update failed: {e}");
            return json!(format!("lesson_dedup_failed: {e}"));
        }
    };

    if let Some(update) = dedup_hit {
        return json!(format!(
            "lesson_deduped (id={}, count={})",
            update.id, update.count
        ));
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
