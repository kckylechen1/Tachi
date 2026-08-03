use super::helpers::{dedup_strings, round3};
use crate::server_state::{DbScope, MemoryServer};
use chrono::Utc;
use memcore::{HybridWeights, MemoryEntry, MemoryStore, SearchOptions};
use serde_json::{json, Value};
use std::path::PathBuf;

fn merge_text(existing: &str, incoming: &str) -> String {
    let existing = existing.trim();
    let incoming = incoming.trim();
    if existing.is_empty() {
        return incoming.to_string();
    }
    if incoming.is_empty() || existing == incoming || existing.contains(incoming) {
        return existing.to_string();
    }
    if incoming.contains(existing) {
        return incoming.to_string();
    }
    format!("{existing}\n{incoming}")
}

fn merge_summary(existing: &str, incoming: &str, merged_text: &str) -> String {
    let existing = existing.trim();
    let incoming = incoming.trim();
    if existing.is_empty() && incoming.is_empty() {
        return merged_text.chars().take(100).collect();
    }
    if existing.is_empty() {
        return incoming.to_string();
    }
    if incoming.is_empty() || existing == incoming || existing.contains(incoming) {
        return existing.to_string();
    }
    if incoming.contains(existing) {
        return incoming.to_string();
    }
    format!("{existing}; {incoming}")
        .chars()
        .take(100)
        .collect()
}

fn merge_category(existing: &str, incoming: &str) -> String {
    for preferred in ["decision", "preference", "entity", "experience", "fact"] {
        if existing == preferred || incoming == preferred {
            return preferred.to_string();
        }
    }
    "other".to_string()
}

fn merge_metadata(
    existing: &serde_json::Value,
    incoming: &serde_json::Value,
    incoming_id: &str,
    similarity: f64,
) -> serde_json::Value {
    let mut merged = match existing {
        serde_json::Value::Object(map) => map.clone(),
        _ => serde_json::Map::new(),
    };

    let mut source_refs = Vec::<serde_json::Value>::new();
    for metadata in [existing, incoming] {
        if let Some(items) = metadata
            .get("source_refs")
            .and_then(|value| value.as_array())
        {
            for item in items {
                if !source_refs.contains(item) {
                    source_refs.push(item.clone());
                }
            }
        }
    }
    if !source_refs.is_empty() {
        merged.insert("source_refs".into(), serde_json::Value::Array(source_refs));
    }

    let mut merge_history = merged
        .get("merge_history")
        .and_then(|value| value.as_array().cloned())
        .unwrap_or_default();
    merge_history.push(json!({
        "merged_at": Utc::now().to_rfc3339(),
        "incoming_id": incoming_id,
        "similarity": round3(similarity),
        "strategy": "inline_foundry_merge",
    }));
    merged.insert(
        "merge_history".into(),
        serde_json::Value::Array(merge_history),
    );

    serde_json::Value::Object(merged)
}

pub(super) fn merge_capture_entries(
    existing: &MemoryEntry,
    incoming: &MemoryEntry,
    similarity: f64,
) -> MemoryEntry {
    let merged_text = merge_text(&existing.text, &incoming.text);
    let merged_summary = merge_summary(&existing.summary, &incoming.summary, &merged_text);
    let timestamp = if incoming.timestamp > existing.timestamp {
        incoming.timestamp.clone()
    } else {
        existing.timestamp.clone()
    };

    let mut path = if existing.path.trim().is_empty() {
        incoming.path.clone()
    } else {
        existing.path.clone()
    };
    let legacy_location = if incoming.location.trim().is_empty() {
        existing.location.as_str()
    } else {
        incoming.location.as_str()
    };
    let mut metadata = merge_metadata(
        &existing.metadata,
        &incoming.metadata,
        &incoming.id,
        similarity,
    );
    path = memcore::types::apply_location_relocation(&path, legacy_location, &mut metadata);

    MemoryEntry {
        id: existing.id.clone(),
        path,
        summary: merged_summary,
        text: merged_text,
        importance: existing.importance * 0.6 + incoming.importance * 0.4,
        timestamp,
        valid_from: if existing.valid_from.trim().is_empty() {
            incoming.valid_from.clone()
        } else {
            existing.valid_from.clone()
        },
        valid_until: existing
            .valid_until
            .clone()
            .or_else(|| incoming.valid_until.clone()),
        category: merge_category(&existing.category, &incoming.category),
        topic: if existing.topic.trim().is_empty() {
            incoming.topic.clone()
        } else {
            existing.topic.clone()
        },
        keywords: dedup_strings(
            existing
                .keywords
                .iter()
                .cloned()
                .chain(incoming.keywords.iter().cloned())
                .collect(),
        ),
        persons: Vec::new(),
        entities: dedup_strings(
            existing
                .entities
                .iter()
                .cloned()
                .chain(incoming.entities.iter().cloned())
                .chain(incoming.persons.iter().cloned())
                .chain(existing.persons.iter().cloned())
                .collect(),
        ),
        location: String::new(),
        source: "capture_session".to_string(),
        scope: if incoming.scope == "general" {
            existing.scope.clone()
        } else {
            incoming.scope.clone()
        },
        archived: false,
        access_count: existing.access_count,
        scored_count: existing.scored_count,
        last_access: existing.last_access.clone(),
        last_use_at: existing.last_use_at.clone(),
        revision: existing.revision + 1,
        metadata,
        vector: None,
        retention_policy: None,
        domain: None,
        recall_count: 0,
        query_diversity: 0,
        tier: "raw".to_string(),
    }
}

