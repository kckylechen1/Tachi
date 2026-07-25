use super::enrichment::enqueue_save_enrichment;
use super::entry::build_save_entry;
use super::persist::{
    find_exact_path_text_duplicate, lookup_existing_entry, spawn_save_contradiction_detection,
    upsert_idless_save_entry, upsert_save_entry, AtomicEvidenceWrite,
};
use super::response::{build_duplicate_save_response, build_save_response};
use super::validation::{validate_save_text, SaveTextValidation};
use super::write_affinity::{apply_write_affinity, AffinityNote};
use crate::memory_search_ops::auto_link::{is_training_seed, spawn_auto_linking};
use crate::memory_search_ops::text_scrub::{scrub_secrets, scrub_think_tags};
use crate::tool_params::{SaveMemoryParams, WikiEvidenceRefV1};
use crate::{DbScope, MemoryServer};
use blake2::{Blake2s256, Digest};
use chrono::Utc;
use serde_json::json;

#[cfg(test)]
struct PreUpsertBarrier {
    entry_id: String,
    barrier: std::sync::Arc<std::sync::Barrier>,
}

#[cfg(test)]
static PRE_UPSERT_BARRIER: std::sync::OnceLock<std::sync::Mutex<Option<PreUpsertBarrier>>> =
    std::sync::OnceLock::new();

#[cfg(test)]
pub(crate) struct PreUpsertBarrierGuard;

#[cfg(test)]
pub(crate) fn install_pre_upsert_barrier(
    entry_id: &str,
    barrier: std::sync::Arc<std::sync::Barrier>,
) -> PreUpsertBarrierGuard {
    let slot = PRE_UPSERT_BARRIER.get_or_init(|| std::sync::Mutex::new(None));
    *slot.lock().unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(PreUpsertBarrier {
        entry_id: entry_id.to_string(),
        barrier,
    });
    PreUpsertBarrierGuard
}

#[cfg(test)]
impl Drop for PreUpsertBarrierGuard {
    fn drop(&mut self) {
        if let Some(slot) = PRE_UPSERT_BARRIER.get() {
            *slot.lock().unwrap_or_else(|poisoned| poisoned.into_inner()) = None;
        }
    }
}

#[cfg(test)]
fn wait_at_pre_upsert_barrier(entry_id: &str) {
    let barrier = PRE_UPSERT_BARRIER.get().and_then(|slot| {
        slot.lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .as_ref()
            .filter(|configured| configured.entry_id == entry_id)
            .map(|configured| std::sync::Arc::clone(&configured.barrier))
    });
    if let Some(barrier) = barrier {
        barrier.wait();
    }
}

fn atomic_evidence_metadata_patch(
    existing: Option<&serde_json::Value>,
    final_metadata: &serde_json::Value,
    explicit_keys: &[String],
) -> serde_json::Map<String, serde_json::Value> {
    let Some(final_object) = final_metadata.as_object() else {
        return serde_json::Map::new();
    };
    let existing_object = existing.and_then(serde_json::Value::as_object);
    let mut patch = serde_json::Map::new();
    for key in explicit_keys {
        if !matches!(key.as_str(), "evidence_refs_v1" | "source_refs") {
            if let Some(value) = final_object.get(key) {
                patch.insert(key.clone(), value.clone());
            }
        }
    }
    for (key, value) in final_object {
        if matches!(key.as_str(), "evidence_refs_v1" | "source_refs") {
            continue;
        }
        if existing_object.and_then(|object| object.get(key)) != Some(value) {
            patch.insert(key.clone(), value.clone());
        }
    }
    patch
}

fn idless_save_identity(path: &str, text: &str) -> String {
    let path = memcore::path_router::normalize_path(path);
    let mut hasher = Blake2s256::new();
    hasher.update(b"tachi:idless-memory:v1\0");
    hasher.update((path.len() as u64).to_be_bytes());
    hasher.update(path.as_bytes());
    hasher.update((text.len() as u64).to_be_bytes());
    hasher.update(text.as_bytes());
    hasher
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

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
    params: SaveMemoryParams,
) -> Result<String, String> {
    handle_save_memory_impl(server, params, None).await
}

