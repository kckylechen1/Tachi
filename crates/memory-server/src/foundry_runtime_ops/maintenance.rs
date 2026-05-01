use super::capture::*;
use super::helpers::*;
use super::recall::{rerank_rows, value_id, value_path, value_relevance, value_topic};
use super::*;
use std::collections::{HashMap, HashSet};
use std::sync::atomic::Ordering;

/// Outcome of a memory-distill job. Carries a structured skip reason so
/// foundry_jobs.metadata.skip_reason answers "why didn't this run?" instead
/// of the previous opaque "worker reported no-op".
pub(crate) enum DistillOutcome {
    /// Wrote a new distill memory with this id. The id is preserved on the
    /// variant so future call sites (e.g. metrics, tracing, or a follow-up
    /// rerank scheduler) can correlate the newly-written memory with the
    /// originating job; today only the `Completed` status is surfaced.
    Wrote(#[allow(dead_code)] String),
    Skipped(String),
}

// ---- Foundry distill skip-reason codes (stable, surfaced in DB metadata) ----
/// All source memories filtered out (archived or already a distill output).
pub(crate) const SKIP_NO_SOURCE_ENTRIES: &str = "no_source_entries";
/// No coherent topic/entity bucket among the source memories.
pub(crate) const SKIP_NO_COHERENT_BUCKET: &str = "no_coherent_bucket";
/// LLM returned an empty payload (post-trim).
pub(crate) const SKIP_EMPTY_LLM_OUTPUT: &str = "empty_llm_output";

fn foundry_requested_by(server: &MemoryServer) -> Option<String> {
    read_or_recover(&server.agent_profile, "agent_profile")
        .as_ref()
        .map(|profile| profile.agent_id.clone())
}

fn foundry_job_lane(kind: &memory_core::FoundryJobKind) -> memory_core::FoundryModelLane {
    match kind {
        memory_core::FoundryJobKind::MemoryNeighborhood => {
            memory_core::FoundryModelLane::Maintenance
        }
        memory_core::FoundryJobKind::RecallRerankCache => memory_core::FoundryModelLane::Rerank,
        memory_core::FoundryJobKind::MemoryDistill => memory_core::FoundryModelLane::Distill,
        memory_core::FoundryJobKind::ForgetSweep => memory_core::FoundryModelLane::Distill,
        _ => memory_core::FoundryModelLane::Reasoning,
    }
}

fn foundry_worker_name(kind: &memory_core::FoundryJobKind) -> &'static str {
    match kind {
        memory_core::FoundryJobKind::MemoryNeighborhood => "foundry_neighborhood",
        memory_core::FoundryJobKind::RecallRerankCache => "foundry_recall_rerank_cache",
        memory_core::FoundryJobKind::MemoryDistill => "foundry_distill",
        memory_core::FoundryJobKind::ForgetSweep => "foundry_forget",
        _ => "foundry",
    }
}

pub(super) fn foundry_job_label(kind: &memory_core::FoundryJobKind) -> &'static str {
    match kind {
        memory_core::FoundryJobKind::MemoryNeighborhood => "memory_neighborhood",
        memory_core::FoundryJobKind::RecallRerankCache => "recall_rerank_cache",
        memory_core::FoundryJobKind::MemoryDistill => "memory_distill",
        memory_core::FoundryJobKind::ForgetSweep => "forget_sweep",
        memory_core::FoundryJobKind::SessionIngest => "session_ingest",
        memory_core::FoundryJobKind::MemoryEnrichment => "memory_enrichment",
        memory_core::FoundryJobKind::SkillEvolution => "skill_evolution",
        memory_core::FoundryJobKind::AgentEvolution => "agent_evolution",
        memory_core::FoundryJobKind::ProfileProjection => "profile_projection",
    }
}

fn build_foundry_maintenance_job(
    server: &MemoryServer,
    kind: memory_core::FoundryJobKind,
    agent_id: &str,
    path_prefix: &str,
    memory_ids: &[String],
    metadata: serde_json::Value,
) -> memory_core::FoundryJobSpec {
    memory_core::FoundryJobSpec {
        id: format!("foundry-job:{}", uuid::Uuid::new_v4()),
        kind: kind.clone(),
        lane: foundry_job_lane(&kind),
        status: memory_core::FoundryJobStatus::Queued,
        target_agent_id: Some(agent_id.to_string()),
        requested_by: foundry_requested_by(server),
        created_at: Utc::now().to_rfc3339(),
        evidence_count: memory_ids.len(),
        goal_count: 1,
        metadata: json!({
            "path_prefix": path_prefix,
            "memory_ids": memory_ids,
            "job": metadata,
        }),
    }
}

