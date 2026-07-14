use super::enrichment::enqueue_save_enrichment;
use super::entry::build_save_entry;
use super::persist::{
    find_exact_path_text_duplicate, lookup_existing_entry, spawn_save_contradiction_detection,
    upsert_save_entry,
};
use super::response::{build_duplicate_save_response, build_save_response};
use super::validation::{validate_save_text, SaveTextValidation};
use super::write_affinity::{apply_write_affinity, AffinityNote};
use crate::memory_search_ops::auto_link::{is_training_seed, spawn_auto_linking};
use crate::memory_search_ops::text_scrub::{scrub_secrets, scrub_think_tags};
use crate::tool_params::SaveMemoryParams;
use crate::{DbScope, MemoryServer};
use chrono::Utc;
use serde_json::json;

/// Render a #1041 S1 write-affinity note into the compact JSON shape
/// surfaced on the save response (`domain_affinity`), never blocking the
/// caller — a hard mismatch with no eligible store is a loud `Err` from
/// `apply_write_affinity` before a response is ever built, not a note.
fn domain_affinity_note_json(note: &AffinityNote) -> serde_json::Value {
    match note {
        AffinityNote::Unregistered { domain } => json!({
            "status": "unregistered",
            "domain": domain,
        }),
        AffinityNote::Rerouted { domain, project } => json!({
            "status": "rerouted",
            "domain": domain,
            "project": project,
        }),
    }
}

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
    // #925: `resolve_write_scope` already detects a silent scope downgrade
    // (requested != "global" but no project DB, so it falls back to global)
    // via `warning`, but that generic `warning` string gets stripped out of
    // the compact save/checkpoint receipt (`save_receipt_value`). Surface a
    // dedicated, stable `scope`/`scope_warning` pair so the fallback stays
    // loud through every response shape.
    let scope_warning = warning.as_ref().map(|_| {
        crate::memory_search_ops::scope_downgrade_warning(&requested_scope, target_db.as_str())
    });

    // #1041 S1: domain-store write affinity gate. Only acts on the ambiguous
    // default path (no explicit project=, no client id, resolved to the
    // bound project store) — reroutes to the domain's registered store when
    // one is mounted, refuses loudly when it isn't, or passes through
    // unchanged when the domain has no registered route at all (uncertain,
    // fail-safe permissive). See `write_affinity` module docs.
    let affinity = apply_write_affinity(server, &params, target_db, named_project.as_deref())?;
    let target_db = affinity.target_db;
    let named_project = affinity.named_project;
    let affinity_note = affinity.note;

    // #1041 S3: dedup must fire for every id-less save, `force` or not.
    // `force` bypasses the *content-quality* gates (noise filter / capture
    // gate) above — it was never meant to also waive "did I already save
    // this exact row", but the prior `!params.force &&` guard coupled the
    // two. That coupling is exactly the "sync pipe has no write-side
    // idempotency" defect: a periodic writer that passes `force=true` (to
    // get past the noise filter on short factual content) minted a fresh
    // random id on every retry because this check was skipped outright.
    // Path+text identity is unaffected by `force` from here on; a caller
    // that truly wants a second, distinct row can still pass its own `id`.
    if params.id.is_none() {
        if let Some(existing_id) = find_exact_path_text_duplicate(
            server,
            &params.path,
            &safe_text,
            target_db,
            named_project.as_deref(),
        )? {
            let response = build_duplicate_save_response(&existing_id, &params.path, target_db);
            return serde_json::to_string(&serde_json::Value::Object(response))
                .map_err(|e| format!("Failed to serialize response: {}", e));
        }
    }
    let existing_entry = lookup_existing_entry(
        server,
        &id,
        requested_id.is_some(),
        target_db,
        named_project.as_deref(),
    )?;
    let enrichment_revision = existing_entry
        .as_ref()
        .map(|entry| entry.revision)
        .unwrap_or(0)
        + 1;

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
        existing_entry.as_ref(),
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
        &requested_scope,
        scope_warning,
    );
    if let Some(event) = continuity_event {
        response.insert("continuity_event".into(), event);
    }

    if let Some(note) = affinity_note {
        response.insert("domain_affinity".into(), domain_affinity_note_json(&note));
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
