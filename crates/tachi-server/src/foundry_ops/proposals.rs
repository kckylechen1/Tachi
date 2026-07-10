use crate::server_state::{DbScope, MemoryServer};
use crate::tool_params::SynthesizeAgentEvolutionParams;
use crate::utils::sanitize_safe_path_name;
use serde_json::json;

use super::AGENT_EVOLUTION_PROPOSAL_SOURCE;

pub(super) fn proposal_root(agent_id: &str) -> String {
    format!(
        "/foundry/agents/{}/proposals",
        sanitize_safe_path_name(agent_id)
    )
}

pub(super) fn agent_evolution_proposal_identity(
    params: &SynthesizeAgentEvolutionParams,
    job: &memcore::FoundryJobSpec,
) -> (String, String) {
    let job_key = sanitize_safe_path_name(&job.id);
    (
        format!("agent-evolution-{job_key}"),
        format!("{}/{}", proposal_root(&params.agent_id), job_key),
    )
}

pub(super) fn resolve_foundry_write_scope(server: &MemoryServer) -> (DbScope, Option<String>) {
    server.resolve_write_scope("project")
}

pub(super) fn persist_agent_evolution_proposal(
    server: &MemoryServer,
    params: &SynthesizeAgentEvolutionParams,
    job: &memcore::FoundryJobSpec,
    synthesis: &memcore::AgentEvolutionSynthesis,
) -> Result<(String, String, DbScope), String> {
    let (proposal_id, path) = agent_evolution_proposal_identity(params, job);
    let summary = if synthesis.summary.trim().is_empty() {
        synthesis
            .no_change_reason
            .clone()
            .unwrap_or_else(|| "agent evolution proposal".to_string())
    } else {
        synthesis.summary.clone()
    };
    let text = serde_json::to_string_pretty(synthesis)
        .map_err(|e| format!("Failed to serialize agent evolution synthesis: {e}"))?;
    let (target_db, _warning) = resolve_foundry_write_scope(server);
    let metadata = crate::provenance::inject_provenance(
        server,
        json!({
            "job": job,
            "agent_id": params.agent_id,
            "display_name": params.display_name,
            "goals": params.goals,
            "document_count": params.documents.len() + params.document_paths.len(),
            "evidence_count": params.evidence.len()
                + params.evidence_paths.len()
                + params.memory_queries.len(),
            "proposal_count": synthesis.proposals.len(),
            "status": "proposed",
        }),
        "synthesize_agent_evolution",
        "agent_evolution_proposal",
        Some(target_db.as_str()),
        target_db,
        json!({
            "agent_id": params.agent_id,
            "proposal_path": path,
        }),
    );
    let derived_id = server.with_store_for_scope(target_db, |store| {
        store
            .save_derived_with_id(
                &proposal_id,
                &text,
                &path,
                &summary,
                0.8,
                AGENT_EVOLUTION_PROPOSAL_SOURCE,
                target_db.as_str(),
                &metadata,
            )
            .map(|()| proposal_id.clone())
            .map_err(|e| format!("Failed to save agent evolution proposal: {e}"))
    })?;
    Ok((derived_id, path, target_db))
}