pub(super) fn enqueue_capture_maintenance_jobs(
    server: &MemoryServer,
    target_db: DbScope,
    named_project: Option<String>,
    agent_id: &str,
    path_prefix: &str,
    memory_ids: &[String],
    merged_count: usize,
    duplicate_count: usize,
) -> Result<Vec<memory_core::FoundryJobSpec>, String> {
    if memory_ids.is_empty() {
        return Ok(Vec::new());
    }

    let specs = vec![
        build_foundry_maintenance_job(
            server,
            memory_core::FoundryJobKind::MemoryNeighborhood,
            agent_id,
            path_prefix,
            memory_ids,
            json!({
                "kind": "memory_neighborhood",
                "neighbor_limit": FOUNDRY_RELATED_LIMIT,
                "merged_count": merged_count,
                "duplicate_count": duplicate_count,
            }),
        ),
        build_foundry_maintenance_job(
            server,
            memory_core::FoundryJobKind::RecallRerankCache,
            agent_id,
            path_prefix,
            memory_ids,
            json!({
                "kind": "recall_rerank_cache",
                "top_k": FOUNDRY_RECALL_RERANK_TOP_K,
                "candidate_multiplier": FOUNDRY_RECALL_RERANK_CANDIDATE_MULTIPLIER,
            }),
        ),
        build_foundry_maintenance_job(
            server,
            memory_core::FoundryJobKind::MemoryDistill,
            agent_id,
            path_prefix,
            memory_ids,
            json!({
                "kind": "memory_distill",
                "window": FOUNDRY_DISTILL_WINDOW,
            }),
        ),
        build_foundry_maintenance_job(
            server,
            memory_core::FoundryJobKind::ForgetSweep,
            agent_id,
            path_prefix,
            memory_ids,
            json!({
                "kind": "forget_sweep",
                "keep_latest": FOUNDRY_DISTILL_KEEP,
            }),
        ),
    ];

    for spec in &specs {
        server.enqueue_foundry_job(FoundryMaintenanceItem {
            job: spec.clone(),
            target_db,
            named_project: named_project.clone(),
            path_prefix: path_prefix.to_string(),
            memory_ids: memory_ids.to_vec(),
        })?;
    }

    Ok(specs)
}

pub(crate) fn enqueue_foundry_capture_maintenance(
    server: &MemoryServer,
    target_db: DbScope,
    named_project: Option<String>,
    agent_id: &str,
    path_prefix: &str,
    memory_ids: &[String],
) -> Result<Vec<memory_core::FoundryJobSpec>, String> {
    enqueue_capture_maintenance_jobs(
        server,
        target_db,
        named_project,
        agent_id,
        path_prefix,
        memory_ids,
        0,
        0,
    )
}

pub(super) fn scheduled_distill_path_prefix(path: &str) -> String {
    let segments = path
        .trim_matches('/')
        .split('/')
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>();
    match segments.as_slice() {
        [] => "/".to_string(),
        ["project", second, ..] => format!("/project/{second}"),
        ["kanban", from, to, ..] => format!("/kanban/{from}/{to}"),
        ["kanban", from] => format!("/kanban/{from}"),
        ["wiki", kind, domain, ..] => format!("/wiki/{kind}/{domain}"),
        ["wiki", kind] => format!("/wiki/{kind}"),
        [first, ..] => format!("/{first}"),
    }
}

fn is_generic_distill_topic(topic: &str) -> bool {
    matches!(
        topic.trim().to_ascii_lowercase().as_str(),
        "" | "unknown"
            | "general"
            | "other"
            | "misc"
            | "architecture"
            | "bug fix"
            | "bug fixes"
            | "bugfix"
            | "testing"
            | "test"
            | "changelog"
            | "roadmap"
            | "todo"
    )
}

pub(super) fn coherence_bucket_key(topic: &str, entities: &[String]) -> Option<String> {
    let topic = topic.trim();
    if !topic.is_empty() && !is_generic_distill_topic(topic) {
        return Some(format!("topic:{topic}"));
    }

    entities
        .iter()
        .map(|entity| entity.trim())
        .find(|entity| !entity.is_empty())
        .map(|entity| format!("entity:{entity}"))
}

pub(super) fn scheduled_distill_group_key(path: &str, coherence_key: &str) -> String {
    format!("{}#{coherence_key}", scheduled_distill_path_prefix(path))
}

fn distill_metadata_is_trusted(metadata: &serde_json::Value) -> bool {
    let has_coherence_key = metadata
        .get("coherence_key")
        .and_then(|value| value.as_str())
        .is_some_and(|value| !value.trim().is_empty());
    if !has_coherence_key {
        return false;
    }

    let has_bad_flag = metadata
        .get("quality_flags")
        .and_then(|value| value.as_array())
        .into_iter()
        .flatten()
        .filter_map(|value| value.as_str())
        .any(|flag| {
            matches!(
                flag,
                "legacy" | "suspect" | "incoherent" | "legacy_incoherent_distill"
            )
        });
    !has_bad_flag
}

fn source_namespace_count(entries: &[MemoryEntry]) -> usize {
    entries
        .iter()
        .map(|entry| scheduled_distill_path_prefix(&entry.path))
        .collect::<HashSet<_>>()
        .len()
}

fn distill_quality_flags(entries: &[MemoryEntry]) -> Vec<String> {
    let mut flags = Vec::new();
    if entries.len() < FOUNDRY_DISTILL_MIN_BATCH {
        flags.push("min_batch_not_met".to_string());
    }
    if source_namespace_count(entries) > 1 {
        flags.push("mixed_namespace".to_string());
    }
    flags
}