pub(super) fn capture_search_options(
    query_vec: Vec<f32>,
    path_prefix: Option<String>,
    vec_available: bool,
    top_k: usize,
) -> SearchOptions {
    SearchOptions {
        top_k: top_k.max(1),
        path_prefix,
        query_vec: Some(query_vec),
        include_archived: false,
        include_superseded: false,
        candidates_per_channel: 8,
        mmr_threshold: None,
        graph_expand_hops: 0,
        graph_relation_filter: None,
        as_of: None,
        precision_matchers: Vec::new(),
        vec_available,
        weights: HybridWeights {
            semantic: 1.0,
            fts: 0.0,
            symbolic: 0.0,
            decay: 0.0,
            use_rrf: false,
        },
        record_access: false,
        domain: None,
        surface: None,
        // Overwritten by `MemoryStore::search` from the store's own
        // identity (tachi#1569); stated here only because this literal
        // is exhaustive.
        wiki_corpus_store: false,
        recall_config: None,
        decay_policy: None,
    }
}

pub(super) fn search_similar_capture_entries(
    server: &MemoryServer,
    target_db: DbScope,
    named_project: Option<&str>,
    db_path: Option<&std::path::PathBuf>,
    path_prefix: &str,
    query_vec: &[f32],
    top_k: usize,
) -> Result<Vec<memcore::SearchResult>, String> {
    if query_vec.is_empty() {
        return Ok(Vec::new());
    }

    let search_action = |store: &mut MemoryStore| {
        let results = store
            .search(
                "",
                Some(capture_search_options(
                    query_vec.to_vec(),
                    Some(path_prefix.to_string()),
                    store.vec_available,
                    top_k,
                )),
            )
            .map_err(|e| format!("Failed to search similar capture entry: {e}"))?;
        Ok(results)
    };

    if let Some(project_name) = named_project {
        server.with_named_project_store_read(project_name, search_action)
    } else if let Some(db_path) = db_path {
        server.with_path_store_read(db_path, search_action)
    } else {
        server.with_store_for_scope_read(target_db, search_action)
    }
}

struct PreparedCaptureReferenceWrite {
    entry: MemoryEntry,
    metadata_patch: serde_json::Map<String, Value>,
    mutations: Vec<memcore::db::ValidatedReferenceMutation>,
}

fn prepare_capture_reference_write(
    entry: &MemoryEntry,
    context: &str,
) -> Result<PreparedCaptureReferenceWrite, String> {
    // Foundry capture metadata is assembled only by server handlers. Pull its
    // lineage out before persistence so reserved fields reach memcore solely
    // through the shape-validated append channel.
    let mut entry = entry.clone();
    let raw_refs = entry
        .metadata
        .as_object_mut()
        .and_then(|metadata| metadata.remove("source_refs"));
    let appends = match raw_refs {
        None => Vec::new(),
        Some(Value::Array(values)) => values
            .into_iter()
            .map(|value| {
                let object = value
                    .as_object()
                    .ok_or_else(|| format!("{context} capture source_ref must be an object"))?;
                let string_field =
                    |key: &str| object.get(key).and_then(Value::as_str).map(str::to_string);
                memcore::db::ValidatedReferenceMutation::capture_source(
                    string_field("ref_type").unwrap_or_default(),
                    string_field("ref_id").unwrap_or_default(),
                    string_field("revision"),
                )
                .map_err(|error| format!("{context} capture source_ref invalid: {error}"))
            })
            .collect::<Result<Vec<_>, _>>()?,
        Some(_) => return Err(format!("{context} capture source_refs must be an array")),
    };
    let metadata_patch = entry.metadata.as_object().cloned().unwrap_or_default();
    Ok(PreparedCaptureReferenceWrite {
        entry,
        metadata_patch,
        mutations: appends,
    })
}