pub(crate) async fn handle_save_memory_with_evidence_refs(
    server: &MemoryServer,
    params: SaveMemoryParams,
    evidence_refs: Vec<WikiEvidenceRefV1>,
) -> Result<String, String> {
    handle_save_memory_impl(server, params, Some(evidence_refs)).await
}

async fn handle_save_memory_impl(
    server: &MemoryServer,
    mut params: SaveMemoryParams,
    evidence_refs: Option<Vec<WikiEvidenceRefV1>>,
) -> Result<String, String> {
    params.text = scrub_think_tags(&params.text);
    params.summary = scrub_think_tags(&params.summary);
    let (safe_text, secret_redactions) = scrub_secrets(&params.text);
    let gate_warnings = match validate_save_text(&params, &safe_text)? {
        SaveTextValidation::Accepted(warnings) => warnings,
        SaveTextValidation::Rejected(body) => return Ok(body),
    };
    let requested_id = params.id.clone();
    let idless_identity = requested_id
        .is_none()
        .then(|| idless_save_identity(&params.path, &safe_text));
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
    //
    // #1176: the actionable half of that warning depends on whether THIS
    // session is bound to a project (`session_project()` — the same signal
    // `reject_unbound_cross_project_write` gates the -32602 on). Suggesting
    // `project=<name>` to an unbound session is a circular instruction: the
    // very next call with that param gets hard-rejected.
    let session_bound = server.session_project().is_some();
    let scope_warning = warning.as_ref().map(|_| {
        crate::memory_search_ops::scope_downgrade_warning(
            &requested_scope,
            target_db.as_str(),
            session_bound,
        )
    });

    // #1041 F2: resolve whether a caller-supplied `id` already exists at the
    // PRE-gate target (see `write_affinity` module doc's F2 note — `id` is
    // an update key, not placement authority, so an id that doesn't resolve
    // here is really a new row and gets the same domain-routing scrutiny as
    // an id-less save). When the gate later determines this genuinely IS a
    // patch (skips unchanged), this same lookup is reused below instead of
    // querying twice.
    let pre_gate_existing_entry = lookup_existing_entry(
        server,
        &id,
        requested_id.is_some(),
        target_db,
        named_project.as_deref(),
    )?;
    let id_resolves_at_target = pre_gate_existing_entry.is_some();

    // #1041 S1: domain-store write affinity gate. Only acts on the ambiguous
    // default path (no CALLER-explicit project=, no id resolving to an
    // existing row at the target, resolved to the bound project store) —
    // reroutes to the domain's registered store when one is mounted,
    // refuses loudly when it isn't, or passes through unchanged when the
    // domain has no registered route at all (uncertain, fail-safe
    // permissive). See `write_affinity` module docs.
    // #1041 B2: capture the PRE-gate target before `apply_write_affinity`
    // shadows `target_db`/`named_project` with the (possibly rerouted)
    // POST-gate values, so the duplicate-lookup avoidance below can tell
    // whether the gate actually changed the target or just passed through.
    let pre_gate_target_db = target_db;
    let pre_gate_named_project = named_project.clone();
    let affinity = apply_write_affinity(
        server,
        &params,
        target_db,
        named_project.as_deref(),
        id_resolves_at_target,
    )?;
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
    //
    // This lookup preserves the existing fast duplicate response. It is not
    // the race boundary: the v19 id-less identity constraint below is the
    // authoritative single-winner decision when concurrent callers both miss
    // this read.
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
    // #1041 F2/B2: when the id resolved at the PRE-gate target, the gate
    // necessarily passed through unchanged (that's exactly the skip
    // condition) — target_db/named_project are identical to what
    // `pre_gate_existing_entry` was already looked up against, so reuse it
    // instead of querying the same store twice. When it did NOT resolve
    // there, only re-query if the gate actually changed the target (a real
    // reroute, which is now never mixed with a caller-supplied id — see
    // `write_affinity`'s B2 refusal): a passthrough (same store, same
    // project) already got its answer from the pre-gate lookup (`None`,
    // since `id_resolves_at_target` is false here), so re-querying the
    // identical store for the identical id would just be a wasted duplicate
    // read.
    let existing_entry = if id_resolves_at_target {
        pre_gate_existing_entry
    } else if target_db == pre_gate_target_db && named_project == pre_gate_named_project {
        None
    } else {
        lookup_existing_entry(
            server,
            &id,
            requested_id.is_some(),
            target_db,
            named_project.as_deref(),
        )?
    };
    let enrichment_revision = existing_entry
        .as_ref()
        .map(|entry| entry.revision)
        .unwrap_or(0)
        + 1;

    let needs_summary = params.summary.is_empty();
    let needs_embedding = params.vector.is_none();
    let auto_link = params.auto_link;
    let emit_continuity = params.emit_continuity;
    let explicit_metadata_keys = evidence_refs
        .as_ref()
        .and_then(|_| params.metadata.as_ref())
        .and_then(serde_json::Value::as_object)
        .map(|object| object.keys().cloned().collect::<Vec<_>>())
        .unwrap_or_default();
    let mut entry = build_save_entry(
        server,
        params,
        safe_text,
        id.clone(),
        timestamp.clone(),
        valid_from,
        target_db,
        existing_entry.as_ref(),
    );

    // #1041 F3/C5: `build_save_entry` -> `inject_provenance` resolves
    // `provenance.db_path` from `target_db` alone, which only ever knows
    // this daemon's OWN bound project path — it has no visibility into ANY
    // named-project target, whether that name came from a write-affinity
    // reroute, a caller's own explicit `project=`, or simply passed through
    // unchanged. So this runs for every named-project save, not only a
    // rerouted one (see `correct_provenance_db_path_for_named_project`'s doc
    // for why narrowing this to "only reroutes" would resurrect an older,
    // broader pre-#1041 bug instead of closing it) — the invariant is
    // `provenance.db_path` always matches where the row actually landed.
    if let Some(project_name) = named_project.as_deref() {
        if let Ok(named_project_path) = MemoryServer::resolve_named_project_db_path(project_name) {
            entry.metadata = crate::provenance::correct_provenance_db_path_for_named_project(
                entry.metadata,
                &named_project_path,
            );
        }
    }

    let evidence_write = evidence_refs.map(|references| AtomicEvidenceWrite {
        metadata_patch: atomic_evidence_metadata_patch(
            existing_entry.as_ref().map(|existing| &existing.metadata),
            &entry.metadata,
            &explicit_metadata_keys,
        ),
        append_refs: references
            .into_iter()
            .map(|reference| serde_json::json!(reference))
            .collect(),
    });

    #[cfg(test)]
    wait_at_pre_upsert_barrier(&entry.id);

    if let Some(identity) = idless_identity.as_deref() {
        match upsert_idless_save_entry(
            server,
            &mut entry,
            identity,
            target_db,
            named_project.as_deref(),
            evidence_write.as_ref(),
        )? {
            memcore::db::IdlessUpsertResult::Saved => {}
            memcore::db::IdlessUpsertResult::Duplicate { id } => {
                let response = build_duplicate_save_response(&id, &entry.path, target_db);
                return serde_json::to_string(&serde_json::Value::Object(response))
                    .map_err(|error| format!("Failed to serialize response: {error}"));
            }
        }
    } else {
        upsert_save_entry(
            server,
            &mut entry,
            target_db,
            named_project.as_deref(),
            evidence_write.as_ref(),
        )?;
    }

    // #1435 slice 3 / #2059: write-side recall-cache bust, shared with the
    // enrichment-flush and contradiction-supersede paths (see
    // `search_memory::cache::invalidate_recall_cache_after_write`'s doc for
    // the epoch guard + cross-process boundary). Both dedupe short-circuits
    // above (`find_exact_path_text_duplicate` and
    // `IdlessUpsertResult::Duplicate`) already returned before this point,
    // so an exact-duplicate no-write correctly never invalidates.
    let recall_fence =
        crate::memory_search_ops::invalidate_recall_cache_after_write(server, "save");

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
        named_project.as_deref(),
        enrichment_enqueued,
        needs_embedding,
        needs_summary,
        warning,
        gate_warnings,
        secret_redactions,
        &requested_scope,
        scope_warning,
        recall_fence,
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