/// Scan for project memories that have not yet been included in a distill output
/// and enqueue distill jobs for sufficiently large coherent groups.
pub(crate) async fn schedule_pending_distill_jobs(server: &MemoryServer) -> Result<usize, String> {
    struct ScheduledDistillGroup {
        path_prefix: String,
        coherence_key: String,
        memory_ids: Vec<String>,
    }

    if !server.has_project_db() {
        return Ok(0);
    }

    let (unprocessed_memories, pending_groups) = server.with_project_store_read(|store| {
        let conn = store.connection();

        let mut processed_ids = HashSet::new();
        let mut stmt = conn
            .prepare("SELECT metadata FROM memories WHERE archived = 0 AND source = ?1")
            .map_err(|e| format!("prepare distill metadata query: {e}"))?;
        let rows = stmt
            .query_map([FOUNDRY_DISTILL_SOURCE], |row| row.get::<_, String>(0))
            .map_err(|e| format!("query distill metadata rows: {e}"))?;
        for row in rows {
            let metadata_raw = row.map_err(|e| format!("read distill metadata row: {e}"))?;
            let metadata = serde_json::from_str::<serde_json::Value>(&metadata_raw)
                .unwrap_or_else(|_| json!({}));
            if distill_metadata_is_trusted(&metadata) {
                let source_ids = metadata
                    .get("source_memory_ids")
                    .and_then(|value| value.as_array())
                    .into_iter()
                    .flatten()
                    .filter_map(|value| value.as_str());
                processed_ids.extend(source_ids.map(ToOwned::to_owned));
            }
        }

        let mut pending_groups = HashSet::new();
        let mut stmt = conn
            .prepare(
                "SELECT path_prefix, metadata
                 FROM foundry_jobs
                 WHERE kind = 'memory_distill' AND status IN ('queued', 'running')",
            )
            .map_err(|e| format!("prepare pending distill job query: {e}"))?;
        let rows = stmt
            .query_map([], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })
            .map_err(|e| format!("query pending distill jobs: {e}"))?;
        for row in rows {
            let (path_prefix, metadata_raw) =
                row.map_err(|e| format!("read pending distill row: {e}"))?;
            let metadata = serde_json::from_str::<serde_json::Value>(&metadata_raw)
                .unwrap_or_else(|_| json!({}));
            let coherence_key = metadata
                .get("job")
                .and_then(|job| job.get("coherence_key"))
                .and_then(|value| value.as_str());
            if let Some(coherence_key) = coherence_key {
                pending_groups.insert(scheduled_distill_group_key(&path_prefix, coherence_key));
            } else {
                pending_groups.insert(path_prefix);
            }
        }

        let mut unprocessed = Vec::new();
        let mut stmt = conn
            .prepare(
                "SELECT id, path, topic, entities
                 FROM memories
                  WHERE archived = 0 AND source != ?1
                  ORDER BY timestamp ASC",
            )
            .map_err(|e| format!("prepare candidate memory query: {e}"))?;
        let rows = stmt
            .query_map([FOUNDRY_DISTILL_SOURCE], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                ))
            })
            .map_err(|e| format!("query candidate memories: {e}"))?;
        for row in rows {
            let (memory_id, path, topic, entities_raw) =
                row.map_err(|e| format!("read candidate memory row: {e}"))?;
            if !processed_ids.contains(&memory_id) {
                let entities =
                    serde_json::from_str::<Vec<String>>(&entities_raw).unwrap_or_default();
                unprocessed.push((memory_id, path, topic, entities));
            }
        }

        Ok((unprocessed, pending_groups))
    })?;

    if unprocessed_memories.is_empty() {
        return Ok(0);
    }

    let mut grouped_ids: HashMap<String, ScheduledDistillGroup> = HashMap::new();
    for (memory_id, path, topic, entities) in unprocessed_memories {
        let Some(coherence_key) = coherence_bucket_key(&topic, &entities) else {
            continue;
        };
        let path_prefix = scheduled_distill_path_prefix(&path);
        let group_key = scheduled_distill_group_key(&path_prefix, &coherence_key);
        if pending_groups.contains(&group_key) || pending_groups.contains(&path_prefix) {
            continue;
        }
        grouped_ids
            .entry(group_key)
            .or_insert_with(|| ScheduledDistillGroup {
                path_prefix: path_prefix.clone(),
                coherence_key: coherence_key.clone(),
                memory_ids: Vec::new(),
            })
            .memory_ids
            .push(memory_id);
    }

    let scheduler_agent_id =
        foundry_requested_by(server).unwrap_or_else(|| "tachi_scheduler".into());
    let mut jobs_scheduled = 0usize;

    for (_, group) in grouped_ids {
        if group.memory_ids.len() < FOUNDRY_DISTILL_MIN_BATCH {
            continue;
        }

        let job = build_foundry_maintenance_job(
            server,
            memory_core::FoundryJobKind::MemoryDistill,
            &scheduler_agent_id,
            &group.path_prefix,
            &group.memory_ids,
            json!({
                "kind": "memory_distill",
                "window": FOUNDRY_DISTILL_WINDOW,
                "coherence_key": group.coherence_key,
                "scheduler": "bootstrap_distill_interval",
            }),
        );

        server.enqueue_foundry_job(FoundryMaintenanceItem {
            job,
            target_db: DbScope::Project,
            named_project: None,
            path_prefix: group.path_prefix,
            memory_ids: group.memory_ids,
        })?;
        jobs_scheduled += 1;
    }

    Ok(jobs_scheduled)
}

