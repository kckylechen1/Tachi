use super::helpers::*;
use super::*;

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

    MemoryEntry {
        id: existing.id.clone(),
        path: if existing.path.trim().is_empty() {
            incoming.path.clone()
        } else {
            existing.path.clone()
        },
        summary: merged_summary,
        text: merged_text,
        importance: existing.importance.max(incoming.importance),
        timestamp,
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
        persons: dedup_strings(
            existing
                .persons
                .iter()
                .cloned()
                .chain(incoming.persons.iter().cloned())
                .collect(),
        ),
        entities: dedup_strings(
            existing
                .entities
                .iter()
                .cloned()
                .chain(incoming.entities.iter().cloned())
                .collect(),
        ),
        location: if incoming.location.trim().is_empty() {
            existing.location.clone()
        } else {
            incoming.location.clone()
        },
        source: "capture_session".to_string(),
        scope: if incoming.scope == "general" {
            existing.scope.clone()
        } else {
            incoming.scope.clone()
        },
        archived: false,
        access_count: existing.access_count,
        last_access: existing.last_access.clone(),
        revision: existing.revision + 1,
        metadata: merge_metadata(
            &existing.metadata,
            &incoming.metadata,
            &incoming.id,
            similarity,
        ),
        vector: None,
        retention_policy: None,
        domain: None,
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
    }
}

pub(super) fn search_similar_capture_entries(
    server: &MemoryServer,
    target_db: DbScope,
    named_project: Option<&str>,
    path_prefix: &str,
    query_vec: &[f32],
    top_k: usize,
) -> Result<Vec<memory_core::SearchResult>, String> {
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
    } else {
        server.with_store_for_scope_read(target_db, search_action)
    }
}

pub(super) fn persist_capture_entry(
    server: &MemoryServer,
    target_db: DbScope,
    named_project: Option<&str>,
    entry: &MemoryEntry,
) -> Result<(), String> {
    if let Some(project_name) = named_project {
        let dest_path = MemoryServer::resolve_named_project_db_path(project_name)?;
        let mut entry = entry.clone();
        entry.metadata = crate::provenance::restamp_provenance_for_destination(
            entry.metadata,
            &dest_path,
            DbScope::Project,
        );
        server.with_named_project_store(project_name, |store| {
            store
                .upsert(&entry)
                .map_err(|e| format!("Failed to save session capture to '{project_name}': {e}"))
        })
    } else {
        let dest_path = match target_db {
            DbScope::Global => Some(server.global_db_path.as_ref().clone()),
            DbScope::Project => server.project_db_path.as_ref().map(|p| p.as_ref().clone()),
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
            store
                .upsert(&entry_to_write)
                .map_err(|e| format!("Failed to save captured memory: {e}"))
        })
    }
}

pub(super) fn queue_capture_enrichment(
    server: &MemoryServer,
    target_db: DbScope,
    named_project: Option<String>,
    entry: &MemoryEntry,
    needs_summary: bool,
    agent_id: Option<&str>,
    path_prefix: Option<&str>,
) {
    let _ = server.enrich_tx.try_send(EnrichmentItem {
        id: entry.id.clone(),
        text: entry.text.clone(),
        summary: entry.summary.clone(),
        keywords: entry.keywords.clone(),
        needs_embedding: true,
        needs_summary,
        target_db,
        named_project,
        db_path: None,
        foundry_agent_id: agent_id.map(ToString::to_string),
        foundry_path_prefix: path_prefix.map(ToString::to_string),
        revision: entry.revision,
    });
}
