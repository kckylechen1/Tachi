use crate::server_state::MemoryServer;
#[cfg(test)]
use crate::tool_params::AgentEvolutionDocumentParams;
use crate::tool_params::SynthesizeAgentEvolutionParams;
use serde_json::json;

mod job;
mod paths;
mod proposals;
mod synthesis;

#[cfg(test)]
mod tests;

pub(super) const AGENT_EVOLUTION_PROPOSAL_SOURCE: &str = "foundry_agent_evolution";

#[cfg(test)]
use job::agent_evolution_job_id;
use job::build_foundry_job;
#[cfg(test)]
use proposals::{agent_evolution_proposal_identity, persist_agent_evolution_proposal};
#[cfg(test)]
use proposals::proposal_root;
#[cfg(test)]
use synthesis::parse_synthesis_response;
use synthesis::{
    build_documents, build_evidence, build_synthesis_payload, has_evolution_inputs,
    run_agent_evolution_synthesis,
};

pub(crate) async fn handle_synthesize_agent_evolution(
    server: &MemoryServer,
    params: SynthesizeAgentEvolutionParams,
) -> Result<String, String> {
    if !has_evolution_inputs(&params) {
        return Err(
            "synthesize_agent_evolution requires at least one document or evidence item"
                .to_string(),
        );
    }

    let documents = build_documents(&params)?;
    let evidence = build_evidence(server, &params).await?;
    let job = build_foundry_job(server, &params);
    let payload = build_synthesis_payload(&params, &documents, &evidence);

    if params.dry_run {
        return serde_json::to_string(&json!({
            "status": "dry_run",
            "job": job,
            "request": payload,
        }))
        .map_err(|e| format!("Failed to serialize dry-run response: {e}"));
    }

    let (synthesis, proposal_id, proposal_path, target_db) =
        run_agent_evolution_synthesis(server, &params, &job).await?;

    serde_json::to_string(&json!({
        "status": "completed",
        "job": job,
        "proposal_id": proposal_id,
        "proposal_path": proposal_path,
        "db": target_db.as_str(),
        "synthesis": synthesis,
    }))
    .map_err(|e| format!("Failed to serialize synthesis response: {e}"))
}
