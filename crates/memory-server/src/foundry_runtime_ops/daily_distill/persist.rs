use chrono::Utc;
use serde_json::json;

use crate::server_state::{DbScope, MemoryServer};
use memory_core::{MemoryEdge, MemoryEntry, MemoryStore};

use crate::foundry_runtime_ops::helpers::dedup_strings;
use crate::foundry_runtime_ops::maintenance::{build_distill_edges, build_foundry_distill_root};
use crate::foundry_runtime_ops::FOUNDRY_DISTILL_SOURCE;

use super::types::{CandidateGroup, GroupPayload};

const DISTILLED_SOURCE_ARCHIVE_IMPORTANCE_CEILING: f64 = 0.85;

fn should_archive_distilled_source(entry: &MemoryEntry) -> bool {
    if entry.archived
        || entry.source.eq_ignore_ascii_case(FOUNDRY_DISTILL_SOURCE)
        || !entry.tier.eq_ignore_ascii_case("raw")
        || entry.access_count > 0
        || entry.recall_count > 0
    {
        return false;
    }

    if entry
        .retention_policy
        .as_deref()
        .is_some_and(|policy| matches!(policy, "pinned" | "permanent"))
    {
        return false;
    }

    entry.importance < DISTILLED_SOURCE_ARCHIVE_IMPORTANCE_CEILING
}

fn archive_distilled_sources(
    store: &mut MemoryStore,
    distill_entry: &MemoryEntry,
    source_entries: &[MemoryEntry],
    batch_run_id: &str,
) -> Result<usize, String> {
    let mut archived_count = 0;
    for source in source_entries
        .iter()
        .filter(|entry| should_archive_distilled_source(entry))
    {
        let edge = MemoryEdge {
            source_id: distill_entry.id.clone(),
            target_id: source.id.clone(),
            relation: "supersedes".to_string(),
            weight: 1.0,
            metadata: json!({
                "source": "foundry_distill",
                "batch_run_id": batch_run_id,
                "reason": "distilled_source_archived",
            }),
            created_at: distill_entry.timestamp.clone(),
            valid_from: distill_entry.timestamp.clone(),
            valid_to: None,
        };
        store
            .add_edge(&edge)
            .map_err(|e| format!("add supersedes edge: {e}"))?;
        store
            .supersede_memory(&source.id, &distill_entry.id)
            .map_err(|e| format!("mark distilled source superseded: {e}"))?;
        if store
            .archive_memory(&source.id)
            .map_err(|e| format!("archive distilled source: {e}"))?
        {
            archived_count += 1;
        }
    }
    Ok(archived_count)
}

/// Write a single distilled memory + its provenance edges to the target store
/// (bound project or a named project), independent of which DB it targets.
fn write_distill_entry(
    store: &mut MemoryStore,
    entry: &MemoryEntry,
    source_entries: &[MemoryEntry],
    derived_id: &str,
    batch_run_id: &str,
) -> Result<(), String> {
    store
        .upsert(entry)
        .map_err(|e| format!("upsert distill memory: {e}"))?;
    for edge in build_distill_edges(entry, source_entries, "daily_batch", &entry.timestamp) {
        store
            .add_edge(&edge)
            .map_err(|e| format!("add distill edge: {e}"))?;
    }
    store
        .save_derived_with_id(
            derived_id,
            &entry.text,
            &entry.path,
            &entry.summary,
            entry.importance,
            &entry.source,
            &entry.scope,
            &entry.metadata,
        )
        .map_err(|e| format!("save derived distill item: {e}"))?;
    archive_distilled_sources(store, entry, source_entries, batch_run_id)?;
    Ok(())
}

pub(crate) fn persist_distill_memory(
    server: &MemoryServer,
    group: &CandidateGroup,
    payload: &GroupPayload,
    batch_run_id: &str,
    backend: &str,
    fallback_used: bool,
    project: Option<&str>,
) -> Result<String, String> {
    let agent_id = server
        .agent_runtime_read()
        .agent_profile
        .as_ref()
        .map(|p| p.agent_id.clone())
        .unwrap_or_else(|| "tachi_scheduler".to_string());
    let distill_root = build_foundry_distill_root(&agent_id);
    let timestamp = Utc::now().to_rfc3339();
    let memory_id = uuid::Uuid::new_v4().to_string();
    let source_ids: Vec<String> = group.entries.iter().map(|e| e.id.clone()).collect();

    let bucket_key = format!("{}#{}", group.path_prefix, group.coherence_key);
    let namespace_key = group.path_prefix.clone();

    let metadata = crate::provenance::inject_provenance(
        server,
        json!({
            "source_memory_ids": source_ids,
            "source_path_prefix": group.path_prefix,
            "namespace_key": namespace_key,
            "coherence_key": group.coherence_key,
            "bucket_key": bucket_key,
            "batch_run_id": batch_run_id,
            "group_id": group.group_id,
            "backend": backend,
            "fallback_used": fallback_used,
        }),
        "foundry_worker",
        "memory_distill",
        Some("project"),
        DbScope::Project,
        json!({
            "agent_id": agent_id,
            "path_prefix": group.path_prefix,
        }),
    );

    let summary = if payload.summary.trim().is_empty() {
        payload.text.chars().take(100).collect::<String>()
    } else {
        payload.summary.clone()
    };

    let mut keywords = vec!["foundry".to_string(), "distill".to_string()];
    keywords.extend(payload.keywords.iter().cloned());
    for entry in &group.entries {
        keywords.extend(entry.keywords.iter().cloned());
    }
    let keywords = dedup_strings(keywords);
    let entities = dedup_strings(
        group
            .entries
            .iter()
            .flat_map(|e| e.entities.clone())
            .collect(),
    );

    let entry = MemoryEntry {
        id: memory_id.clone(),
        path: format!("{distill_root}/{}", Utc::now().format("%Y%m%dT%H%M%S")),
        summary,
        text: payload.text.clone(),
        importance: 0.75,
        timestamp,
        valid_from: String::new(),
        valid_until: None,
        category: "other".to_string(),
        topic: "foundry_distill".to_string(),
        keywords,
        persons: Vec::new(),
        entities,
        location: String::new(),
        source: FOUNDRY_DISTILL_SOURCE.to_string(),
        scope: "project".to_string(),
        archived: false,
        access_count: 0,
        last_access: None,
        revision: 1,
        metadata,
        vector: None,
        retention_policy: Some("permanent".to_string()),
        domain: Some("foundry".to_string()),
        recall_count: 0,
        query_diversity: 0,
        tier: "consolidated".to_string(), // distilled memories skip raw — directly promoted
    };

    let derived_id = format!("derived:{memory_id}");
    match project {
        Some(name) => server.with_named_project_store(name, |store| {
            write_distill_entry(store, &entry, &group.entries, &derived_id, batch_run_id)
        }),
        None => server.with_project_store(|store| {
            write_distill_entry(store, &entry, &group.entries, &derived_id, batch_run_id)
        }),
    }?;

    Ok(memory_id)
}
