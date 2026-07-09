use crate::server_state::MemoryServer;
#[cfg(test)]
use crate::tool_params::AgentEvolutionDocumentParams;
use crate::tool_params::{
    ListAgentEvolutionProposalsParams, ProjectAgentProfileParams,
    ReviewAgentEvolutionProposalParams, SynthesizeAgentEvolutionParams,
};
use chrono::Utc;
use serde_json::json;

mod job;
mod paths;
mod projection;
mod proposals;
mod synthesis;

#[cfg(test)]
mod tests;

pub(super) const AGENT_EVOLUTION_PROPOSAL_SOURCE: &str = "foundry_agent_evolution";
pub(super) const FOUNDRY_JOB_NAMESPACE: &str = "foundry_job";
pub(super) const FOUNDRY_PROPOSAL_REVIEW_NAMESPACE: &str = "foundry_proposal_review";

#[cfg(test)]
use job::{agent_evolution_job_id, foundry_running_job_is_stale};
use job::{
    build_foundry_job, foundry_job_is_active, foundry_job_status, load_foundry_job_state,
    save_foundry_job_state, try_claim_foundry_job,
};
#[cfg(test)]
use paths::resolve_projection_write_path;
use paths::write_document_if_requested;
use projection::{apply_markdown_section_update, proposal_targets_document};
#[cfg(test)]
use proposals::{agent_evolution_proposal_identity, persist_agent_evolution_proposal};
use proposals::{
    load_agent_proposal_rows, load_review_state, mark_proposal_applied, parse_review_status,
    proposal_record_from_row, proposal_root, resolve_foundry_write_scope, review_status_or_default,
};
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

pub(crate) async fn handle_queue_agent_evolution(
    server: &MemoryServer,
    params: SynthesizeAgentEvolutionParams,
) -> Result<String, String> {
    if !has_evolution_inputs(&params) {
        return Err(
            "queue_agent_evolution requires at least one document or evidence item".to_string(),
        );
    }

    let job = build_foundry_job(server, &params);
    if let Ok(Some(state)) = load_foundry_job_state(server, &job.id) {
        if foundry_job_is_active(&state) {
            return serde_json::to_string(&json!({
                "status": "deduped",
                "job": job,
                "existing_status": foundry_job_status(&state),
            }))
            .map_err(|e| format!("Failed to serialize dedupe response: {e}"));
        }
        let status = foundry_job_status(&state);
        if status == "completed" || status == "failed" {
            return serde_json::to_string(&json!({
                "status": "deduped",
                "job": job,
                "existing_status": status,
            }))
            .map_err(|e| format!("Failed to serialize dedupe response: {e}"));
        }
    }

    let mut job = job;
    job.status = memcore::FoundryJobStatus::Queued;
    save_foundry_job_state(server, &job, "queued", json!({}))?;

    let server = server.clone();
    let params_for_task = params;
    let job_for_task = job.clone();
    tokio::spawn(async move {
        if !try_claim_foundry_job(&server, &job_for_task) {
            eprintln!(
                "[foundry-agent-evolution] job {} already claimed or finished; skipping synthesis",
                job_for_task.id
            );
            return;
        }

        match run_agent_evolution_synthesis(&server, &params_for_task, &job_for_task).await {
            Ok((synthesis, proposal_id, proposal_path, target_db)) => {
                let _ = save_foundry_job_state(
                    &server,
                    &job_for_task,
                    "completed",
                    json!({
                        "proposal_id": proposal_id,
                        "proposal_path": proposal_path,
                        "db": target_db.as_str(),
                        "proposal_count": synthesis.proposals.len(),
                    }),
                );
            }
            Err(err) => {
                eprintln!(
                    "[foundry-agent-evolution] job {} failed: {err}",
                    job_for_task.id
                );
                let _ = save_foundry_job_state(
                    &server,
                    &job_for_task,
                    "failed",
                    json!({ "error": err }),
                );
            }
        }
    });

    serde_json::to_string(&json!({
        "status": "queued",
        "job": job,
    }))
    .map_err(|e| format!("Failed to serialize queue response: {e}"))
}

pub(crate) async fn handle_list_agent_evolution_proposals(
    server: &MemoryServer,
    params: ListAgentEvolutionProposalsParams,
) -> Result<String, String> {
    let root = proposal_root(&params.agent_id);
    let limit = params.limit.max(1).min(100);
    let (target_db, _warning) = resolve_foundry_write_scope(server);
    let rows = server.with_store_for_scope_read(target_db, |store| {
        store
            .list_derived_by_source(AGENT_EVOLUTION_PROPOSAL_SOURCE, &root, limit)
            .map_err(|e| format!("Failed to list agent evolution proposals: {e}"))
    })?;

    let desired_status = params
        .status
        .as_deref()
        .map(|status| review_status_or_default(Some(status)));
    let mut proposals = Vec::new();
    for row in rows {
        let proposal_id = row
            .get("id")
            .and_then(|value| value.as_str())
            .unwrap_or("")
            .to_string();
        let review = load_review_state(server, &proposal_id)?;
        let record = proposal_record_from_row(&row, review);
        let status = record
            .get("status")
            .and_then(|value| value.as_str())
            .unwrap_or("proposed");
        if let Some(ref desired) = desired_status {
            if status != desired {
                continue;
            }
        }
        proposals.push(record);
    }

    serde_json::to_string(&json!({
        "agent_id": params.agent_id,
        "count": proposals.len(),
        "proposals": proposals,
    }))
    .map_err(|e| format!("Failed to serialize proposal list: {e}"))
}

