use super::enrichment::enqueue_save_enrichment;
use super::entry::build_save_entry;
use super::persist::{
    lookup_existing_revision, spawn_save_contradiction_detection, upsert_save_entry,
};
use super::response::build_save_response;
use super::validation::{validate_save_text, SaveTextValidation};
use crate::memory_search_ops::auto_link::{is_training_seed, spawn_auto_linking};
use crate::memory_search_ops::text_scrub::{scrub_secrets, scrub_think_tags};
use crate::tool_params::SaveMemoryParams;
use crate::{DbScope, MemoryServer};
use chrono::Utc;
use serde_json::json;

pub(crate) async fn handle_save_memory(
    server: &MemoryServer,
    mut params: SaveMemoryParams,
) -> Result<String, String> {
    params.text = scrub_think_tags(&params.text);
    params.summary = scrub_think_tags(&params.summary);
    let (safe_text, secret_redactions) = scrub_secrets(&params.text);
    let gate_warnings = match validate_save_text(&params, &safe_text)? {
        SaveTextValidation::Accepted(warnings) => warnings,
        SaveTextValidation::Rejected(body) => return Ok(body),
    };
    let requested_id = params.id.clone();
    let id = requested_id
        .clone()
        .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
    let timestamp = params
        .timestamp
        .clone()
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| Utc::now().to_rfc3339());
    let valid_from = params
        .valid_from
        .clone()
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| timestamp.clone());
    let requested_scope = params.scope.clone();
    let named_project = params.project.clone();
    let (target_db, warning) = if named_project.is_some() {
        (DbScope::Project, None) // Will use named project below
    } else {
        server.resolve_write_scope(&requested_scope)
    };
    let existing_revision = lookup_existing_revision(
        server,
        &id,
        requested_id.is_some(),
        target_db,
        named_project.as_deref(),
    )?;
    let enrichment_revision = existing_revision.unwrap_or(0) + 1;

    let needs_summary = params.summary.is_empty();
    let needs_embedding = params.vector.is_none();
    let auto_link = params.auto_link;
    let emit_continuity = params.emit_continuity;
    let entry = build_save_entry(
        server,
        params,
        safe_text,
        id.clone(),
        timestamp.clone(),
        valid_from,
        target_db,
    );

    upsert_save_entry(server, &entry, target_db, named_project.as_deref())?;

    let continuity_event = if emit_continuity {
        Some(crate::continuity_ops::emit_memory_saved_event(
            server,
            &entry,
            target_db,
            named_project.as_deref(),
        ))
    } else {
        None
    };

    if !needs_embedding && entry.vector.is_some() {
        spawn_save_contradiction_detection(server, id, target_db, named_project.clone());
    }

    let enrichment_enqueued = enqueue_save_enrichment(
        server,
        &entry,
        needs_embedding,
        needs_summary,
        target_db,
        named_project.clone(),
        enrichment_revision,
    );
    let mut response = build_save_response(
        &entry,
        &timestamp,
        target_db,
        enrichment_enqueued,
        needs_embedding,
        needs_summary,
        warning,
        gate_warnings,
        secret_redactions,
    );
    if let Some(event) = continuity_event {
        response.insert("continuity_event".into(), event);
    }

    if auto_link && !entry.entities.is_empty() && !is_training_seed(&entry) {
        spawn_auto_linking(server, &entry, target_db, named_project);
        response.insert("auto_link".into(), json!("pending"));
    } else if auto_link && !entry.entities.is_empty() && is_training_seed(&entry) {
        response.insert("auto_link".into(), json!("skipped_training_seed"));
    }

    serde_json::to_string(&serde_json::Value::Object(response))
        .map_err(|e| format!("Failed to serialize response: {}", e))
}
