use memcore::{AuthorityLevel, EffectScope, ProjectionKind, TachiEventRecord};
use serde_json::{json, Value};

use crate::tool_params::Message;
use crate::MemoryServer;

use super::storage::write_event;
use super::{
    now_rfc3339, parse_continuity_candidate_batch, parse_continuity_outcome_label,
    stable_event_payload_id, ContinuityEventTarget,
};

fn projection_candidate_event_type(projection: ProjectionKind) -> &'static str {
    match projection {
        ProjectionKind::Pattern => "pattern.candidate",
        ProjectionKind::Timeline => "timeline.candidate",
        ProjectionKind::Outcome => "outcome.candidate",
        ProjectionKind::Affect => "affect.candidate",
        ProjectionKind::Bonding => "bonding.candidate",
        ProjectionKind::WorldBook => "world_book.candidate",
        ProjectionKind::ProjectCycle => "project_cycle.candidate",
        ProjectionKind::DomainProfile => "domain_profile.candidate",
        ProjectionKind::EvidenceGate => "evidence_gate.candidate",
    }
}

fn continuity_pipeline_enabled() -> bool {
    std::env::var("TACHI_CONTINUITY_PIPELINE")
        .map(|value| {
            matches!(
                value.trim().to_ascii_lowercase().as_str(),
                "1" | "true" | "yes" | "on"
            )
        })
        .unwrap_or(false)
}

