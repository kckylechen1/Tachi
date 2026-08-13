use memcore::{AuthorityLevel, EffectScope, TachiEventRecord};
use serde_json::{json, Value};

use crate::MemoryServer;

use super::storage::{write_event_if_absent, ContinuityEventTarget};
use super::{now_rfc3339, stable_event_payload_id};

const ADAPTER: &str = "tachi.pattern_evidence.v1";
const ACTOR: &str = "tachi-internal";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PatternEvidenceSource {
    TaskCompletion,
    WorkflowClosure,
}

impl PatternEvidenceSource {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::TaskCompletion => "task_completion",
            Self::WorkflowClosure => "workflow_closure",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PatternEvidenceOutcome {
    Hit,
    Miss,
    Stale,
    Seen,
}

impl PatternEvidenceOutcome {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Hit => "hit",
            Self::Miss => "miss",
            Self::Stale => "stale",
            Self::Seen => "seen",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PatternEvidenceInput {
    pub source: PatternEvidenceSource,
    pub project: Option<String>,
    pub run_id: String,
    pub source_revision: String,
    pub evidence_digest: String,
    pub pattern_id: String,
    pub outcome: PatternEvidenceOutcome,
    pub idempotency_key: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PatternEvidenceReceipt {
    pub event_id: String,
    pub idempotency_key: String,
    pub replayed: bool,
}

fn required<'a>(name: &str, value: &'a str) -> Result<&'a str, String> {
    let value = value.trim();
    if value.is_empty() {
        Err(format!("pattern evidence requires {name}"))
    } else {
        Ok(value)
    }
}

pub(crate) fn append_pattern_evidence(
    server: &MemoryServer,
    input: PatternEvidenceInput,
) -> Result<PatternEvidenceReceipt, String> {
    let run_id = required("real run/session/flow id", &input.run_id)?;
    let source_revision = required("source revision", &input.source_revision)?;
    let evidence_digest = required("evidence digest", &input.evidence_digest)?;
    let pattern_id = required("exact target pattern id", &input.pattern_id)?;
    let idempotency_key = required("idempotency key", &input.idempotency_key)?;
    let source = input.source.as_str();
    let outcome = input.outcome.as_str();
    let event_id = stable_event_payload_id(&["pattern.evidence", idempotency_key]);
    let target = ContinuityEventTarget::from_default_write(server, input.project.as_deref());
    let event = TachiEventRecord {
        id: event_id.clone(),
        source_repo: "tachi".to_string(),
        adapter: ADAPTER.to_string(),
        project: target.project_label(input.project.as_deref()),
        domain: "pattern_evidence".to_string(),
        session_id: run_id.to_string(),
        actor: ACTOR.to_string(),
        event_type: format!("pattern.evidence.{outcome}"),
        authority: AuthorityLevel::CollectOnly,
        effects: vec![EffectScope::None],
        projection_hints: Vec::new(),
        payload: json!({
            "source": source,
            "source_revision": source_revision,
            "evidence_digest": evidence_digest,
            "pattern_id": pattern_id,
            "outcome": outcome,
            "idempotency_key": idempotency_key,
        }),
        provenance: json!({
            "source": source,
            "adapter": ADAPTER,
            "contract": "append_only_collect_only_no_projection_v1",
        }),
        created_at: now_rfc3339(),
    };
    let inserted = write_event_if_absent(server, &target, &event)?;
    Ok(PatternEvidenceReceipt {
        event_id,
        idempotency_key: idempotency_key.to_string(),
        replayed: !inserted,
    })
}

pub(crate) fn receipt_json(receipt: PatternEvidenceReceipt) -> Value {
    json!({
        "status": if receipt.replayed { "replayed" } else { "saved" },
        "event_id": receipt.event_id,
        "idempotency_key": receipt.idempotency_key,
        "replayed": receipt.replayed,
    })
}
