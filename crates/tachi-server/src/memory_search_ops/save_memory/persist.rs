use super::error::format_save_error;
use crate::memory_search_ops::contradiction::apply_auto_contradiction_detection;
use crate::{DbScope, MemoryServer};
use memcore::{db::IdlessUpsertResult, MemoryEntry, MemoryStore};

pub(super) struct WikiProjectionWriteResult {
    pub upsert: IdlessUpsertResult,
    pub duplicates_superseded: usize,
}

pub(super) struct AtomicReferenceWrite {
    pub metadata_patch: serde_json::Map<String, serde_json::Value>,
    pub metadata_removals: Vec<&'static str>,
    pub mutations: Vec<memcore::db::ValidatedReferenceMutation>,
}

/// Return the id of an active row with the same normalized path and exact
/// text. #1041 F6: this used to fetch `list_by_path(path, 64, false)` (exact
/// path + descendants, capped at 64 rows) and filter for an exact path+text
/// match in memory — once 64+ rows already existed under that path's
/// descendant family, a genuinely duplicate row could sort past the cutoff
/// and never reach the filter. `find_exact_path_text_id` pushes the exact
/// match into SQL instead, so no window size can hide an existing duplicate.
pub(in crate::memory_search_ops::save_memory) fn find_exact_path_text_duplicate(
    server: &MemoryServer,
    path: &str,
    text: &str,
    target_db: DbScope,
    named_project: Option<&str>,
) -> Result<Option<String>, String> {
    let normalized_path = memcore::path_router::normalize_path(path);
    let lookup = |store: &mut MemoryStore| {
        store
            .find_exact_path_text_id(&normalized_path, text)
            .map_err(|e| format_save_error(server, target_db, named_project, &e))
    };
    if let Some(project_name) = named_project {
        server.with_named_project_store_read(project_name, lookup)
    } else {
        server.with_store_for_scope_read(target_db, lookup)
    }
}

pub(in crate::memory_search_ops::save_memory) fn lookup_existing_entry(
    server: &MemoryServer,
    id: &str,
    requested_id: bool,
    target_db: DbScope,
    named_project: Option<&str>,
) -> Result<Option<MemoryEntry>, String> {
    if !requested_id {
        return Ok(None);
    }

    let lookup = |store: &mut MemoryStore| {
        store
            .get(id)
            .map_err(|e| format_save_error(server, target_db, named_project, &e))
    };
    if let Some(project_name) = named_project {
        server.with_named_project_store_read(project_name, lookup)
    } else {
        server.with_store_for_scope_read(target_db, lookup)
    }
}

/// Mark `id` as **used** — tachi#1446 signal D's only production call site.
///
/// # What counts as "a save references a memory id"
///
/// Exactly one thing, on today's code: **a caller-initiated save whose `id`
/// resolved to an existing memory row**. The caller had to already hold that
/// id and chose to write to that specific memory; that is a caller consuming
/// a memory, not the system touching its own row.
///
/// The alternatives were checked and rejected against what the save path
/// actually supports:
/// * `TachiSaveParams::references` / the `evidence_refs_v1` + `source_refs`
///   channels carry **external** targets only — `wiki_ops::references`'
///   `validate_reference_format` accepts a URL, an absolute/`docs/` path or a
///   GitHub shorthand, and rejects a bare memory id, so no memory id can
///   arrive that way.
/// * Edge writes (`memcore::db::add_edge`) that name two memory ids are
///   produced by `auto_link` / contradiction detection / foundry distillation
///   — the system linking its own rows, which is the defect under repair, not
///   evidence of use.
/// * Free text is deliberately never scanned for id-shaped substrings.
///
/// # Why the initiator flag, not just "an id was supplied"
///
/// `handle_save_memory` is reached both from the MCP tool wrapper and from
/// in-process writers that pass a deterministic id (`dispatch_ops::
/// kanban_helpers` re-saving a kanban row, `dispatch_ops::dispatch::execution`
/// re-saving an eval by `eval_id`). Those are the system rewriting its own
/// bookkeeping. [`super::handler::SaveInitiator`] separates them, and its
/// default is `System` so a new call site under-records rather than
/// re-manufacturing the exposure loop.
///
/// Failure is non-fatal by design: the row is already saved, and losing a
/// provenance mark must not turn a successful save into an error response.
/// The failure is returned so the caller can decide (today: a warning on
/// stderr). That is also why this write is not wrapped in
/// `db::retry_memory_locked` the way `MemoryStore::upsert` is — under lock
/// contention the honest outcome is one dropped use event, not a retry loop
/// held open after the save the caller was waiting for already committed.
pub(in crate::memory_search_ops::save_memory) fn mark_save_target_used(
    server: &MemoryServer,
    id: &str,
    target_db: DbScope,
    named_project: Option<&str>,
) -> Result<usize, String> {
    let now = chrono::Utc::now().to_rfc3339();
    let ids = [id.to_string()];
    let mark = |store: &mut MemoryStore| {
        memcore::db::record_memory_use(store.connection(), &ids, &now)
            .map_err(|e| format_save_error(server, target_db, named_project, &e))
    };
    if let Some(project_name) = named_project {
        server.with_named_project_store(project_name, mark)
    } else {
        server.with_store_for_scope(target_db, mark)
    }
}

