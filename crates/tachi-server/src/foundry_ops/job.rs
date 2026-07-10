use crate::server_state::MemoryServer;
use crate::tool_params::SynthesizeAgentEvolutionParams;
use chrono::Utc;
use serde_json::json;

fn agent_evolution_job_fingerprint(params: &SynthesizeAgentEvolutionParams) -> String {
    let document_paths: Vec<String> = params
        .document_paths
        .iter()
        .map(|item| format!("{}:{}", item.kind, item.path))
        .collect();
    let evidence_paths: Vec<String> = params
        .evidence_paths
        .iter()
        .map(|item| format!("{}:{}", item.kind, item.path))
        .collect();
    let documents: Vec<String> = params
        .documents
        .iter()
        .map(|item| {
            format!(
                "{}:{}:{}",
                item.kind,
                item.path.as_deref().unwrap_or(""),
                item.content
            )
        })
        .collect();
    let evidence: Vec<String> = params
        .evidence
        .iter()
        .map(|item| {
            format!(
                "{}:{}:{}",
                item.kind,
                item.source_ref.as_deref().unwrap_or(""),
                item.content
            )
        })
        .collect();
    let memory_queries: Vec<String> = params
        .memory_queries
        .iter()
        .map(|item| {
            format!(
                "{}:{}:{:?}",
                item.query,
                item.path_prefix.as_deref().unwrap_or(""),
                item.project
            )
        })
        .collect();
    let payload = format!(
        "agent_id={}|display_name={}|goals={goals:?}|document_paths={document_paths:?}|evidence_paths={evidence_paths:?}|documents={documents:?}|evidence={evidence:?}|memory_queries={memory_queries:?}",
        params.agent_id,
        params.display_name.as_deref().unwrap_or(""),
        goals = params.goals,
    );
    crate::utils::stable_hash(&payload)
}

pub(super) fn agent_evolution_job_id(params: &SynthesizeAgentEvolutionParams) -> String {
    format!(
        "foundry-job:agent-evolution:{}",
        agent_evolution_job_fingerprint(params)
    )
}

pub(super) fn build_foundry_job(
    server: &MemoryServer,
    params: &SynthesizeAgentEvolutionParams,
) -> memcore::FoundryJobSpec {
    let requested_by = server
        .agent_runtime_read()
        .agent_profile
        .as_ref()
        .map(|profile| profile.agent_id.clone());

    memcore::FoundryJobSpec {
        id: agent_evolution_job_id(params),
        kind: memcore::FoundryJobKind::AgentEvolution,
        lane: memcore::FoundryModelLane::Reasoning,
        status: memcore::FoundryJobStatus::Planned,
        target_agent_id: Some(params.agent_id.clone()),
        requested_by,
        created_at: Utc::now().to_rfc3339(),
        evidence_count: params.evidence.len()
            + params.evidence_paths.len()
            + params.memory_queries.len(),
        goal_count: params.goals.len(),
        metadata: json!({
            "display_name": params.display_name,
            "document_count": params.documents.len() + params.document_paths.len(),
            "memory_query_count": params.memory_queries.len(),
        }),
    }
}