pub(super) fn with_foundry_store<T>(
    server: &MemoryServer,
    item: &FoundryMaintenanceItem,
    f: impl FnOnce(&mut MemoryStore) -> Result<T, String>,
) -> Result<T, String> {
    if let Some(project_name) = item.named_project.as_deref() {
        server.with_named_project_store(project_name, f)
    } else {
        server.with_store_for_scope(item.target_db, f)
    }
}

pub(super) fn with_foundry_store_read<T>(
    server: &MemoryServer,
    item: &FoundryMaintenanceItem,
    f: impl FnOnce(&mut MemoryStore) -> Result<T, String>,
) -> Result<T, String> {
    if let Some(project_name) = item.named_project.as_deref() {
        server.with_named_project_store_read(project_name, f)
    } else {
        server.with_store_for_scope_read(item.target_db, f)
    }
}

pub(super) fn memory_claim_signature(entry: &MemoryEntry) -> String {
    format!(
        "{}:r{}:vec{}:arch{}",
        entry.id,
        entry.revision,
        if entry.vector.is_some() { 1 } else { 0 },
        if entry.archived { 1 } else { 0 }
    )
}

pub(super) fn build_foundry_event_hash(
    server: &MemoryServer,
    item: &FoundryMaintenanceItem,
) -> Result<String, String> {
    let mut signatures = Vec::with_capacity(item.memory_ids.len());
    for memory_id in &item.memory_ids {
        let maybe_entry = with_foundry_store_read(server, item, |store| {
            store
                .get(memory_id)
                .map_err(|e| format!("Failed to load memory {} for claim hash: {e}", memory_id))
        })?;
        match maybe_entry {
            Some(entry) => signatures.push(memory_claim_signature(&entry)),
            None => signatures.push(format!("{memory_id}:missing")),
        }
    }
    signatures.sort();
    let job_scope = match item.job.kind {
        memory_core::FoundryJobKind::RecallRerankCache => {
            stable_hash(&item.job.metadata.to_string())
        }
        _ => String::new(),
    };

    Ok(stable_hash(&format!(
        "{}:{}:{}:{}:{}",
        foundry_job_label(&item.job.kind),
        item.named_project.as_deref().unwrap_or("default"),
        item.path_prefix,
        job_scope,
        signatures.join(","),
    )))
}

pub(super) fn merge_foundry_metadata(
    existing: &serde_json::Value,
    patch: serde_json::Value,
) -> serde_json::Value {
    let mut root = match existing {
        serde_json::Value::Object(map) => map.clone(),
        _ => serde_json::Map::new(),
    };
    let mut foundry = root
        .get("foundry")
        .and_then(|value| value.as_object().cloned())
        .unwrap_or_default();
    if let Some(patch_obj) = patch.as_object() {
        for (key, value) in patch_obj {
            foundry.insert(key.clone(), value.clone());
        }
    }
    root.insert("foundry".into(), serde_json::Value::Object(foundry));
    serde_json::Value::Object(root)
}

pub(super) fn update_entry_metadata(
    store: &mut MemoryStore,
    entry: &MemoryEntry,
    metadata: &serde_json::Value,
) -> Result<bool, String> {
    store
        .update_with_revision(
            &entry.id,
            &entry.text,
            &entry.summary,
            &entry.source,
            metadata,
            entry.vector.as_deref(),
            entry.revision,
        )
        .map_err(|e| format!("Failed to update foundry metadata for {}: {e}", entry.id))
}

pub(super) fn build_foundry_distill_root(agent_id: &str) -> String {
    format!("{}/distilled", build_foundry_agent_root(agent_id))
}

