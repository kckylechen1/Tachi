use crate::server_state::MemoryServer;
use crate::tool_params::SynthesizeAgentEvolutionParams;
use chrono::Utc;
use serde_json::json;

use super::FOUNDRY_JOB_NAMESPACE;

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

pub(super) fn load_foundry_job_state(
    server: &MemoryServer,
    job_id: &str,
) -> Result<Option<serde_json::Value>, String> {
    server.with_global_store(
        |store| match store.get_state_kv(FOUNDRY_JOB_NAMESPACE, job_id) {
            Ok(Some((value, _version))) => serde_json::from_str(&value)
                .map(Some)
                .map_err(|e| format!("Failed to parse foundry job state: {e}")),
            Ok(None) => Ok(None),
            Err(e) => Err(format!("Failed to load foundry job state: {e}")),
        },
    )
}

pub(super) fn foundry_job_status(state: &serde_json::Value) -> &str {
    state
        .get("status")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("")
}

fn foundry_job_updated_at(state: &serde_json::Value) -> Option<chrono::DateTime<Utc>> {
    state
        .get("updated_at")
        .and_then(serde_json::Value::as_str)
        .and_then(|raw| chrono::DateTime::parse_from_rfc3339(raw).ok())
        .map(|ts| ts.with_timezone(&Utc))
}

pub(super) fn foundry_running_job_is_stale(state: &serde_json::Value) -> bool {
    const STALE_AFTER_SECS: i64 = 30 * 60;
    foundry_job_updated_at(state)
        .is_some_and(|updated| (Utc::now() - updated).num_seconds() > STALE_AFTER_SECS)
}

pub(super) fn foundry_job_is_active(state: &serde_json::Value) -> bool {
    match foundry_job_status(state) {
        "queued" => true,
        "running" => !foundry_running_job_is_stale(state),
        _ => false,
    }
}

pub(super) fn try_claim_foundry_job(
    server: &MemoryServer,
    job: &memcore::FoundryJobSpec,
) -> bool {
    let claim = server.with_global_store(|store| {
        let value_json = foundry_job_state_json(job, "running", json!({}))?;
        match store
            .get_state_kv(FOUNDRY_JOB_NAMESPACE, &job.id)
            .map_err(|e| format!("Failed to load foundry job state: {e}"))?
        {
            Some((raw, version)) => {
                let state: serde_json::Value = serde_json::from_str(&raw)
                    .map_err(|e| format!("Failed to parse foundry job state: {e}"))?;
                let status = foundry_job_status(&state);
                if status == "running" && !foundry_running_job_is_stale(&state) {
                    return Ok(false);
                }
                if status == "completed" || status == "failed" {
                    return Ok(false);
                }
                store
                    .set_state_if_version(FOUNDRY_JOB_NAMESPACE, &job.id, &value_json, version)
                    .map_err(|e| format!("Failed to claim foundry job: {e}"))
            }
            None => store
                .insert_state_if_absent(FOUNDRY_JOB_NAMESPACE, &job.id, &value_json)
                .map_err(|e| format!("Failed to claim new foundry job: {e}")),
        }
    });

    match claim {
        Ok(claimed) => claimed,
        Err(err) => {
            eprintln!(
                "[foundry-agent-evolution] failed to claim job {}: {err}",
                job.id
            );
            false
        }
    }
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

pub(super) fn save_foundry_job_state(
    server: &MemoryServer,
    job: &memcore::FoundryJobSpec,
    status: &str,
    extra: serde_json::Value,
) -> Result<(), String> {
    let value_json = foundry_job_state_json(job, status, extra)?;
    server.with_global_store(|store| {
        store
            .set_state(FOUNDRY_JOB_NAMESPACE, &job.id, &value_json)
            .map_err(|e| format!("Failed to persist foundry job state: {e}"))?;
        Ok(())
    })
}

fn foundry_job_state_json(
    job: &memcore::FoundryJobSpec,
    status: &str,
    extra: serde_json::Value,
) -> Result<String, String> {
    let mut payload = serde_json::Map::new();
    payload.insert("job".into(), json!(job));
    payload.insert("status".into(), json!(status));
    payload.insert("updated_at".into(), json!(Utc::now().to_rfc3339()));
    if let Some(extra_obj) = extra.as_object() {
        for (key, value) in extra_obj {
            payload.insert(key.clone(), value.clone());
        }
    }
    serde_json::to_string(&serde_json::Value::Object(payload))
        .map_err(|e| format!("Failed to serialize foundry job state: {e}"))
}
