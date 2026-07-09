use crate::server_state::{DbScope, MemoryServer};
use crate::tool_params::SynthesizeAgentEvolutionParams;
use crate::utils::sanitize_safe_path_name;
use chrono::Utc;
use serde_json::json;

use super::{AGENT_EVOLUTION_PROPOSAL_SOURCE, FOUNDRY_PROPOSAL_REVIEW_NAMESPACE};

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

pub(super) fn review_status_or_default(raw: Option<&str>) -> String {
    match raw.unwrap_or("").trim().to_ascii_lowercase().as_str() {
        "approved" => "approved".to_string(),
        "rejected" => "rejected".to_string(),
        "applied" => "applied".to_string(),
        _ => "proposed".to_string(),
    }
}

pub(super) fn parse_review_status(raw: &str) -> Result<String, String> {
    match raw.trim().to_ascii_lowercase().as_str() {
        "approved" => Ok("approved".to_string()),
        "rejected" => Ok("rejected".to_string()),
        "applied" => Ok("applied".to_string()),
        other => Err(format!(
            "Invalid review status '{}'. Expected approved|rejected|applied",
            other
        )),
    }
}

pub(super) fn load_review_state(
    server: &MemoryServer,
    proposal_id: &str,
) -> Result<Option<serde_json::Value>, String> {
    server.with_global_store(|store| {
        match store.get_state_kv(FOUNDRY_PROPOSAL_REVIEW_NAMESPACE, proposal_id) {
            Ok(Some((value, _version))) => {
                let parsed =
                    serde_json::from_str(&value).unwrap_or_else(|_| json!({ "raw": value }));
                Ok(Some(parsed))
            }
            Ok(None) => Ok(None),
            Err(e) => Err(format!("Failed to load proposal review state: {e}")),
        }
    })
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

fn parse_derived_metadata(row: &serde_json::Value) -> serde_json::Value {
    row.get("metadata")
        .and_then(|value| value.as_str())
        .and_then(|raw| serde_json::from_str(raw).ok())
        .unwrap_or_else(|| json!({}))
}

fn parse_derived_synthesis(row: &serde_json::Value) -> Option<memcore::AgentEvolutionSynthesis> {
    row.get("text")
        .and_then(|value| value.as_str())
        .and_then(|raw| serde_json::from_str(raw).ok())
}

pub(super) fn proposal_record_from_row(
    row: &serde_json::Value,
    review: Option<serde_json::Value>,
) -> serde_json::Value {
    let metadata = parse_derived_metadata(row);
    let status = review_status_or_default(
        review
            .as_ref()
            .and_then(|value| value.get("status"))
            .and_then(|value| value.as_str())
            .or_else(|| metadata.get("status").and_then(|value| value.as_str())),
    );
    json!({
        "proposal_id": row.get("id").and_then(|value| value.as_str()).unwrap_or(""),
        "path": row.get("path").and_then(|value| value.as_str()).unwrap_or(""),
        "summary": row.get("summary").and_then(|value| value.as_str()).unwrap_or(""),
        "created_at": row.get("created_at").and_then(|value| value.as_str()).unwrap_or(""),
        "status": status,
        "metadata": metadata,
        "review": review,
        "synthesis": parse_derived_synthesis(row),
    })
}

pub(super) fn load_agent_proposal_rows(
    server: &MemoryServer,
    agent_id: &str,
    limit: usize,
) -> Result<Vec<serde_json::Value>, String> {
    let root = proposal_root(agent_id);
    let (target_db, _warning) = resolve_foundry_write_scope(server);
    server.with_store_for_scope_read(target_db, |store| {
        store
            .list_derived_by_source(AGENT_EVOLUTION_PROPOSAL_SOURCE, &root, limit)
            .map_err(|e| format!("Failed to list agent evolution proposals: {e}"))
    })
}

pub(super) fn mark_proposal_applied(
    server: &MemoryServer,
    proposal_id: &str,
) -> Result<(), String> {
    let reviewer = server
        .agent_runtime_read()
        .agent_profile
        .as_ref()
        .map(|profile| profile.agent_id.clone());
    let mut review = load_review_state(server, proposal_id)?.unwrap_or_else(|| json!({}));
    if let Some(obj) = review.as_object_mut() {
        obj.insert("status".into(), json!("applied"));
        obj.insert("applied_at".into(), json!(Utc::now().to_rfc3339()));
        obj.insert("applied_by".into(), json!(reviewer));
    }
    let value_json = serde_json::to_string(&review)
        .map_err(|e| format!("Failed to serialize applied proposal state: {e}"))?;
    server.with_global_store(|store| {
        store
            .set_state(FOUNDRY_PROPOSAL_REVIEW_NAMESPACE, proposal_id, &value_json)
            .map_err(|e| format!("Failed to persist applied proposal state: {e}"))?;
        Ok(())
    })
}