async fn process_memory_neighborhood_job(
    server: &MemoryServer,
    item: &FoundryMaintenanceItem,
) -> Result<usize, String> {
    let mut updated = 0usize;

    for memory_id in &item.memory_ids {
        let Some(entry) = with_foundry_store_read(server, item, |store| {
            store
                .get(memory_id)
                .map_err(|e| format!("Failed to load memory {} for neighborhood: {e}", memory_id))
        })?
        else {
            continue;
        };

        let Some(vector) = entry.vector.clone() else {
            continue;
        };

        let neighbors = search_similar_capture_entries(
            server,
            item.target_db,
            item.named_project.as_deref(),
            &item.path_prefix,
            &vector,
            FOUNDRY_RELATED_LIMIT + 2,
        )?;

        let mut best_neighbor: Option<memory_core::SearchResult> = None;
        let related = neighbors
            .into_iter()
            .filter(|row| row.entry.id != entry.id)
            .inspect(|row| {
                if best_neighbor.is_none() {
                    best_neighbor = Some(row.clone());
                }
            })
            .take(FOUNDRY_RELATED_LIMIT)
            .map(|row| {
                json!({
                    "id": row.entry.id,
                    "topic": row.entry.topic,
                    "path": row.entry.path,
                    "score": round3(row.score.vector),
                })
            })
            .collect::<Vec<_>>();

        if let Some(similar) = best_neighbor {
            let similarity = similar.score.vector;
            if similarity >= CAPTURE_DEDUP_THRESHOLD {
                let changed = with_foundry_store(server, item, |store| {
                    store.archive_memory(&entry.id).map_err(|e| {
                        format!("Failed to archive duplicate memory {}: {e}", entry.id)
                    })
                })?;
                if changed {
                    updated += 1;
                }
                continue;
            }

            if similarity >= CAPTURE_MERGE_THRESHOLD {
                let merged = merge_capture_entries(&similar.entry, &entry, similarity);
                persist_capture_entry(
                    server,
                    item.target_db,
                    item.named_project.as_deref(),
                    &merged,
                )?;
                let changed = with_foundry_store(server, item, |store| {
                    store
                        .archive_memory(&entry.id)
                        .map_err(|e| format!("Failed to archive merged memory {}: {e}", entry.id))
                })?;
                if changed {
                    updated += 1;
                }
                queue_capture_enrichment(
                    server,
                    item.target_db,
                    item.named_project.clone(),
                    &merged,
                    true,
                    item.job.target_agent_id.as_deref(),
                    Some(&item.path_prefix),
                );
                continue;
            }
        }

        if related.is_empty() {
            continue;
        }

        let metadata = merge_foundry_metadata(
            &entry.metadata,
            json!({
                "last_neighborhood_at": Utc::now().to_rfc3339(),
                "neighborhood_job_id": item.job.id,
                "related_entries": related,
            }),
        );

        let applied = with_foundry_store(server, item, |store| {
            update_entry_metadata(store, &entry, &metadata)
        })?;
        if applied {
            updated += 1;
        }
    }

    Ok(updated)
}

fn job_metadata_value<'a>(
    metadata: &'a serde_json::Value,
    key: &str,
) -> Option<&'a serde_json::Value> {
    metadata
        .get("job")
        .and_then(|job| job.get(key))
        .or_else(|| metadata.get(key))
}

fn job_metadata_usize(metadata: &serde_json::Value, key: &str, default: usize) -> usize {
    job_metadata_value(metadata, key)
        .and_then(|value| value.as_u64())
        .map(|value| value as usize)
        .unwrap_or(default)
}

fn job_metadata_string(metadata: &serde_json::Value, key: &str) -> Option<String> {
    job_metadata_value(metadata, key)
        .and_then(|value| value.as_str())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
}

fn recall_cache_queries_from_metadata(metadata: &serde_json::Value) -> Vec<String> {
    job_metadata_value(metadata, "queries")
        .and_then(|value| value.as_array())
        .into_iter()
        .flatten()
        .filter_map(|value| {
            value
                .as_str()
                .or_else(|| value.get("query").and_then(serde_json::Value::as_str))
        })
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
        .collect()
}

fn default_recall_cache_query(
    item: &FoundryMaintenanceItem,
    source_entries: &[MemoryEntry],
) -> String {
    let topics = dedup_strings(
        source_entries
            .iter()
            .flat_map(|entry| {
                std::iter::once(entry.topic.clone())
                    .chain(entry.keywords.iter().cloned())
                    .chain(entry.entities.iter().cloned())
            })
            .collect::<Vec<_>>(),
    );
    let topic_hint = topics.into_iter().take(8).collect::<Vec<_>>().join(" ");
    if !topic_hint.trim().is_empty() {
        return format!("{} {}", item.path_prefix, topic_hint);
    }

    let agent = item
        .job
        .target_agent_id
        .as_deref()
        .unwrap_or("agent")
        .trim();
    format!("durable context for {agent} {}", item.path_prefix)
}

fn row_string(row: &serde_json::Value, key: &str) -> String {
    row.get(key)
        .and_then(serde_json::Value::as_str)
        .unwrap_or("")
        .to_string()
}

fn build_recall_cache_text(query: &str, rows: &[serde_json::Value]) -> String {
    let mut lines = vec![format!("Recall rerank cache for query: {query}")];
    for (idx, row) in rows.iter().enumerate() {
        let id = value_id(row);
        let path = value_path(row);
        let topic = value_topic(row);
        let score = value_relevance(row);
        let summary = row_string(row, "summary");
        let text = row_string(row, "text");
        lines.push(format!(
            "{}. id={} score={:.3} topic={} path={}",
            idx + 1,
            if id.is_empty() { "unknown" } else { &id },
            score,
            if topic.is_empty() { "unknown" } else { &topic },
            if path.is_empty() { "unknown" } else { &path },
        ));
        lines.push(format!(
            "   {}",
            if summary.trim().is_empty() {
                text.chars().take(180).collect::<String>()
            } else {
                summary
            }
        ));
    }
    lines.join("\n")
}

