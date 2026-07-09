use crate::memory_search_ops::search_memory_rows;
use crate::server_state::{DbScope, MemoryServer};
use crate::tool_params::{SearchMemoryParams, SynthesizeAgentEvolutionParams};
use serde_json::{json, Value};

use super::paths::read_foundry_input_text;
use super::proposals::persist_agent_evolution_proposal;

pub(super) fn parse_document_kind(
    raw: &str,
) -> Result<memcore::AgentProfileDocumentKind, String> {
    memcore::AgentProfileDocumentKind::parse(raw).ok_or_else(|| {
        format!(
            "Unknown document kind '{}'. Expected identity|agents|latest_truths|routing_policy|tool_policy|memory_policy|other",
            raw
        )
    })
}

fn parse_evidence_kind(raw: &str) -> Result<memcore::FoundryEvidenceKind, String> {
    memcore::FoundryEvidenceKind::parse(raw).ok_or_else(|| {
        format!(
            "Unknown evidence kind '{}'. Expected memory|reflection|tooluse|eval|ghost|session_outcome|skill_telemetry|profile_snapshot|proposal|other",
            raw
        )
    })
}

pub(super) fn has_evolution_inputs(params: &SynthesizeAgentEvolutionParams) -> bool {
    !params.documents.is_empty()
        || !params.document_paths.is_empty()
        || !params.evidence.is_empty()
        || !params.evidence_paths.is_empty()
        || !params.memory_queries.is_empty()
}

pub(super) fn build_documents(
    params: &SynthesizeAgentEvolutionParams,
) -> Result<Vec<memcore::AgentProfileDocument>, String> {
    let mut documents = params
        .documents
        .iter()
        .map(|doc| {
            Ok(memcore::AgentProfileDocument {
                kind: parse_document_kind(&doc.kind)?,
                path: doc.path.clone(),
                content: doc.content.clone(),
            })
        })
        .collect::<Result<Vec<_>, String>>()?;

    for doc in &params.document_paths {
        let kind = parse_document_kind(&doc.kind)?;
        let (resolved_path, content) = read_foundry_input_text(&doc.path)?;
        documents.push(memcore::AgentProfileDocument {
            kind,
            path: Some(resolved_path),
            content,
        });
    }

    Ok(documents)
}

