use crate::server_state::{DbScope, MemoryServer};
use chrono::Utc;
use serde_json::json;

use super::super::recall_cache::durable_recall_cache_enabled;
use super::super::{
    FoundryMaintenanceItem, FOUNDRY_DISTILL_KEEP, FOUNDRY_RECALL_RERANK_CANDIDATE_MULTIPLIER,
    FOUNDRY_RECALL_RERANK_TOP_K, FOUNDRY_RELATED_LIMIT,
};

fn foundry_requested_by(server: &MemoryServer) -> Option<String> {
    server
        .agent_runtime_read()
        .agent_profile
        .as_ref()
        .map(|profile| profile.agent_id.clone())
}

fn foundry_job_lane(kind: &memcore::FoundryJobKind) -> memcore::FoundryModelLane {
    match kind {
        // PR #2 / Q3: `MemoryNeighborhood` is pure vector-math + DB work
        // and never calls a chat model. The legacy `Maintenance` lane has
        // been removed (deserialize-aliased to `Reasoning` for back-compat).
        memcore::FoundryJobKind::MemoryNeighborhood => memcore::FoundryModelLane::Reasoning,
        memcore::FoundryJobKind::RecallRerankCache => memcore::FoundryModelLane::Rerank,
        memcore::FoundryJobKind::MemoryDistill => memcore::FoundryModelLane::Distill,
        memcore::FoundryJobKind::ForgetSweep => memcore::FoundryModelLane::Distill,
        _ => memcore::FoundryModelLane::Reasoning,
    }
}

pub(super) fn foundry_worker_name(kind: &memcore::FoundryJobKind) -> &'static str {
    match kind {
        memcore::FoundryJobKind::MemoryNeighborhood => "foundry_neighborhood",
        memcore::FoundryJobKind::RecallRerankCache => "foundry_recall_rerank_cache",
        memcore::FoundryJobKind::MemoryDistill => "foundry_distill",
        memcore::FoundryJobKind::ForgetSweep => "foundry_forget",
        _ => "foundry",
    }
}

pub(in crate::foundry_runtime_ops) fn foundry_job_label(
    kind: &memcore::FoundryJobKind,
) -> &'static str {
    match kind {
        memcore::FoundryJobKind::MemoryNeighborhood => "memory_neighborhood",
        memcore::FoundryJobKind::RecallRerankCache => "recall_rerank_cache",
        memcore::FoundryJobKind::MemoryDistill => "memory_distill",
        memcore::FoundryJobKind::ForgetSweep => "forget_sweep",
        memcore::FoundryJobKind::SessionIngest => "session_ingest",
        memcore::FoundryJobKind::MemoryEnrichment => "memory_enrichment",
        memcore::FoundryJobKind::SkillEvolution => "skill_evolution",
        memcore::FoundryJobKind::AgentEvolution => "agent_evolution",
        memcore::FoundryJobKind::ProfileProjection => "profile_projection",
    }
}

fn build_foundry_maintenance_job(
    server: &MemoryServer,
    kind: memcore::FoundryJobKind,
    agent_id: &str,
    path_prefix: &str,
    memory_ids: &[String],
    metadata: serde_json::Value,
    deterministic_key: Option<&str>,
) -> memcore::FoundryJobSpec {
    let mut sorted_memory_ids = memory_ids.to_vec();
    sorted_memory_ids.sort();
    sorted_memory_ids.dedup();
    memcore::FoundryJobSpec {
        id: deterministic_key.map_or_else(
            || format!("foundry-job:{}", uuid::Uuid::new_v4()),
            |key| {
                format!(
                    "foundry-job:{}",
                    uuid::Uuid::new_v5(
                        &uuid::Uuid::NAMESPACE_OID,
                        format!("{key}:{}", foundry_job_label(&kind)).as_bytes()
                    )
                )
            },
        ),
        kind: kind.clone(),
        lane: foundry_job_lane(&kind),
        status: memcore::FoundryJobStatus::Queued,
        target_agent_id: Some(agent_id.to_string()),
        requested_by: foundry_requested_by(server),
        created_at: Utc::now().to_rfc3339(),
        evidence_count: sorted_memory_ids.len(),
        goal_count: 1,
        metadata: json!({
            "path_prefix": path_prefix,
            "memory_ids": sorted_memory_ids,
            "job": metadata,
        }),
    }
}