async fn process_recall_rerank_cache_job(
    server: &MemoryServer,
    item: &FoundryMaintenanceItem,
) -> Result<usize, String> {
    let source_entries = with_foundry_store_read(server, item, |store| {
        let mut entries = Vec::new();
        for memory_id in &item.memory_ids {
            if let Some(entry) = store
                .get(memory_id)
                .map_err(|e| format!("Failed to load memory {memory_id} for recall cache: {e}"))?
            {
                entries.push(entry);
            }
        }
        Ok(entries)
    })?;

    let mut queries = recall_cache_queries_from_metadata(&item.job.metadata);
    if queries.is_empty() && !source_entries.is_empty() {
        queries.push(default_recall_cache_query(item, &source_entries));
    }
    queries = dedup_strings(queries);
    if queries.is_empty() {
        return Ok(0);
    }

    let top_k = job_metadata_usize(&item.job.metadata, "top_k", FOUNDRY_RECALL_RERANK_TOP_K).max(1);
    let candidate_multiplier = job_metadata_usize(
        &item.job.metadata,
        "candidate_multiplier",
        FOUNDRY_RECALL_RERANK_CANDIDATE_MULTIPLIER,
    )
    .max(1);
    let candidate_top_k = top_k.saturating_mul(candidate_multiplier);
    let path_prefix = job_metadata_string(&item.job.metadata, "path_prefix")
        .or_else(|| normalize_path_prefix_value(&item.path_prefix));
    let agent_role = job_metadata_string(&item.job.metadata, "agent_role");
    let project = job_metadata_string(&item.job.metadata, "project").or_else(|| {
        if item.named_project.is_some() {
            item.named_project.clone()
        } else {
            None
        }
    });
    let scope = if item.target_db == DbScope::Project {
        "project".to_string()
    } else {
        "global".to_string()
    };

    let mut updated = 0usize;
    for query in queries {
        let mut rows = search_memory_rows(
            server,
            SearchMemoryParams {
                query: query.clone(),
                query_vec: None,
                top_k: candidate_top_k,
                path_prefix: path_prefix.clone(),
                include_archived: false,
                candidates_per_channel: candidate_top_k.max(20),
                mmr_threshold: None,
                graph_expand_hops: 0,
                graph_relation_filter: None,
                weights: None,
                agent_role: agent_role.clone(),
                project: project.clone(),
                domain: None,
            },
        )
        .await?;
        if let Some(prefix) = path_prefix.as_deref() {
            rows.retain(|row| {
                let path = value_path(row);
                !path.is_empty() && path_is_within_prefix(&path, prefix)
            });
        }
        rows.retain(|row| {
            row.get("source").and_then(serde_json::Value::as_str)
                != Some(FOUNDRY_RECALL_RERANK_CACHE_SOURCE)
        });
        let reranked = rerank_rows(server, &query, rows, top_k).await;
        if reranked.is_empty() {
            continue;
        }

        let cache_seed = format!(
            "{}|{}|{}|{}",
            item.named_project.as_deref().unwrap_or("default"),
            item.path_prefix,
            top_k,
            query
        );
        let cache_id = format!("foundry:recall-cache:{}", stable_hash(&cache_seed));
        let cache_topic = sanitize_safe_path_name(&query)
            .chars()
            .take(64)
            .collect::<String>();
        let cache_path = format!(
            "{}/recall-cache/{}",
            item.path_prefix.trim_end_matches('/'),
            cache_topic
        );
        let result_ids = reranked.iter().map(value_id).collect::<Vec<_>>();
        let result_scores = reranked
            .iter()
            .map(|row| {
                json!({
                    "id": value_id(row),
                    "score": round3(value_relevance(row)),
                    "path": value_path(row),
                    "topic": value_topic(row),
                })
            })
            .collect::<Vec<_>>();
        let timestamp = Utc::now().to_rfc3339();
        let text = build_recall_cache_text(&query, &reranked);
        let metadata = crate::provenance::inject_provenance(
            server,
            json!({
                "query": query,
                "top_k": top_k,
                "candidate_multiplier": candidate_multiplier,
                "candidate_top_k": candidate_top_k,
                "source_memory_ids": item.memory_ids.clone(),
                "result_ids": result_ids.clone(),
                "result_scores": result_scores,
                "job_id": item.job.id.clone(),
                "path_prefix": item.path_prefix.clone(),
            }),
            "foundry_worker",
            "recall_rerank_cache",
            Some(scope.as_str()),
            item.target_db,
            json!({
                "agent_id": item.job.target_agent_id.clone(),
                "path_prefix": item.path_prefix.clone(),
            }),
        );

        let cache_entry = MemoryEntry {
            id: cache_id,
            path: cache_path,
            summary: text.chars().take(100).collect(),
            text,
            importance: 0.35,
            timestamp,
            category: "other".to_string(),
            topic: "recall_rerank_cache".to_string(),
            keywords: vec![
                "foundry".to_string(),
                "recall".to_string(),
                "rerank".to_string(),
                "cache".to_string(),
            ],
            persons: Vec::new(),
            entities: result_ids,
            location: item.path_prefix.clone(),
            source: FOUNDRY_RECALL_RERANK_CACHE_SOURCE.to_string(),
            scope: scope.clone(),
            archived: false,
            access_count: 0,
            last_access: None,
            revision: 1,
            metadata,
            vector: None,
            retention_policy: Some("ephemeral".to_string()),
            domain: None,
        };

        persist_capture_entry(
            server,
            item.target_db,
            item.named_project.as_deref(),
            &cache_entry,
        )?;
        queue_capture_enrichment(
            server,
            item.target_db,
            item.named_project.clone(),
            &cache_entry,
            false,
            item.job.target_agent_id.as_deref(),
            Some(&item.path_prefix),
        );
        updated += 1;
    }

    Ok(updated)
}