pub(crate) fn maybe_spawn_session_continuity_pipeline(
    server: &MemoryServer,
    target: ContinuityEventTarget,
    operation_key: &str,
    conversation_id: String,
    turn_id: String,
    agent_id: String,
    project: Option<String>,
    messages: Vec<Message>,
) -> Value {
    if !continuity_pipeline_enabled() {
        return json!({
            "status": "disabled",
            "reason": "set TACHI_CONTINUITY_PIPELINE=1 to run distill/reasoning continuity labelers",
        });
    }

    match target.claim_pipeline_schedule(server, operation_key) {
        Ok(false) => return json!({"status": "already_scheduled", "operation_key": operation_key}),
        Err(error) => {
            return json!({"status": "failed", "reason": "schedule_claim_failed", "error": error})
        }
        Ok(true) => {}
    }

    let server_clone = server.clone();
    let event_count_hint = messages.len();
    tokio::spawn(async move {
        let payload = json!({
            "conversation_id": conversation_id,
            "turn_id": turn_id,
            "agent_id": agent_id,
            "messages": messages,
        });
        let request = match serde_json::to_string_pretty(&payload) {
            Ok(request) => request,
            Err(error) => {
                tracing::warn!("[continuity] serialize session payload failed: {error}");
                return;
            }
        };

        match server_clone
            .llm
            .call_distill_llm_with_receipt(
                crate::prompts::CONTINUITY_CANDIDATE_PROMPT,
                &request,
                None,
                0.2,
                1800,
            )
            .await
        {
            Ok(raw)
                if raw.invocation.completion_status()
                    == tachi_llm::CompletionStatusV1::Truncated =>
            {
                tracing::warn!("[continuity] candidate distill rejected: llm_output_truncated");
            }
            Ok(raw) => match parse_continuity_candidate_batch(&raw.value) {
                Ok(batch) => {
                    for candidate in batch.candidates {
                        let event_type = candidate
                            .event_type
                            .clone()
                            .filter(|value| !value.trim().is_empty())
                            .unwrap_or_else(|| {
                                projection_candidate_event_type(candidate.projection).to_string()
                            });
                        let provenance = match crate::provenance::attach_model_invocation(
                            json!({
                                "source": "continuity_distill",
                                "lane": "distill",
                            }),
                            &raw.invocation,
                        ) {
                            Ok(provenance) => provenance,
                            Err(error) => {
                                tracing::warn!(
                                    "[continuity] candidate receipt attachment failed: {error}"
                                );
                                continue;
                            }
                        };
                        let event = TachiEventRecord {
                            id: stable_event_payload_id(&[
                                event_type.as_str(),
                                payload["conversation_id"].as_str().unwrap_or_default(),
                                candidate.summary.as_str(),
                                &uuid::Uuid::new_v4().to_string(),
                            ]),
                            source_repo: "tachi".to_string(),
                            adapter: "continuity_distill".to_string(),
                            project: target.project_label(project.as_deref()),
                            domain: "session".to_string(),
                            session_id: payload["conversation_id"]
                                .as_str()
                                .unwrap_or_default()
                                .to_string(),
                            actor: payload["agent_id"].as_str().unwrap_or_default().to_string(),
                            event_type,
                            authority: AuthorityLevel::CollectOnly,
                            effects: vec![EffectScope::None],
                            projection_hints: vec![candidate.projection],
                            payload: json!({
                                "candidate": candidate,
                                "auto_applied": false,
                            }),
                            provenance,
                            created_at: now_rfc3339(),
                        };
                        if let Err(error) = write_event(&server_clone, &target, &event) {
                            tracing::warn!("[continuity] candidate event write failed: {error}");
                        }
                    }
                }
                Err(error) => tracing::warn!("[continuity] candidate parse failed: {error}"),
            },
            Err(error) => tracing::warn!("[continuity] candidate distill failed: {error}"),
        }

        match server_clone
            .llm
            .call_reasoning_llm_with_receipt(
                crate::prompts::SESSION_OUTCOME_LABEL_PROMPT,
                &request,
                None,
                0.0,
                900,
            )
            .await
        {
            Ok(raw) if raw.truncated => {
                tracing::warn!("[continuity] outcome label rejected: llm_output_truncated");
            }
            Ok(raw) => match parse_continuity_outcome_label(&raw.text) {
                Ok(label) => {
                    let provenance = match crate::provenance::attach_model_invocation(
                        json!({
                            "source": "continuity_labeler",
                            "lane": "reasoning",
                            "note": "read-only signal; projectors must calibrate before automatic counter updates",
                        }),
                        &raw.invocation,
                    ) {
                        Ok(provenance) => provenance,
                        Err(error) => {
                            tracing::warn!(
                                "[continuity] outcome receipt attachment failed: {error}"
                            );
                            return;
                        }
                    };
                    let event = TachiEventRecord {
                        id: stable_event_payload_id(&[
                            "session.outcome",
                            payload["conversation_id"].as_str().unwrap_or_default(),
                            payload["turn_id"].as_str().unwrap_or_default(),
                            &uuid::Uuid::new_v4().to_string(),
                        ]),
                        source_repo: "tachi".to_string(),
                        adapter: "continuity_labeler".to_string(),
                        project: target.project_label(project.as_deref()),
                        domain: "session".to_string(),
                        session_id: payload["conversation_id"]
                            .as_str()
                            .unwrap_or_default()
                            .to_string(),
                        actor: payload["agent_id"].as_str().unwrap_or_default().to_string(),
                        event_type: "session.outcome".to_string(),
                        authority: AuthorityLevel::ReviewSignalOnly,
                        effects: vec![EffectScope::Scoring],
                        projection_hints: vec![
                            ProjectionKind::Outcome,
                            ProjectionKind::EvidenceGate,
                        ],
                        payload: json!({
                            "outcome": label.outcome.as_str(),
                            "evidence_basis": label.evidence_basis.as_str(),
                            "confidence": label.confidence,
                            "rationale": label.rationale,
                            "evidence_refs": label.evidence_refs,
                            "claims": label.claims,
                            "open_questions": label.open_questions,
                        }),
                        provenance,
                        created_at: now_rfc3339(),
                    };
                    if let Err(error) = write_event(&server_clone, &target, &event) {
                        tracing::warn!("[continuity] outcome event write failed: {error}");
                    }
                }
                Err(error) => tracing::warn!("[continuity] outcome parse failed: {error}"),
            },
            Err(error) => tracing::warn!("[continuity] outcome labeler failed: {error}"),
        }
    });

    json!({
        "status": "scheduled_best_effort",
        "delivery": "at_most_once",
        "operation_key": operation_key,
        "messages": event_count_hint,
        "lanes": ["distill", "reasoning"],
    })
}
