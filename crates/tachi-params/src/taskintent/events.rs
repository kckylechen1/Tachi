//! Durable task events for `watch` (tachi#1840, zeroclaw #205 TB-9/TB-10).
//!
//! Every durable event binds the 8-field set (TB-9): monotonic `seq`,
//! stable `event_id`, source identity, source revision, `occurred_at`,
//! `recorded_at`, payload digest, visibility class.
//!
//! **Sequence assignment is a deterministic projection, not a stored
//! cursor**: events are derived per-task from the ordered fact log kept by
//! the [`super::TaskFactSource`], sorted by `(recorded_at, event_id)`, and
//! numbered `1..` in that order. Reconnect with `after_seq = last_seen`
//! therefore replays EXACTLY the missed events with no gaps and no
//! duplicates — the derivation is a pure function of the fact log, so a
//! re-read after downtime cannot fork. Duplicate delivery of a known
//! `(seq, event_id)` is deterministically suppressed (dedup by `event_id`
//! at derivation time and again at page assembly).
//!
//! TB-10: stale events never regress canonical state — event derivation is
//! read-only over facts, and the dimension projections (TB-16) fold facts
//! order-independently for terminals; see `mapping::execution`'s
//! `stale_events_never_regress_terminal_state` test.

use serde::{Deserialize, Serialize};

use super::refs::TaskRef;

/// Event visibility class (TB-9 field 8). Private-Dyad-class content never
/// exists on this wire (TB-4).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VisibilityClass {
    /// Public-safe event payload.
    Public,
    /// Internal-only event payload.
    Internal,
}

/// Source identity for an event (TB-9 field 3): which existing-truth
/// surface produced the fact.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EventSource {
    /// Bridge admission (submit path).
    BridgeAdmission,
    /// Existing `dispatch_outcomes` truth (outcome rows).
    DispatchOutcome {
        /// The canonical outcome row id.
        outcome_id: String,
    },
    /// Existing `session_claims` truth (WorkClaim transitions).
    WorkClaim {
        /// The canonical claim id.
        claim_id: String,
    },
    /// Existing `dispatch_adjudications` truth.
    Adjudication {
        /// The canonical adjudication id.
        adjudication_id: String,
    },
    /// Lifecycle-owner intervention/stop path (tachi#1678 vocabulary).
    LifecycleOwner {
        /// The owner's operation token.
        operation: String,
    },
}

/// One page of events returned by `watch`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskEventPage {
    /// The watched task.
    pub task_ref: TaskRef,
    /// Events with `seq > after_seq`, in seq order.
    pub events: Vec<TaskEvent>,
    /// Whether more events exist beyond this page.
    pub has_more: bool,
}

/// A durable task event binding the TB-9 8-field set.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskEvent {
    /// 1) Monotonic per-task sequence (derived; gaps impossible).
    pub seq: u64,
    /// 2) Stable event id (content-derived from the source fact).
    pub event_id: String,
    /// 3) Source identity.
    pub source: EventSource,
    /// 4) Source revision (row revision / transition version).
    pub source_revision: String,
    /// 5) When the fact occurred (source-stated).
    pub occurred_at: String,
    /// 6) When Tachi recorded the fact.
    pub recorded_at: String,
    /// 7) Canonical payload digest (SHA-256 hex via the workspace rule).
    pub payload_digest: String,
    /// 8) Visibility/redaction class.
    pub visibility: VisibilityClass,
    /// The typed event payload.
    pub payload: TaskEventPayload,
}

/// Typed event payloads (projections over existing truth; never
/// caller-authored success — tachi#1678 law).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskEventPayload {
    /// Intent admitted; task minted. Carries the intent digest and the
    /// intent-derived contract projection (TB-2: re-targeting preserves
    /// this projection across plans).
    TaskSubmitted {
        /// Canonical intent digest (TB-7 rule 1).
        intent_digest: String,
        /// The intent-derived contract projection (objective, constraints,
        /// expected artifacts, evaluation requirement) — plan-independent.
        contract: IntentContractProjection,
    },
    /// An ExecutionPlan was admitted for the task (plan identity is
    /// Tachi-chosen; TB-2).
    PlanAdmitted {
        /// The plan projection identity.
        plan: super::plan::ExecutionPlanProjection,
    },
    /// Canonical execution fact observed (from existing truth surfaces).
    Execution(super::mapping::execution::CanonicalExecutionFact),
    /// Canonical adjudication fact observed.
    Adjudication(super::mapping::adjudication::CanonicalAdjudicationFact),
    /// Canonical delivery fact observed.
    Delivery(super::mapping::delivery::CanonicalDeliveryFact),
    /// A stop was requested (TB-12: request, not confirmation).
    StopRequested {
        /// The stop operation id (idempotency-bound).
        stop_id: String,
        /// `graceful` or `hard`.
        mode: String,
    },
    /// The lifecycle OWNER confirmed a terminal (e.g. cancellation confirmed
    /// by the harness — TB-12).
    OwnerConfirmedTerminal {
        /// The terminal class confirmed by the owner.
        terminal: String,
    },
    /// An intervention was forwarded to the lifecycle owner (tachi#1678
    /// typed request path).
    InterventionForwarded {
        /// The intervention operation token.
        operation: String,
        /// The intervention receipt id.
        intervention_id: String,
    },
    /// The lifecycle owner disappeared after possible side effects (maps to
    /// `outcome_unknown`, never optimistic success/failure/cancel).
    OwnerDisappeared,
    /// The lifecycle owner declared its closed intervention capability set
    /// (ingested from existing truth — e.g. attachment
    /// `session_capabilities`). Only this fact can widen a snapshot's
    /// advertisement beyond the mode baseline (TB-15).
    OwnerCapabilitiesDeclared {
        /// The declared operations.
        operations: Vec<super::intervention::InterventionV1Static>,
    },
    /// An existing `dispatch_outcomes` row was observed for this task
    /// (ingested canonical terminal truth; see [`OutcomeObservation`]).
    OutcomeObserved {
        /// The observation projected from the canonical row.
        observation: OutcomeObservation,
    },
}