/// Minimum coherent batch size — fewer than this and a topic/entity bucket is
/// considered too thin to justify an LLM round-trip (avoids stitched hallucinations).
const FOUNDRY_DISTILL_MIN_BATCH: usize = 3;

/// Group memories by a coherence key (topic when available, otherwise the most
/// frequent shared entity). Buckets smaller than [`FOUNDRY_DISTILL_MIN_BATCH`]
/// are dropped — they would otherwise force the LLM to stitch unrelated facts
/// into a single false summary (the v0.15.x "缝合怪" regression).
pub(super) fn coherent_distill_buckets(
    entries: Vec<MemoryEntry>,
) -> Vec<(String, Vec<MemoryEntry>)> {
    let mut buckets: HashMap<String, Vec<MemoryEntry>> = HashMap::new();
    for entry in entries {
        let Some(key) = coherence_bucket_key(&entry.topic, &entry.entities) else {
            // No coherence signal — skip rather than risk a stitched distill.
            continue;
        };
        let namespace = scheduled_distill_path_prefix(&entry.path);
        buckets
            .entry(format!("{namespace}#{key}"))
            .or_default()
            .push(entry);
    }
    // PR #50 review: `coherent_distill_buckets` already drops buckets
    // whose `distill_quality_flags` are non-empty, so by construction
    // the selected bucket has clean flags. We re-derive them here for
    // two reasons:
    //   (1) they are stored in the distill memory's metadata for audit;
    //   (2) defense in depth: if the filter contract drifts in future
    //       refactors, we still surface a structured skip reason instead
    //       of silently writing a low-quality distill.
    buckets
        .into_iter()
        .filter(|(_, group)| distill_quality_flags(group).is_empty())
        .collect()
}