pub(super) fn insert_capture_entry_if_absent(
    store: &mut MemoryStore,
    entry: &MemoryEntry,
    context: &str,
) -> Result<memcore::db::InsertMemoryResult, String> {
    let prepared = prepare_capture_reference_write(entry, context)?;
    store
        .insert_if_absent_with_validated_reference_mutations(
            &prepared.entry,
            &prepared.metadata_patch,
            &prepared.mutations,
        )
        .map_err(|error| format!("{context}: {error}"))
}

pub(super) fn persist_capture_entry(
    server: &MemoryServer,
    target_db: DbScope,
    named_project: Option<&str>,
    db_path: Option<&std::path::PathBuf>,
    entry: &MemoryEntry,
) -> Result<(), String> {
    fn persist(store: &mut MemoryStore, entry: &MemoryEntry, context: &str) -> Result<(), String> {
        let prepared = prepare_capture_reference_write(entry, context)?;
        store
            .upsert_with_validated_reference_mutations(
                &prepared.entry,
                None,
                &prepared.metadata_patch,
                &prepared.mutations,
            )
            .map_err(|error| format!("{context}: {error}"))?;
        Ok(())
    }

    if let Some(project_name) = named_project {
        let dest_path = server.resolve_server_named_project_db_path(project_name)?;
        let mut entry = entry.clone();
        entry.metadata = crate::provenance::restamp_provenance_for_destination(
            entry.metadata,
            &dest_path,
            DbScope::Project,
        );
        server.with_named_project_store(project_name, |store| {
            persist(
                store,
                &entry,
                &format!("Failed to save session capture to '{project_name}'"),
            )
        })
    } else if let Some(db_path) = db_path {
        let mut entry = entry.clone();
        entry.metadata = crate::provenance::restamp_provenance_for_destination(
            entry.metadata,
            db_path,
            target_db,
        );
        server.with_path_store(db_path, |store| {
            persist(
                store,
                &entry,
                &format!("Failed to save captured memory to {}", db_path.display()),
            )
        })
    } else {
        let dest_path = match target_db {
            DbScope::Global => Some(server.global_db_path_buf()),
            DbScope::Project => server.project_db_path_buf(),
        };
        let entry_to_write = if let Some(dest) = dest_path {
            let mut e = entry.clone();
            e.metadata =
                crate::provenance::restamp_provenance_for_destination(e.metadata, &dest, target_db);
            e
        } else {
            entry.clone()
        };
        server.with_store_for_scope(target_db, |store| {
            persist(store, &entry_to_write, "Failed to save captured memory")
        })
    }
}

pub(super) fn queue_capture_enrichment(
    server: &MemoryServer,
    target_db: DbScope,
    named_project: Option<String>,
    db_path: Option<PathBuf>,
    entry: &MemoryEntry,
    needs_summary: bool,
    agent_id: Option<&str>,
    path_prefix: Option<&str>,
) {
    if let Err(err) =
        server
            .enrichment_lock()
            .enrich_tx
            .try_send(crate::enrichment::build_enrichment_item(
                entry,
                true,
                needs_summary,
                target_db,
                named_project,
                db_path,
                agent_id.map(ToString::to_string),
                path_prefix.map(ToString::to_string),
                entry.revision,
            ))
    {
        tracing::warn!(
            error = %err,
            entry_id = %entry.id,
            "failed to queue capture enrichment"
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn merge_capture_entries_preserves_existing_diagnostic_counts() {
        let mut existing = crate::tests::make_entry("existing");
        existing.access_count = 3;
        existing.scored_count = 7;
        let incoming = crate::tests::make_entry("incoming");

        let merged = merge_capture_entries(&existing, &incoming, 0.9);

        assert_eq!(merged.access_count, 3);
        assert_eq!(merged.scored_count, 7);
    }
}
