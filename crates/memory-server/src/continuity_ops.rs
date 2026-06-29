use chrono::{SecondsFormat, Utc};
use memory_core::TachiEventQuery;

use crate::tool_params::TachiEventParams;
use crate::{DbScope, MemoryServer};

mod context;
mod emit;
mod feedback;
mod outcome;
mod parsing;
mod pipeline;
mod projection;
mod promotion;
mod read_models;
mod storage;

pub(crate) use self::context::{build_a2a_context, build_continuity_context, list_active_patterns};
pub(crate) use self::emit::{
    emit_memory_saved_event, emit_pattern_feedback_event, emit_pattern_seen_events,
    emit_session_captured_event, emit_task_completion_events, emit_wiki_saved_event,
    WikiSavedEventInput,
};
pub(crate) use self::feedback::{
    attach_pattern_ref_to_row, emit_pattern_feedback_for_refs, pattern_feedback_refs_from_strings,
    pattern_ref_json,
};
pub(crate) use self::outcome::evaluate_outcome_labels;
pub(crate) use self::parsing::{parse_continuity_candidate_batch, parse_continuity_outcome_label};
pub(crate) use self::pipeline::maybe_spawn_session_continuity_pipeline;
pub(crate) use self::projection::{
    project_auto_continuity_events_for_target, project_continuity_events,
};
pub(crate) use self::promotion::promote_pattern_review_artifacts;
#[cfg(test)]
use self::storage::list_projection_memories;
pub(crate) use self::storage::ContinuityEventTarget;
#[cfg(test)]
use memory_core::{
    AuthorityLevel, EffectScope, OutcomeEvidenceBasis, ProjectionKind, SessionOutcomeKind,
    TachiEventRecord,
};
#[cfg(test)]
use serde_json::json;

fn now_rfc3339() -> String {
    Utc::now().to_rfc3339_opts(SecondsFormat::Millis, true)
}

fn stable_event_payload_id(parts: &[&str]) -> String {
    let joined = parts
        .iter()
        .map(|part| part.trim())
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join("|");
    format!("event-{}", crate::utils::stable_hash(&joined))
}

fn query_limit(limit: usize) -> usize {
    limit.clamp(1, 500)
}

fn trim_opt(value: &Option<String>) -> Option<String> {
    value
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
}

fn target_from_event_params(
    server: &MemoryServer,
    params: &TachiEventParams,
) -> ContinuityEventTarget {
    if let Some(project) = trim_opt(&params.project) {
        ContinuityEventTarget::new(DbScope::Project, Some(project), None)
    } else if server.has_project_db() {
        ContinuityEventTarget::new(DbScope::Project, None, None)
    } else {
        ContinuityEventTarget::new(DbScope::Global, None, None)
    }
}

