use chrono::Utc;
use serde_json::json;

use crate::foundry_runtime_ops::FOUNDRY_DISTILL_SOURCE;
use crate::server_state::{DbScope, MemoryServer};
use memcore::{MemoryEdge, MemoryEntry, MemoryStore};
use tachi_foundry::{
    plan_daily_distill_memory, plan_distill_edges, should_archive_daily_distill_source,
    DailyDistillMemoryInput,
};

use super::types::{CandidateGroup, GroupPayload};

fn archive_distilled_sources(
    store: &mut MemoryStore,
    distill_entry: &MemoryEntry,
    source_entries: &[MemoryEntry],
    batch_run_id: &str,
) -> Result<usize, String> {
    let mut archived_count = 0;
    for source in source_entries
        .iter()
        .filter(|entry| should_archive_daily_distill_source(entry))
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
    for edge in plan_distill_edges(entry, source_entries, "daily_batch", &entry.timestamp) {
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
    let timestamp = Utc::now().to_rfc3339();
    let timestamp_segment = Utc::now().format("%Y%m%dT%H%M%S").to_string();
    let memory_id = uuid::Uuid::new_v4().to_string();
    let plan = plan_daily_distill_memory(DailyDistillMemoryInput {
        agent_id: &agent_id,
        path_prefix: &group.path_prefix,
        coherence_key: &group.coherence_key,
        entries: &group.entries,
        payload_summary: &payload.summary,
        payload_text: &payload.text,
        payload_keywords: &payload.keywords,
        timestamp_segment: &timestamp_segment,
    });

    let metadata = crate::provenance::inject_provenance(
        server,
        json!({
            "source_memory_ids": plan.source_memory_ids,
            "source_path_prefix": group.path_prefix,
            "namespace_key": plan.namespace_key,
            "coherence_key": group.coherence_key,
            "bucket_key": plan.bucket_key,
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

    let entry = MemoryEntry {
        id: memory_id.clone(),
        path: plan.path,
        summary: plan.summary,
        text: payload.text.clone(),
        importance: 0.75,
        timestamp,
        valid_from: String::new(),
        valid_until: None,
        category: "other".to_string(),
        topic: "foundry_distill".to_string(),
        keywords: plan.keywords,
        persons: Vec::new(),
        entities: plan.entities,
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