pub(in crate::memory_search_ops::save_memory) fn upsert_save_entry(
    server: &MemoryServer,
    entry: &mut MemoryEntry,
    target_db: DbScope,
    named_project: Option<&str>,
    evidence_write: &AtomicReferenceWrite,
) -> Result<(), String> {
    let mut persist = |store: &mut MemoryStore, project_name: Option<&str>| {
        let (_, metadata) = store
            .upsert_with_validated_reference_mutations_and_metadata_removals(
                entry,
                None,
                &evidence_write.metadata_patch,
                &evidence_write.metadata_removals,
                &evidence_write.mutations,
            )
            .map_err(|error| format_save_error(server, target_db, project_name, &error))?;
        entry.metadata = metadata;
        Ok(())
    };
    if let Some(project_name) = named_project {
        server.with_named_project_store(project_name, |store| persist(store, Some(project_name)))
    } else {
        server.with_store_for_scope(target_db, |store| persist(store, None))
    }
}

pub(in crate::memory_search_ops::save_memory) fn upsert_idless_save_entry(
    server: &MemoryServer,
    entry: &mut MemoryEntry,
    identity: &str,
    target_db: DbScope,
    named_project: Option<&str>,
    evidence_write: &AtomicReferenceWrite,
) -> Result<IdlessUpsertResult, String> {
    let mut persist = |store: &mut MemoryStore, project_name: Option<&str>| {
        let (result, metadata) = store
            .upsert_with_validated_reference_mutations_and_metadata_removals(
                entry,
                Some(identity),
                &evidence_write.metadata_patch,
                &evidence_write.metadata_removals,
                &evidence_write.mutations,
            )
            .map_err(|error| format_save_error(server, target_db, project_name, &error))?;
        entry.metadata = metadata;
        Ok(result)
    };
    if let Some(project_name) = named_project {
        server.with_named_project_store(project_name, |store| persist(store, Some(project_name)))
    } else {
        server.with_store_for_scope(target_db, |store| persist(store, None))
    }
}

/// Persist the canonical Wiki/Guide row, every duplicate supersession claim,
/// and every corresponding graph edge under one `BEGIN IMMEDIATE` writer
/// snapshot. A failed scan, claim, or edge write rolls the canonical upsert
/// back with the rest of the projection.
pub(in crate::memory_search_ops::save_memory) fn upsert_wiki_projection_entry(
    server: &MemoryServer,
    entry: &mut MemoryEntry,
    idless_identity: Option<&str>,
    target_db: DbScope,
    named_project: Option<&str>,
    evidence_write: &AtomicReferenceWrite,
) -> Result<WikiProjectionWriteResult, String> {
    let mut persist = |store: &mut MemoryStore, project_name: Option<&str>| {
        let (result, metadata, duplicates_superseded) = store
            .with_immutable_supersession_transaction(|projection| {
                if idless_identity.is_none() {
                    let active = projection
                        .find_active_wiki_entry_by_path_or_topic(&entry.path, &entry.topic)?;
                    if active.as_ref().map(|winner| winner.id.as_str()) != Some(entry.id.as_str()) {
                        return Err(memcore::MemoryError::InvalidArg(format!(
                            "wiki projection canonical changed before commit: expected {}",
                            entry.id
                        )));
                    }
                }
                let (result, metadata) = projection
                    .upsert_with_validated_reference_mutations_and_metadata_removals(
                        entry,
                        idless_identity,
                        &evidence_write.metadata_patch,
                        &evidence_write.metadata_removals,
                        &evidence_write.mutations,
                        false,
                    )?;
                if matches!(result, IdlessUpsertResult::Duplicate { .. }) {
                    return Ok((result, metadata, 0));
                }

                // Ordinary Wiki projection may retire only other ordinary Wiki
                // rows. Guide writes share this persistence seam for atomic
                // updates, but they are a separate corpus and must not claim a
                // Wiki row as their supersession predecessor.
                let candidates = if entry.path == "/wiki" || entry.path.starts_with("/wiki/") {
                    let parent_path = crate::copilot_ops::wiki_parent_path(&entry.path);
                    projection.list_all_wiki_duplicate_candidates(
                        &entry.path,
                        &entry.topic,
                        &parent_path,
                    )?
                } else {
                    Vec::new()
                };
                let created_at = chrono::Utc::now().to_rfc3339();
                let mut changed = 0usize;
                for candidate in candidates {
                    if candidate.id == entry.id
                        || !crate::copilot_ops::is_wiki_projection_duplicate(
                            &candidate,
                            &entry.path,
                            &entry.topic,
                            &entry.text,
                        )
                    {
                        continue;
                    }
                    projection.claim_immutable_supersession(&candidate.id, &entry.id)?;
                    projection.add_edge(&crate::copilot_ops::wiki_projection_supersedes_edge(
                        &entry.id,
                        &candidate.id,
                        &entry.path,
                        &entry.topic,
                        &created_at,
                    ))?;
                    changed += 1;
                }
                Ok((result, metadata, changed))
            })
            .map_err(|error| format_save_error(server, target_db, project_name, &error))?;
        entry.metadata = metadata;
        Ok(WikiProjectionWriteResult {
            upsert: result,
            duplicates_superseded,
        })
    };
    if let Some(project_name) = named_project {
        server.with_named_project_store(project_name, |store| persist(store, Some(project_name)))
    } else {
        server.with_store_for_scope(target_db, |store| persist(store, None))
    }
}

pub(in crate::memory_search_ops::save_memory) fn spawn_save_contradiction_detection(
    server: &MemoryServer,
    entry_id: String,
    target_db: DbScope,
    named_project: Option<String>,
) {
    let contradiction_server = server.clone();
    tokio::spawn(async move {
        if let Err(err) = apply_auto_contradiction_detection(
            &contradiction_server,
            &entry_id,
            target_db,
            named_project.as_deref(),
            None,
        )
        .await
        {
            eprintln!("[save_memory] auto contradiction detection failed for {entry_id}: {err}");
        }
    });
}