pub(in crate::foundry_runtime_ops) fn enqueue_capture_maintenance_jobs(
    server: &MemoryServer,
    target_db: DbScope,
    named_project: Option<String>,
    db_path: Option<std::path::PathBuf>,
    agent_id: &str,
    path_prefix: &str,
    memory_ids: &[String],
    merged_count: usize,
    duplicate_count: usize,
    deterministic_key: Option<&str>,
) -> Result<Vec<memcore::FoundryJobSpec>, String> {
    if memory_ids.is_empty() {
        return Ok(Vec::new());
    }

    let specs = capture_maintenance_specs(
        server,
        agent_id,
        path_prefix,
        memory_ids,
        merged_count,
        duplicate_count,
        deterministic_key,
    );

    for spec in &specs {
        server.enqueue_foundry_job(FoundryMaintenanceItem {
            job: spec.clone(),
            target_db,
            named_project: named_project.clone(),
            db_path: db_path.clone(),
            path_prefix: path_prefix.to_string(),
            memory_ids: memory_ids.to_vec(),
            counted_queue_slot: true,
        })?;
    }

    Ok(specs)
}

/// Build the (Phase 1) per-capture maintenance specs. Pulled out of
/// [`enqueue_capture_maintenance_jobs`] so the kind-set can be asserted in
/// unit tests without spinning up a MemoryServer.
///
/// Phase 1 invariant: this list MUST NOT include
/// [`memcore::FoundryJobKind::MemoryDistill`]. Distill is now handled
/// exclusively by the daily batch scheduler
/// (`run_daily_batch_distill`); per-capture distill jobs would defeat the
/// batching that keeps Claude CLI invocations cheap.
pub(in crate::foundry_runtime_ops) fn capture_maintenance_specs(
    server: &MemoryServer,
    agent_id: &str,
    path_prefix: &str,
    memory_ids: &[String],
    merged_count: usize,
    duplicate_count: usize,
    deterministic_key: Option<&str>,
) -> Vec<memcore::FoundryJobSpec> {
    let mut specs = vec![build_foundry_maintenance_job(
        server,
        memcore::FoundryJobKind::MemoryNeighborhood,
        agent_id,
        path_prefix,
        memory_ids,
        json!({
            "kind": "memory_neighborhood",
            "neighbor_limit": FOUNDRY_RELATED_LIMIT,
            "merged_count": merged_count,
            "duplicate_count": duplicate_count,
        }),
        deterministic_key,
    )];

    if durable_recall_cache_enabled() {
        specs.push(build_foundry_maintenance_job(
            server,
            memcore::FoundryJobKind::RecallRerankCache,
            agent_id,
            path_prefix,
            memory_ids,
            json!({
                "kind": "recall_rerank_cache",
                "top_k": FOUNDRY_RECALL_RERANK_TOP_K,
                "candidate_multiplier": FOUNDRY_RECALL_RERANK_CANDIDATE_MULTIPLIER,
            }),
            deterministic_key,
        ));
    }

    // NOTE: Phase 1 — MemoryDistill is no longer enqueued from capture.
    // The daily batch distill (`run_daily_batch_distill`, invoked from
    // the bootstrap scheduler) replaces the per-capture distill job.
    // We still enqueue ForgetSweep so per-capture sweeps continue to
    // garbage-collect stale distill memories.
    specs.push(build_foundry_maintenance_job(
        server,
        memcore::FoundryJobKind::ForgetSweep,
        agent_id,
        path_prefix,
        memory_ids,
        json!({
            "kind": "forget_sweep",
            "keep_latest": FOUNDRY_DISTILL_KEEP,
        }),
        deterministic_key,
    ));

    specs
}

pub(crate) fn enqueue_foundry_capture_maintenance(
    server: &MemoryServer,
    target_db: DbScope,
    named_project: Option<String>,
    db_path: Option<std::path::PathBuf>,
    agent_id: &str,
    path_prefix: &str,
    memory_ids: &[String],
) -> Result<Vec<memcore::FoundryJobSpec>, String> {
    enqueue_capture_maintenance_jobs(
        server,
        target_db,
        named_project,
        db_path,
        agent_id,
        path_prefix,
        memory_ids,
        0,
        0,
        None,
    )
}
