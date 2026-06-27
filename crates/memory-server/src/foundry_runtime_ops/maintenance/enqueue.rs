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

fn foundry_job_lane(kind: &memory_core::FoundryJobKind) -> memory_core::FoundryModelLane {
    match kind {
        // PR #2 / Q3: `MemoryNeighborhood` is pure vector-math + DB work
        // and never calls a chat model. The legacy `Maintenance` lane has
        // been removed (deserialize-aliased to `Reasoning` for back-compat).
        memory_core::FoundryJobKind::MemoryNeighborhood => memory_core::FoundryModelLane::Reasoning,
        memory_core::FoundryJobKind::RecallRerankCache => memory_core::FoundryModelLane::Rerank,
        memory_core::FoundryJobKind::MemoryDistill => memory_core::FoundryModelLane::Distill,
        memory_core::FoundryJobKind::ForgetSweep => memory_core::FoundryModelLane::Distill,
        _ => memory_core::FoundryModelLane::Reasoning,
    }
}

pub(super) fn foundry_worker_name(kind: &memory_core::FoundryJobKind) -> &'static str {
    match kind {
        memory_core::FoundryJobKind::MemoryNeighborhood => "foundry_neighborhood",
        memory_core::FoundryJobKind::RecallRerankCache => "foundry_recall_rerank_cache",
        memory_core::FoundryJobKind::MemoryDistill => "foundry_distill",
        memory_core::FoundryJobKind::ForgetSweep => "foundry_forget",
        _ => "foundry",
    }
}

pub(in crate::foundry_runtime_ops) fn foundry_job_label(
    kind: &memory_core::FoundryJobKind,
) -> &'static str {
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
    let mut sorted_memory_ids = memory_ids.to_vec();
    sorted_memory_ids.sort();
    sorted_memory_ids.dedup();
    memory_core::FoundryJobSpec {
        id: format!("foundry-job:{}", uuid::Uuid::new_v4()),
        kind: kind.clone(),
        lane: foundry_job_lane(&kind),
        status: memory_core::FoundryJobStatus::Queued,
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
) -> Result<Vec<memory_core::FoundryJobSpec>, String> {
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
/// [`memory_core::FoundryJobKind::MemoryDistill`]. Distill is now handled
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
) -> Vec<memory_core::FoundryJobSpec> {
    let mut specs = vec![build_foundry_maintenance_job(
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
    )];

    if durable_recall_cache_enabled() {
        specs.push(build_foundry_maintenance_job(
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
        ));
    }

    // NOTE: Phase 1 — MemoryDistill is no longer enqueued from capture.
    // The daily batch distill (`run_daily_batch_distill`, invoked from
    // the bootstrap scheduler) replaces the per-capture distill job.
    // We still enqueue ForgetSweep so per-capture sweeps continue to
    // garbage-collect stale distill memories.
    specs.push(build_foundry_maintenance_job(
        server,
        memory_core::FoundryJobKind::ForgetSweep,
        agent_id,
        path_prefix,
        memory_ids,
        json!({
            "kind": "forget_sweep",
            "keep_latest": FOUNDRY_DISTILL_KEEP,
        }),
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
) -> Result<Vec<memory_core::FoundryJobSpec>, String> {
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
    )
}
