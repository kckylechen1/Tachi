use chrono::Utc;
use serde_json::json;

use crate::foundry_runtime_ops::FOUNDRY_DISTILL_SOURCE;
use crate::server_state::{DbScope, MemoryServer};
use memcore::{
    store::immutable_supersession::ImmutableSupersessionTransaction, InsertMemoryResult,
    MemoryEdge, MemoryEntry, MemoryError, MemoryStore,
};
use tachi_foundry::{
    plan_daily_distill_memory, plan_distill_edges, should_archive_daily_distill_source,
    DailyDistillMemoryInput,
};

use super::types::{CandidateGroup, GroupPayload};

const DAILY_DISTILL_SOURCE_SET_CONTRACT: &str = "daily-distill-source-set-v1";

pub(super) fn normalized_source_memory_ids(group: &CandidateGroup) -> Vec<String> {
    let mut source_ids = group
        .entries
        .iter()
        .map(|entry| entry.id.clone())
        .collect::<Vec<_>>();
    source_ids.sort();
    source_ids.dedup();
    source_ids
}

pub(super) fn stable_distill_memory_id(
    group: &CandidateGroup,
    source_memory_ids: &[String],
) -> String {
    let identity = json!({
        "contract": DAILY_DISTILL_SOURCE_SET_CONTRACT,
        "path_prefix": group.path_prefix,
        "coherence_key": group.coherence_key,
        "source_memory_ids": source_memory_ids,
    });
    format!(
        "distill:{}",
        uuid::Uuid::new_v5(&uuid::Uuid::NAMESPACE_OID, identity.to_string().as_bytes())
    )
}

fn validate_existing_distill_winner(
    existing: &MemoryEntry,
    expected_id: &str,
    expected_source_ids: &[String],
) -> Result<(), MemoryError> {
    let mut stored_source_ids = existing
        .metadata
        .get("source_memory_ids")
        .and_then(|value| value.as_array())
        .into_iter()
        .flatten()
        .filter_map(|value| value.as_str().map(ToOwned::to_owned))
        .collect::<Vec<_>>();
    stored_source_ids.sort();
    stored_source_ids.dedup();
    let contract_matches = existing
        .metadata
        .get("source_set_contract")
        .and_then(|value| value.as_str())
        == Some(DAILY_DISTILL_SOURCE_SET_CONTRACT);
    let identity_matches = existing
        .metadata
        .get("source_set_identity")
        .and_then(|value| value.as_str())
        == Some(expected_id);
    if existing.source != FOUNDRY_DISTILL_SOURCE
        || !contract_matches
        || !identity_matches
        || stored_source_ids != expected_source_ids
    {
        return Err(MemoryError::InvalidArg(format!(
            "daily distill source-set identity collision for {expected_id}"
        )));
    }
    Ok(())
}

/// Claim every source before writing the candidate projection. The candidate
/// id is generated before this transaction and `superseded_by` is deliberately
/// not a foreign key, so the operation can fail fast on a stale source without
/// even tentatively writing its replacement row.
fn claim_distilled_sources<'a>(
    replacement: &mut ImmutableSupersessionTransaction<'_>,
    distill_entry: &MemoryEntry,
    source_entries: &'a [MemoryEntry],
) -> Result<Vec<&'a MemoryEntry>, MemoryError> {
    let mut claimed = Vec::new();
    for source in source_entries
        .iter()
        .filter(|entry| should_archive_daily_distill_source(entry))
    {
        // A false claim is an operation-wide conflict: this new distilled
        // candidate must not persist its own row, derived projection, or any
        // graph/archive side effect when an input already has an immutable
        // successor.
        replacement.claim_immutable_supersession(&source.id, &distill_entry.id)?;
        claimed.push(source);
    }
    Ok(claimed)
}

fn archive_claimed_distilled_sources(
    replacement: &mut ImmutableSupersessionTransaction<'_>,
    distill_entry: &MemoryEntry,
    claimed_sources: &[&MemoryEntry],
    batch_run_id: &str,
) -> Result<usize, MemoryError> {
    for source in claimed_sources {
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
        replacement.add_edge(&edge)?;
        replacement.archive_claimed_source(&source.id)?;
    }
    Ok(claimed_sources.len())
}

/// Write a single distilled memory + its provenance edges to the target store
/// (bound project or a named project), independent of which DB it targets.
fn write_distill_entry(
    store: &mut MemoryStore,
    entry: &MemoryEntry,
    source_entries: &[MemoryEntry],
    derived_id: &str,
    batch_run_id: &str,
) -> Result<InsertMemoryResult, String> {
    store
        .with_immutable_supersession_transaction(|replacement| {
            let inserted = replacement.insert_if_absent(entry)?;
            if inserted == InsertMemoryResult::Existing {
                let existing = replacement.get_memory(&entry.id)?.ok_or_else(|| {
                    MemoryError::Internal(format!(
                        "daily distill winner disappeared inside transaction: {}",
                        entry.id
                    ))
                })?;
                let expected_source_ids = entry
                    .metadata
                    .get("source_memory_ids")
                    .and_then(|value| value.as_array())
                    .into_iter()
                    .flatten()
                    .filter_map(|value| value.as_str().map(ToOwned::to_owned))
                    .collect::<Vec<_>>();
                validate_existing_distill_winner(&existing, &entry.id, &expected_source_ids)?;
                return Ok(inserted);
            }
            let claimed_sources = claim_distilled_sources(replacement, entry, source_entries)?;
            for edge in plan_distill_edges(entry, source_entries, "daily_batch", &entry.timestamp) {
                replacement.add_edge(&edge)?;
            }
            replacement.save_derived_with_id(
                derived_id,
                &entry.text,
                &entry.path,
                &entry.summary,
                entry.importance,
                &entry.source,
                &entry.scope,
                &entry.metadata,
            )?;
            archive_claimed_distilled_sources(replacement, entry, &claimed_sources, batch_run_id)?;
            Ok(inserted)
        })
        .map_err(|e| format!("persist daily distill replacement: {e}"))
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
    let source_memory_ids = normalized_source_memory_ids(group);
    let memory_id = stable_distill_memory_id(group, &source_memory_ids);
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
            "source_memory_ids": source_memory_ids,
            "source_set_contract": DAILY_DISTILL_SOURCE_SET_CONTRACT,
            "source_set_identity": memory_id,
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
        scored_count: 0,
        last_access: None,
        last_use_at: None,
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
    let _inserted = match project {
        Some(name) => server.with_named_project_store(name, |store| {
            write_distill_entry(store, &entry, &group.entries, &derived_id, batch_run_id)
        }),
        None => server.with_project_store(|store| {
            write_distill_entry(store, &entry, &group.entries, &derived_id, batch_run_id)
        }),
    }?;

    Ok(memory_id)
}
