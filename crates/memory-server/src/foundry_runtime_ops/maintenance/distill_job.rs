use super::super::capture::queue_capture_enrichment;
use super::super::{FoundryMaintenanceItem, FOUNDRY_DISTILL_SOURCE};
use super::distill_helpers::{
    build_memory_distill_input, plan_distill_edges, plan_guide_distill_memory,
    select_memory_distill_bucket,
};
use super::store::{with_foundry_store, with_foundry_store_read};
use super::{
    DistillOutcome, SKIP_EMPTY_LLM_OUTPUT, SKIP_NO_COHERENT_BUCKET, SKIP_NO_SOURCE_ENTRIES,
};
use crate::server_state::{DbScope, MemoryServer};
use chrono::Utc;
use memory_core::MemoryEntry;
use serde_json::json;

pub(super) async fn process_memory_distill_job(
    server: &MemoryServer,
    item: &FoundryMaintenanceItem,
) -> Result<DistillOutcome, String> {
    let source_entries = with_foundry_store_read(server, item, |store| {
        let mut entries = Vec::new();
        for memory_id in &item.memory_ids {
            let maybe_entry = store
                .get(memory_id)
                .map_err(|e| format!("Failed to load memory {memory_id} for distill: {e}"))?;
            if let Some(entry) = maybe_entry {
                entries.push(entry);
            }
        }
        Ok(entries)
    })?;

    let raw_entries = source_entries
        .into_iter()
        .filter(|entry| !entry.archived && entry.source != FOUNDRY_DISTILL_SOURCE)
        .collect::<Vec<_>>();
    if raw_entries.is_empty() {
        return Ok(DistillOutcome::Skipped(SKIP_NO_SOURCE_ENTRIES.to_string()));
    }

    let preferred_coherence_key = item
        .job
        .metadata
        .get("job")
        .and_then(|job| job.get("coherence_key"))
        .and_then(|value| value.as_str())
        .map(str::to_string);

    // Coherence guard: only distil memories that share a topic or entity.
    // Pick the largest coherent bucket per job to keep behaviour 1:1 with the
    // legacy contract (one distill output per job).
    let Some(bucket) = select_memory_distill_bucket(
        raw_entries,
        preferred_coherence_key.as_deref(),
        &item.path_prefix,
    ) else {
        return Ok(DistillOutcome::Skipped(SKIP_NO_COHERENT_BUCKET.to_string()));
    };
    if preferred_coherence_key
        .as_deref()
        .is_some_and(|preferred_key| {
            bucket.bucket_key != format!("{}#{preferred_key}", item.path_prefix)
                && bucket.bucket_key != preferred_key
        })
    {
        tracing::debug!(
            "[foundry/distill] preferred_coherence_key={:?} not found; falling back to largest bucket {}",
            preferred_coherence_key,
            bucket.bucket_key
        );
    }

    if !bucket.quality_flags.is_empty() {
        // Defense in depth: by construction `coherent_distill_buckets`
        // already drops buckets with non-empty flags, so this branch is
        // currently unreachable. Kept (and converted to a structured
        // Skipped reason rather than a panic) so a future contract drift
        // in `coherent_distill_buckets` produces an observable skip code
        // ("quality_flags:<csv>") instead of silently emitting a low
        // quality distill memory. PR #50 review.
        return Ok(DistillOutcome::Skipped(format!(
            "quality_flags:{}",
            bucket.quality_flags.join(",")
        )));
    }

    let distill_text = server
        .llm
        .generate_distill(&build_memory_distill_input(&bucket))
        .await
        .map_err(|e| format!("Foundry distill summary failed: {e}"))?;
    if distill_text.trim().is_empty() {
        return Ok(DistillOutcome::Skipped(SKIP_EMPTY_LLM_OUTPUT.to_string()));
    }

    let agent_id = item
        .job
        .target_agent_id
        .as_deref()
        .unwrap_or("unknown-agent");
    let now = Utc::now();
    let timestamp = now.to_rfc3339();
    let timestamp_segment = now.format("%Y%m%dT%H%M%S").to_string();
    let memory_id = uuid::Uuid::new_v4().to_string();
    let plan = plan_guide_distill_memory(agent_id, &bucket, &distill_text, &timestamp_segment);
    let mut metadata = crate::provenance::inject_provenance(
        server,
        json!({
            "guide": true,
            "guide_type": plan.guide_type,
            "guide_layer": "guide",
            "file_patterns": plan.file_patterns,
            "error_patterns": plan.error_patterns,
            "source_memory_ids": plan.source_memory_ids,
            "source_path_prefix": plan.source_path_prefix,
            "namespace_key": plan.namespace_key,
            "coherence_key": plan.coherence_key,
            "bucket_key": plan.bucket_key,
            "quality_flags": plan.quality_flags,
            "job_id": item.job.id,
            "legacy_distill_root": plan.legacy_distill_root,
        }),
        "foundry_worker",
        "memory_distill",
        Some(if item.target_db == DbScope::Project {
            "project"
        } else {
            "global"
        }),
        item.target_db,
        json!({
            "agent_id": agent_id,
            "path_prefix": item.path_prefix,
        }),
    );
    if let Some(db_path) = item.db_path.as_ref() {
        metadata = crate::provenance::restamp_provenance_for_destination(
            metadata,
            db_path,
            item.target_db,
        );
    }

    let distill_entry = MemoryEntry {
        id: memory_id.clone(),
        path: plan.path,
        summary: plan.summary,
        text: distill_text,
        importance: 0.75,
        timestamp,
        valid_from: String::new(),
        valid_until: None,
        category: "guide".to_string(),
        topic: plan.guide_type.clone(),
        keywords: plan.keywords,
        persons: Vec::new(),
        entities: plan.entities,
        location: String::new(),
        source: FOUNDRY_DISTILL_SOURCE.to_string(),
        scope: if item.target_db == DbScope::Project {
            "project".to_string()
        } else {
            "global".to_string()
        },
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
        tier: "raw".to_string(),
    };

    with_foundry_store(server, item, |store| {
        store
            .upsert(&distill_entry)
            .map_err(|e| format!("Failed to save foundry distill memory: {e}"))
    })?;
    let edges = plan_distill_edges(
        &distill_entry,
        &bucket.entries,
        &plan.guide_type,
        &distill_entry.timestamp,
    );
    with_foundry_store(server, item, |store| {
        for edge in &edges {
            store
                .add_edge(edge)
                .map_err(|e| format!("Failed to save foundry distill edge: {e}"))?;
        }
        Ok(())
    })?;
    queue_capture_enrichment(
        server,
        item.target_db,
        item.named_project.clone(),
        item.db_path.clone(),
        &distill_entry,
        false,
        None,
        None,
    );

    Ok(DistillOutcome::Wrote)
}