pub(crate) async fn handle_review_agent_evolution_proposal(
    server: &MemoryServer,
    params: ReviewAgentEvolutionProposalParams,
) -> Result<String, String> {
    let status = parse_review_status(&params.status)?;
    let reviewer = server
        .agent_runtime_read()
        .agent_profile
        .as_ref()
        .map(|profile| profile.agent_id.clone());
    let review = json!({
        "status": status,
        "note": params.note,
        "reviewed_at": Utc::now().to_rfc3339(),
        "reviewed_by": reviewer,
    });
    let value_json = serde_json::to_string(&review)
        .map_err(|e| format!("Failed to serialize proposal review: {e}"))?;
    server.with_global_store(|store| {
        store
            .set_state(
                FOUNDRY_PROPOSAL_REVIEW_NAMESPACE,
                &params.proposal_id,
                &value_json,
            )
            .map_err(|e| format!("Failed to persist proposal review: {e}"))?;
        Ok(())
    })?;

    serde_json::to_string(&json!({
        "proposal_id": params.proposal_id,
        "review": review,
    }))
    .map_err(|e| format!("Failed to serialize proposal review response: {e}"))
}

pub(crate) async fn handle_project_agent_profile(
    server: &MemoryServer,
    params: ProjectAgentProfileParams,
) -> Result<String, String> {
    if params.documents.is_empty() {
        return Err("project_agent_profile requires at least one document".to_string());
    }

    let proposal_id_filter = params
        .proposal_ids
        .iter()
        .map(|id| id.trim().to_string())
        .filter(|id| !id.is_empty())
        .collect::<std::collections::HashSet<_>>();
    let rows = load_agent_proposal_rows(server, &params.agent_id, 100)?;

    let mut proposal_records = Vec::<serde_json::Value>::new();
    for row in rows {
        let proposal_id = row
            .get("id")
            .and_then(|value| value.as_str())
            .unwrap_or("")
            .to_string();
        if !proposal_id_filter.is_empty() && !proposal_id_filter.contains(&proposal_id) {
            continue;
        }
        let review = load_review_state(server, &proposal_id)?;
        let record = proposal_record_from_row(&row, review);
        let status = record
            .get("status")
            .and_then(|value| value.as_str())
            .unwrap_or("proposed");
        if params.approved_only && status != "approved" {
            continue;
        }
        proposal_records.push(record);
    }

    if proposal_records.is_empty() {
        return serde_json::to_string(&json!({
            "status": "skipped",
            "reason": "no_matching_proposals",
            "agent_id": params.agent_id,
        }))
        .map_err(|e| format!("Failed to serialize projection response: {e}"));
    }

    let mut projected_documents = Vec::<serde_json::Value>::new();
    let mut applied_proposal_ids = std::collections::HashSet::<String>::new();

    for doc in &params.documents {
        let mut content = doc.content.clone();
        let mut applied = Vec::<serde_json::Value>::new();
        for record in &proposal_records {
            let proposal_id = record
                .get("proposal_id")
                .and_then(|value| value.as_str())
                .unwrap_or("")
                .to_string();
            let status = record
                .get("status")
                .and_then(|value| value.as_str())
                .unwrap_or("proposed")
                .to_string();
            let Some(synthesis) = record.get("synthesis").cloned().and_then(|value| {
                serde_json::from_value::<memcore::AgentEvolutionSynthesis>(value).ok()
            }) else {
                continue;
            };
            for proposal in synthesis.proposals {
                if !proposal_targets_document(&proposal, doc) {
                    continue;
                }
                content = apply_markdown_section_update(
                    &content,
                    proposal.target_section.as_deref(),
                    &proposal.suggested_value,
                );
                applied.push(json!({
                    "proposal_id": proposal_id,
                    "status": status,
                    "title": proposal.title,
                    "target_section": proposal.target_section,
                }));
                applied_proposal_ids.insert(proposal_id.clone());
            }
        }

        let written = if params.write && !applied.is_empty() {
            let path = doc.path.as_deref().ok_or_else(|| {
                "project_agent_profile write=true requires document paths".to_string()
            })?;
            write_document_if_requested(path, &content).await?;
            true
        } else {
            false
        };

        projected_documents.push(json!({
            "kind": doc.kind,
            "path": doc.path,
            "written": written,
            "applied_proposals": applied,
            "content": content,
        }));
    }

    if params.write {
        for proposal_id in &applied_proposal_ids {
            mark_proposal_applied(server, proposal_id)?;
        }
    }

    serde_json::to_string(&json!({
        "status": "completed",
        "agent_id": params.agent_id,
        "applied_count": applied_proposal_ids.len(),
        "documents": projected_documents,
    }))
    .map_err(|e| format!("Failed to serialize projection response: {e}"))
}