fn event_query_from_params(params: &TachiEventParams) -> TachiEventQuery {
    TachiEventQuery {
        project: trim_opt(&params.project),
        domain: trim_opt(&params.domain),
        event_type: trim_opt(&params.event_type),
        session_id: trim_opt(&params.session_id),
        source_repo: trim_opt(&params.source_repo),
        adapter: trim_opt(&params.adapter),
        limit: query_limit(params.limit),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_continuity_candidates_with_projection_aliases() {
        let raw = r#"{
          "candidates": [
            {
              "projection": "timeline",
              "summary": "User reframed scope",
              "text": "The session moved from ROI judgment to code mapping.",
              "confidence": 0.74,
              "evidence_refs": ["message:4"]
            },
            {
              "kind": "worldbook",
              "summary": "Tachi substrate",
              "metadata": {"domain": "agent_os"}
            }
          ],
          "open_threads": ["wire projectors"]
        }"#;

        let parsed = parse_continuity_candidate_batch(raw).expect("parse candidates");
        assert_eq!(parsed.candidates.len(), 2);
        assert_eq!(parsed.candidates[0].projection, ProjectionKind::Timeline);
        assert_eq!(parsed.candidates[1].projection, ProjectionKind::WorldBook);
        assert_eq!(parsed.open_threads, vec!["wire projectors"]);
    }

    #[test]
    fn parses_outcome_label_into_typed_axes() {
        let raw = r#"{
          "outcome": "partial_reframe",
          "evidence_basis": "interlocutor_argument",
          "confidence": 0.61,
          "rationale": "Both sides changed scope.",
          "claims": ["enum too coarse"],
          "open_questions": ["external label source"]
        }"#;

        let parsed = parse_continuity_outcome_label(raw).expect("parse outcome");
        assert_eq!(parsed.outcome, SessionOutcomeKind::PartialReframe);
        assert_eq!(
            parsed.evidence_basis,
            OutcomeEvidenceBasis::InterlocutorArgument
        );
        assert_eq!(parsed.claims, vec!["enum too coarse"]);
    }

    #[test]
    fn emits_session_captured_event_to_target_store() {
        let dir = tempfile::tempdir().expect("temp dir");
        let db = dir.path().join("memory.db");
        let server = MemoryServer::new(db, None).expect("test server");
        let target = ContinuityEventTarget::new(DbScope::Global, None, None);
        let status = emit_session_captured_event(
            &server,
            &target,
            "conversation-1",
            "turn-1",
            "codex",
            "/agents/codex",
            &["memory-1".to_string()],
            3,
            Some("sigil"),
        );
        assert_eq!(status["status"], json!("saved"));

        let events = server
            .with_global_store_read(|store| {
                store
                    .list_tachi_events(&memory_core::TachiEventQuery {
                        event_type: Some("session.captured".to_string()),
                        session_id: Some("conversation-1".to_string()),
                        limit: 5,
                        ..memory_core::TachiEventQuery::default()
                    })
                    .map_err(|e| e.to_string())
            })
            .expect("list events");
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].project, "sigil");
        assert_eq!(
            events[0].projection_hints,
            vec![ProjectionKind::Timeline, ProjectionKind::ProjectCycle]
        );
        assert_eq!(
            events[0].payload["captured_memory_ids"][0],
            json!("memory-1")
        );
    }

    #[test]
    fn auto_projection_skips_execution_gate_events() {
        let dir = tempfile::tempdir().expect("temp dir");
        let db = dir.path().join("memory.db");
        let server = MemoryServer::new(db, None).expect("test server");
        server
            .with_global_store(|store| {
                let allowed = TachiEventRecord {
                    id: "pattern-candidate-1".to_string(),
                    source_repo: "tachi".to_string(),
                    adapter: "test".to_string(),
                    project: "sigil".to_string(),
                    domain: "agent_os".to_string(),
                    session_id: "s1".to_string(),
                    actor: "codex".to_string(),
                    event_type: "pattern.candidate".to_string(),
                    authority: AuthorityLevel::CollectOnly,
                    effects: vec![EffectScope::None],
                    projection_hints: vec![ProjectionKind::Pattern],
                    payload: json!({
                        "summary": "Continuity-first planning",
                        "text": "Use continuity evidence before picking the next project-management action.",
                        "projection_key": "continuity-first"
                    }),
                    provenance: json!({"source": "test"}),
                    created_at: now_rfc3339(),
                };
                let blocked = TachiEventRecord {
                    id: "execution-gate-1".to_string(),
                    source_repo: "tachi".to_string(),
                    adapter: "test".to_string(),
                    project: "sigil".to_string(),
                    domain: "agent_os".to_string(),
                    session_id: "s1".to_string(),
                    actor: "codex".to_string(),
                    event_type: "evidence_gate.required".to_string(),
                    authority: AuthorityLevel::ExecutionGate,
                    effects: vec![EffectScope::Execution],
                    projection_hints: vec![ProjectionKind::EvidenceGate],
                    payload: json!({
                        "summary": "Do not ship without tests",
                        "projection_key": "must-test"
                    }),
                    provenance: json!({"source": "test"}),
                    created_at: now_rfc3339(),
                };
                store.insert_tachi_event(&allowed).map_err(|e| e.to_string())?;
                store.insert_tachi_event(&blocked).map_err(|e| e.to_string())
            })
            .expect("seed events");

        let report = project_auto_continuity_events_for_target(
            &server,
            ContinuityEventTarget::new(DbScope::Global, None, None),
            20,
        )
        .expect("project events");
        assert_eq!(report["projected_count"], json!(1));
        assert_eq!(report["skipped_count"], json!(1));

        let patterns = list_projection_memories(
            &server,
            &ContinuityEventTarget::new(DbScope::Global, None, None),
            "/user/patterns",
            10,
        )
        .expect("list patterns");
        assert_eq!(patterns.len(), 1);
        assert_eq!(patterns[0].summary, "Continuity-first planning");
        let gates = list_projection_memories(
            &server,
            &ContinuityEventTarget::new(DbScope::Global, None, None),
            "/evidence-gates",
            10,
        )
        .expect("list gates");
        assert!(gates.is_empty());
    }
}