pub(super) async fn build_evidence(
    server: &MemoryServer,
    params: &SynthesizeAgentEvolutionParams,
) -> Result<Vec<memcore::FoundryEvidence>, String> {
    let mut evidence = params
        .evidence
        .iter()
        .map(|item| {
            Ok(memcore::FoundryEvidence {
                kind: parse_evidence_kind(&item.kind)?,
                title: item.title.clone(),
                content: item.content.clone(),
                source_ref: item.source_ref.clone(),
                path: item.path.clone(),
                weight: item.weight.max(0.0),
            })
        })
        .collect::<Result<Vec<_>, String>>()?;

    for item in &params.evidence_paths {
        let kind = parse_evidence_kind(&item.kind)?;
        let (resolved_path, content) = read_foundry_input_text(&item.path)?;
        let fallback_title = std::path::Path::new(&resolved_path)
            .file_name()
            .and_then(|name| name.to_str())
            .map(ToOwned::to_owned);
        evidence.push(memcore::FoundryEvidence {
            kind,
            title: item.title.clone().or(fallback_title),
            content,
            source_ref: item
                .source_ref
                .clone()
                .or_else(|| Some(resolved_path.clone())),
            path: Some(resolved_path),
            weight: item.weight.max(0.0),
        });
    }

    for query in &params.memory_queries {
        let rows = search_memory_rows(
            server,
            SearchMemoryParams {
                query: query.query.clone(),
                query_vec: None,
                top_k: query.top_k.max(1),
                path_prefix: query.path_prefix.clone(),
                include_training: false,
                include_archived: false,
                candidates_per_channel: query.top_k.max(1).max(20),
                mmr_threshold: None,
                graph_expand_hops: 1,
                graph_relation_filter: None,
                weights: None,
                context_symbols: Vec::new(),
                agent_role: None,
                project: query.project.clone(),
                domain: None,
                file_context: None,
                error_context: None,
                enable_rerank: false,
                as_of: None,
                include_metadata: false,
            },
            false,
        )
        .await?;
        if rows.is_empty() {
            continue;
        }
        let mut lines = vec![format!("Memory query: {}", query.query.trim())];
        for (idx, row) in rows.iter().enumerate() {
            let id = row.get("id").and_then(|value| value.as_str()).unwrap_or("");
            let topic = row
                .get("topic")
                .and_then(|value| value.as_str())
                .unwrap_or("");
            let summary = row
                .get("summary")
                .and_then(|value| value.as_str())
                .unwrap_or("");
            let text = row
                .get("text")
                .and_then(|value| value.as_str())
                .unwrap_or("");
            let path = row
                .get("path")
                .and_then(|value| value.as_str())
                .unwrap_or("");
            let relevance = row
                .get("relevance")
                .and_then(|value| value.as_f64())
                .unwrap_or(0.0);
            lines.push(format!(
                "{}. [{}] {} (id={}, score={:.3}, path={})",
                idx + 1,
                if topic.is_empty() { "memory" } else { topic },
                if summary.is_empty() { text } else { summary },
                id,
                relevance,
                path
            ));
            if !summary.is_empty() && summary != text {
                lines.push(format!("   Detail: {}", text));
            }
        }
        evidence.push(memcore::FoundryEvidence {
            kind: memcore::FoundryEvidenceKind::Memory,
            title: query
                .title
                .clone()
                .or_else(|| Some(format!("memory query: {}", query.query.trim()))),
            content: lines.join("\n"),
            source_ref: Some(format!("memory_query:{}", query.query.trim())),
            path: query.path_prefix.clone(),
            weight: query.weight.max(0.0),
        });
    }

    Ok(evidence)
}

pub(super) fn build_synthesis_payload(
    params: &SynthesizeAgentEvolutionParams,
    documents: &[memcore::AgentProfileDocument],
    evidence: &[memcore::FoundryEvidence],
) -> Value {
    json!({
        "agent": {
            "agent_id": params.agent_id,
            "display_name": params.display_name,
        },
        "goals": params.goals,
        "documents": documents,
        "evidence": evidence,
    })
}

pub(super) fn parse_synthesis_response(
    raw: &str,
) -> Result<memcore::AgentEvolutionSynthesis, String> {
    let json_str = tachi_llm::LlmClient::strip_code_fence(raw);
    serde_json::from_str(json_str).map_err(|e| {
        format!(
            "Failed to parse agent evolution synthesis JSON: {e} — response was: {}",
            json_str
        )
    })
}

pub(super) async fn run_agent_evolution_synthesis(
    server: &MemoryServer,
    params: &SynthesizeAgentEvolutionParams,
    job: &memcore::FoundryJobSpec,
) -> Result<
    (
        memcore::AgentEvolutionSynthesis,
        String,
        String,
        DbScope,
    ),
    String,
> {
    let documents = build_documents(params)?;
    let evidence = build_evidence(server, params).await?;
    let payload = build_synthesis_payload(params, &documents, &evidence);
    let user = serde_json::to_string_pretty(&payload)
        .map_err(|e| format!("Failed to serialize synthesis request: {e}"))?;
    let response = server
        .llm
        .call_reasoning_llm(
            crate::prompts::AGENT_EVOLUTION_SYNTHESIS_PROMPT,
            &user,
            None,
            0.2,
            2400,
        )
        .await?;
    let synthesis = parse_synthesis_response(&response)?;
    let (proposal_id, proposal_path, target_db) =
        persist_agent_evolution_proposal(server, params, job, &synthesis)?;
    Ok((synthesis, proposal_id, proposal_path, target_db))
}