/// A projection of one canonical `dispatch_outcomes` row onto the bridge
/// fact log. Every field copies existing truth; none is caller-authored.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OutcomeObservation {
    /// `dispatch_outcomes.outcome_id`.
    pub outcome_id: String,
    /// `dispatch_outcomes.dispatch_id` (the attempt identity spine).
    pub dispatch_id: String,
    /// The machine-resolved terminal mapped into the canonical execution
    /// vocabulary (`completed`/`failed`/`aborted`/`partial`).
    pub execution: super::mapping::execution::CanonicalExecutionFact,
    /// The verbatim agent self-report, kept for audit only.
    pub reported_outcome: Option<String>,
    /// `verification_present` evidence fact.
    pub verification_present: bool,
    /// `diff_present` evidence fact.
    pub diff_present: bool,
    /// `evidence_refs` (artifact/evidence refs).
    pub evidence_refs: Vec<String>,
    /// Observed vendor (`unknown` when unattributed).
    pub vendor: String,
    /// Observed model, if recorded.
    pub model: Option<String>,
    /// Identity attribution basis (#1065 option D vocabulary).
    pub identity_attribution_basis: String,
}

/// The intent-derived contract projection (TB-2): plan-independent fields
/// the bridge projects from the SUBMITTED intent, so two different admitted
/// ExecutionPlans over one intent observably share this projection.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct IntentContractProjection {
    /// Objective text.
    pub objective: String,
    /// Constraint descriptions.
    pub constraints: Vec<String>,
    /// Expected artifact classes + requiredness.
    pub expected_artifacts: Vec<ExpectedArtifactProjection>,
    /// Required evaluation independence class.
    pub evaluation_requirement: super::wire::IndependenceClass,
}

/// Expected-artifact projection entry.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExpectedArtifactProjection {
    /// Artifact class.
    pub artifact_class: super::wire::ArtifactClass,
    /// Whether it is required for contract satisfaction.
    pub required: bool,
}

impl TaskEvent {
    /// Derive the ordered event list for a task's raw fact log: stable sort
    /// by `(recorded_at, event_id)`, dedup by `event_id`, assign `seq = 1..`.
    pub(crate) fn derive(facts: Vec<TaskEvent>) -> Vec<TaskEvent> {
        let mut facts = facts;
        facts.sort_by(|a, b| {
            (a.recorded_at.as_str(), a.event_id.as_str())
                .cmp(&(b.recorded_at.as_str(), b.event_id.as_str()))
        });
        facts.dedup_by(|a, b| a.event_id == b.event_id);
        facts
            .into_iter()
            .enumerate()
            .map(|(index, mut event)| {
                event.seq = (index + 1) as u64;
                event
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn event(event_id: &str, recorded_at: &str) -> TaskEvent {
        TaskEvent {
            seq: 0,
            event_id: event_id.to_string(),
            source: EventSource::BridgeAdmission,
            source_revision: "1".to_string(),
            occurred_at: recorded_at.to_string(),
            recorded_at: recorded_at.to_string(),
            payload_digest: "digest".to_string(),
            visibility: VisibilityClass::Internal,
            payload: TaskEventPayload::OwnerDisappeared,
        }
    }

    #[test]
    fn derivation_assigns_monotonic_gapless_seq() {
        // Deliberately unordered input.
        let derived = TaskEvent::derive(vec![
            event("c", "2026-08-25T03:00:00Z"),
            event("a", "2026-08-25T01:00:00Z"),
            event("b", "2026-08-25T02:00:00Z"),
        ]);
        let seqs: Vec<u64> = derived.iter().map(|e| e.seq).collect();
        assert_eq!(seqs, vec![1, 2, 3]);
    }

    #[test]
    fn duplicate_delivery_is_deterministically_suppressed() {
        let derived = TaskEvent::derive(vec![
            event("a", "2026-08-25T01:00:00Z"),
            event("a", "2026-08-25T01:00:00Z"),
            event("b", "2026-08-25T02:00:00Z"),
        ]);
        assert_eq!(derived.len(), 2);
    }

    #[test]
    fn watch_after_seq_replays_exactly_the_missed_events() {
        // TB-9: reconnect with after_seq = last_seen ⇒ exactly the missed
        // events, no gaps, no duplicates.
        let derived = TaskEvent::derive(vec![
            event("a", "2026-08-25T01:00:00Z"),
            event("b", "2026-08-25T02:00:00Z"),
            event("c", "2026-08-25T03:00:00Z"),
            event("d", "2026-08-25T04:00:00Z"),
        ]);
        let missed: Vec<&str> = derived
            .iter()
            .filter(|e| e.seq > 2)
            .map(|e| e.event_id.as_str())
            .collect();
        assert_eq!(missed, vec!["c", "d"]);
    }
}