async fn process_memory_distill_job(
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
            eprintln!(
                "[foundry/distill] preferred_coherence_key={preferred_key:?} not found in buckets ({} buckets); falling back to largest",
                buckets.len()
            );
            buckets.sort_by(|a, b| b.1.len().cmp(&a.1.len()));
            buckets.into_iter().next().expect("non-empty")
        }
    } else {
        buckets.sort_by(|a, b| b.1.len().cmp(&a.1.len()));
        buckets.into_iter().next().expect("non-empty")
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
    let distill_root = build_foundry_distill_root(agent_id);
    let timestamp = Utc::now().to_rfc3339();
    let memory_id = uuid::Uuid::new_v4().to_string();
    let metadata = crate::provenance::inject_provenance(
        server,
        json!({
            "source_memory_ids": source_entries.iter().map(|entry| entry.id.clone()).collect::<Vec<_>>(),
            "source_path_prefix": item.path_prefix,
            "namespace_key": namespace_key,
            "coherence_key": coherence_key,
            "bucket_key": bucket_key,
            "quality_flags": quality_flags,
            "job_id": item.job.id,
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

    let distill_entry = MemoryEntry {
        id: memory_id.clone(),
        path: format!("{distill_root}/{}", Utc::now().format("%Y%m%dT%H%M%S")),
        summary: distill_text.chars().take(100).collect(),
        text: distill_text,
        importance: 0.75,
        timestamp,
        category: "other".to_string(),
        topic: "foundry_distill".to_string(),
        keywords: dedup_strings({
            let mut kws = vec!["foundry".to_string(), "distill".to_string()];
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
        location: item.path_prefix.clone(),
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
        retention_policy: None,
        domain: None,
    };

    with_foundry_store(server, item, |store| {
        store
            .upsert(&distill_entry)
            .map_err(|e| format!("Failed to save foundry distill memory: {e}"))
    })?;
    queue_capture_enrichment(
        server,
        item.target_db,
        item.named_project.clone(),
        &distill_entry,
        false,
        None,
        None,
    );

    Ok(DistillOutcome::Wrote(memory_id))
}

fn process_forget_sweep_job(
    server: &MemoryServer,
    item: &FoundryMaintenanceItem,
) -> Result<usize, String> {
    let agent_id = item
        .job
        .target_agent_id
        .as_deref()
        .unwrap_or("unknown-agent");
    let distill_root = build_foundry_distill_root(agent_id);
    let distill_entries = with_foundry_store_read(server, item, |store| {
        store
            .list_by_path(&distill_root, FOUNDRY_DISTILL_KEEP + 12, false)
            .map_err(|e| format!("Failed to list foundry distill memories: {e}"))
    })?;

    let mut distill_entries = distill_entries
        .into_iter()
        .filter(|entry| entry.source == FOUNDRY_DISTILL_SOURCE)
        .collect::<Vec<_>>();
    distill_entries.sort_by(|a, b| {
        b.timestamp
            .cmp(&a.timestamp)
            .then_with(|| b.path.cmp(&a.path))
            .then_with(|| b.id.cmp(&a.id))
    });

    let stale_ids = distill_entries
        .into_iter()
        .skip(FOUNDRY_DISTILL_KEEP)
        .map(|entry| entry.id)
        .collect::<Vec<_>>();

    let mut archived = 0usize;
    for stale_id in stale_ids {
        let changed = with_foundry_store(server, item, |store| {
            store
                .archive_memory(&stale_id)
                .map_err(|e| format!("Failed to archive stale foundry distill {}: {e}", stale_id))
        })?;
        if changed {
            archived += 1;
        }
    }

    Ok(archived)
}

async fn handle_foundry_maintenance_item(
    server: &MemoryServer,
    item: &FoundryMaintenanceItem,
) -> Result<(memory_core::FoundryJobStatus, Option<String>), String> {
    let worker = foundry_worker_name(&item.job.kind);
    let event_hash = build_foundry_event_hash(server, item)?;
    let claimed = with_foundry_store(server, item, |store| {
        store
            .try_claim_event(&event_hash, &item.job.id, worker)
            .map_err(|e| format!("Failed to claim foundry job {}: {e}", item.job.id))
    })?;

    if !claimed {
        return Ok((
            memory_core::FoundryJobStatus::Skipped,
            Some("event_already_claimed".to_string()),
        ));
    }

    let result = match item.job.kind {
        memory_core::FoundryJobKind::MemoryNeighborhood => {
            process_memory_neighborhood_job(server, item)
                .await
                .map(|_| (memory_core::FoundryJobStatus::Completed, None))
        }
        memory_core::FoundryJobKind::RecallRerankCache => {
            process_recall_rerank_cache_job(server, item)
                .await
                .map(|_| (memory_core::FoundryJobStatus::Completed, None))
        }
        memory_core::FoundryJobKind::MemoryDistill => process_memory_distill_job(server, item)
            .await
            .map(|outcome| match outcome {
                DistillOutcome::Wrote(_) => (memory_core::FoundryJobStatus::Completed, None),
                DistillOutcome::Skipped(reason) => {
                    (memory_core::FoundryJobStatus::Skipped, Some(reason))
                }
            }),
        memory_core::FoundryJobKind::ForgetSweep => process_forget_sweep_job(server, item)
            .map(|_| (memory_core::FoundryJobStatus::Completed, None)),
        _ => Ok((
            memory_core::FoundryJobStatus::Skipped,
            Some("unknown_job_kind".to_string()),
        )),
    };

    if let Err(err) = &result {
        let _ = with_foundry_store(server, item, |store| {
            store
                .release_event_claim(&event_hash, worker)
                .map_err(|e| format!("Failed to release foundry job claim {}: {e}", item.job.id))
        });
        return Err(err.clone());
    }

    result
}

pub(crate) async fn run_foundry_maintenance_worker(
    server: MemoryServer,
    mut rx: mpsc::Receiver<FoundryMaintenanceItem>,
) {
    while let Some(item) = rx.recv().await {
        server.foundry_stats.queued.fetch_sub(1, Ordering::Relaxed);
        server.foundry_stats.running.fetch_add(1, Ordering::Relaxed);

        let result = handle_foundry_maintenance_item(&server, &item).await;

        server.foundry_stats.running.fetch_sub(1, Ordering::Relaxed);

        // Branch #5 + PR-C: capture a structured reason for non-completed
        // terminal transitions so `tachi doctor --jobs` and post-mortems
        // can surface *why* a job skipped/failed instead of just the bare
        // status. The reason is now returned in-band by
        // `handle_foundry_maintenance_item` (no global stash, no draining).
        let (status_str, reason): (&str, Option<String>) =
            match &result {
                Ok((memory_core::FoundryJobStatus::Skipped, reason)) => (
                    "skipped",
                    Some(reason.clone().unwrap_or_else(|| {
                        "worker reported no-op (no qualifying inputs)".to_string()
                    })),
                ),
                Ok((_, _)) => ("completed", None),
                Err(e) => ("failed", Some(e.clone())),
            };
        let _ = with_foundry_store(&server, &item, |store| {
            memory_core::update_foundry_job_status_with_reason(
                store.connection(),
                &item.job.id,
                status_str,
                reason.as_deref(),
            )
            .map_err(|e| format!("update foundry job status: {e}"))
        });

        match result {
            Ok((memory_core::FoundryJobStatus::Skipped, _)) => {
                server.foundry_stats.skipped.fetch_add(1, Ordering::Relaxed);
            }
            Ok(_) => {
                server
                    .foundry_stats
                    .completed
                    .fetch_add(1, Ordering::Relaxed);
            }
            Err(err) => {
                eprintln!("[foundry-worker] job {} failed: {err}", item.job.id);
                server.foundry_stats.failed.fetch_add(1, Ordering::Relaxed);
            }
        }
    }

    eprintln!("[foundry-worker] channel closed, worker exiting");
}

fn build_distill_input(entries: &[MemoryEntry]) -> String {
    entries
        .iter()
        .enumerate()
        .map(|(idx, entry)| {
            let summary = if entry.summary.trim().is_empty() {
                entry.text.chars().take(180).collect::<String>()
            } else {
                entry.summary.clone()
            };
            format!(
                "Memory {} | topic={} | importance={:.2}\nSummary: {}\nText: {}",
                idx + 1,
                if entry.topic.is_empty() {
                    "unknown"
                } else {
                    &entry.topic
                },
                entry.importance,
                summary,
                entry.text.chars().take(320).collect::<String>()
            )
        })
        .collect::<Vec<_>>()
        .join("\n\n")
}
