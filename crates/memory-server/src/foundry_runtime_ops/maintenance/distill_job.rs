use super::super::capture::queue_capture_enrichment;
use super::super::helpers::dedup_strings;
use super::super::{FoundryMaintenanceItem, FOUNDRY_DISTILL_SOURCE};
use super::distill_helpers::{
    build_distill_edges, build_distill_input, build_foundry_distill_root, build_guide_distill_path,
    classify_distill_guide_type, coherent_distill_buckets, distill_quality_flags,
    infer_error_patterns, infer_file_patterns,
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
    let mut buckets = coherent_distill_buckets(raw_entries);
    if buckets.is_empty() {
        return Ok(DistillOutcome::Skipped(SKIP_NO_COHERENT_BUCKET.to_string()));
    }
    let (bucket_key, source_entries) = if let Some(preferred_key) = preferred_coherence_key {
        let preferred_bucket_key = format!("{}#{preferred_key}", item.path_prefix);
        if let Some(index) = buckets
            .iter()
            .position(|(key, _)| key == &preferred_bucket_key || key == &preferred_key)
        {
            buckets.swap_remove(index)
        } else {
            // Preferred key was requested but no bucket matched. Log so this
            // doesn't silently diverge from the scheduler's intent.
            tracing::debug!(
                "[foundry/distill] preferred_coherence_key={preferred_key:?} not found in buckets ({} buckets); falling back to largest",
                buckets.len()
            );
            buckets.sort_by(|a, b| b.1.len().cmp(&a.1.len()));
            match buckets.into_iter().next() {
                Some(bucket) => bucket,
                None => return Ok(DistillOutcome::Skipped(SKIP_NO_COHERENT_BUCKET.to_string())),
            }
        }
    } else {
        buckets.sort_by(|a, b| b.1.len().cmp(&a.1.len()));
        match buckets.into_iter().next() {
            Some(bucket) => bucket,
            None => return Ok(DistillOutcome::Skipped(SKIP_NO_COHERENT_BUCKET.to_string())),
        }
    };

    let quality_flags = distill_quality_flags(&source_entries);
    if !quality_flags.is_empty() {
        // Defense in depth: by construction `coherent_distill_buckets`
        // already drops buckets with non-empty flags, so this branch is
        // currently unreachable. Kept (and converted to a structured
        // Skipped reason rather than a panic) so a future contract drift
        // in `coherent_distill_buckets` produces an observable skip code
        // ("quality_flags:<csv>") instead of silently emitting a low
        // quality distill memory. PR #50 review.
        return Ok(DistillOutcome::Skipped(format!(
            "quality_flags:{}",
            quality_flags.join(",")
        )));
    }

    let (namespace_key, coherence_key) = bucket_key
        .split_once('#')
        .map(|(namespace, coherence)| (namespace.to_string(), coherence.to_string()))
        .unwrap_or_else(|| (item.path_prefix.clone(), bucket_key.clone()));

    let distill_text = server
        .llm
        .generate_distill(&build_distill_input(&source_entries))
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
    let guide_type = classify_distill_guide_type(&distill_text, &source_entries);
    let file_patterns = infer_file_patterns(&source_entries);
    let error_patterns = infer_error_patterns(&distill_text, &source_entries);
    let legacy_distill_root = build_foundry_distill_root(agent_id);
    let mut metadata = crate::provenance::inject_provenance(
        server,
        json!({
            "guide": true,
            "guide_type": guide_type,
            "guide_layer": "guide",
            "file_patterns": file_patterns,
            "error_patterns": error_patterns,
            "source_memory_ids": source_entries.iter().map(|entry| entry.id.clone()).collect::<Vec<_>>(),
            "source_path_prefix": item.path_prefix,
            "namespace_key": namespace_key,
            "coherence_key": coherence_key,
            "bucket_key": bucket_key,
            "quality_flags": quality_flags,
            "job_id": item.job.id,
            "legacy_distill_root": legacy_distill_root,
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
        path: build_guide_distill_path(agent_id, guide_type, &timestamp_segment),
        summary: distill_text.chars().take(100).collect(),
        text: distill_text,
        importance: 0.75,
        timestamp,
        valid_from: String::new(),
        valid_until: None,
        category: "guide".to_string(),
        topic: guide_type.to_string(),
        keywords: dedup_strings({
            let mut kws = vec![
                "foundry".to_string(),
                "distill".to_string(),
                "guide".to_string(),
                guide_type.to_string(),
            ];
            for entry in &source_entries {
                kws.extend(entry.keywords.iter().cloned());
            }
            kws
        }),
        persons: Vec::new(),
        entities: dedup_strings(
            source_entries
                .iter()
                .flat_map(|entry| entry.entities.clone())
                .collect::<Vec<_>>(),
        ),
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
    let edges = build_distill_edges(
        &distill_entry,
        &source_entries,
        guide_type,
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
