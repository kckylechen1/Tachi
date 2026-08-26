//! Intervention vocabulary and stop machine (tachi#1840, zeroclaw #205
//! TB-11/TB-12).
//!
//! The frozen 10-op `InterventionV1` vocabulary; typed
//! `unsupported_by_lifecycle_owner` refusal with ZERO state mutation and NO
//! fresh-task fallback; idempotency-keyed and revision-bound applies;
//! single-pathed stop authority (`RequestGracefulStop`/`RequestHardCancel`
//! inside `intervene` are aliases of `request_stop`, one underlying stop
//! operation, one `StopReceipt` payload type).
//!
//! `RequestIndependentReview`/`Escalate` are NOT session interventions —
//! they map to new task/adjudication lineage (owning tachi#1623/#1675,
//! NOT-YET); this leaf returns a typed
//! [`InterventionError::RequiresNewTaskLineage`] refusal with zero mutation
//! rather than fabricating the lineage.

use serde::{Deserialize, Serialize};

use super::refs::TaskRef;
use super::wire::BoundedText;

/// Payload-free discriminant of the frozen 10-op vocabulary (used for
/// advertisement sets and matching).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InterventionV1Static {
    /// ProvideAdditionalContext
    ProvideAdditionalContext,
    /// RequestCorrection
    RequestCorrection,
    /// RequestContinuation
    RequestContinuation,
    /// RequestIndependentReview
    RequestIndependentReview,
    /// RequestUserInput
    RequestUserInput,
    /// RequestPause
    RequestPause,
    /// RequestResume
    RequestResume,
    /// RequestGracefulStop (alias of `request_stop(graceful)`)
    RequestGracefulStop,
    /// RequestHardCancel (alias of `request_stop(hard)`)
    RequestHardCancel,
    /// Escalate
    Escalate,
}

/// All ten frozen operations (machine-checkable vocabulary freeze).
pub const INTERVENTION_V1_OPERATIONS: &[InterventionV1Static] = &[
    InterventionV1Static::ProvideAdditionalContext,
    InterventionV1Static::RequestCorrection,
    InterventionV1Static::RequestContinuation,
    InterventionV1Static::RequestIndependentReview,
    InterventionV1Static::RequestUserInput,
    InterventionV1Static::RequestPause,
    InterventionV1Static::RequestResume,
    InterventionV1Static::RequestGracefulStop,
    InterventionV1Static::RequestHardCancel,
    InterventionV1Static::Escalate,
];

/// The frozen 10-op intervention vocabulary (TB-11).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum InterventionV1 {
    /// Provide additional context to the running work.
    ProvideAdditionalContext {
        /// The context note (content, not authority — TB-4 seam law).
        note: BoundedText,
    },
    /// Request a correction.
    RequestCorrection {
        /// What to correct.
        note: BoundedText,
    },
    /// Request continuation (NEVER independent review — TB-17).
    RequestContinuation {
        /// Continuation note.
        note: BoundedText,
    },
    /// Request independent review (new task/adjudication lineage — refused
    /// by this leaf's session-intervention path).
    RequestIndependentReview {
        /// Required independence class for the review.
        independence_class: super::wire::IndependenceClass,
    },
    /// Request user input.
    RequestUserInput {
        /// The question to surface.
        prompt: BoundedText,
    },
    /// Request a pause.
    RequestPause,
    /// Request a resume.
    RequestResume,
    /// Request a graceful stop — alias of `request_stop(graceful)`.
    RequestGracefulStop {
        /// Why the stop is requested.
        reason: BoundedText,
    },
    /// Request a hard cancel — alias of `request_stop(hard)`.
    RequestHardCancel {
        /// Why the cancel is requested.
        reason: BoundedText,
    },
    /// Escalate (new task/adjudication lineage — refused by this leaf's
    /// session-intervention path).
    Escalate {
        /// Why the escalation is requested.
        reason: BoundedText,
    },
}

impl InterventionV1 {
    /// The payload-free discriminant of this operation.
    pub fn discriminant(&self) -> InterventionV1Static {
        match self {
            Self::ProvideAdditionalContext { .. } => InterventionV1Static::ProvideAdditionalContext,
            Self::RequestCorrection { .. } => InterventionV1Static::RequestCorrection,
            Self::RequestContinuation { .. } => InterventionV1Static::RequestContinuation,
            Self::RequestIndependentReview { .. } => InterventionV1Static::RequestIndependentReview,
            Self::RequestUserInput { .. } => InterventionV1Static::RequestUserInput,
            Self::RequestPause => InterventionV1Static::RequestPause,
            Self::RequestResume => InterventionV1Static::RequestResume,
            Self::RequestGracefulStop { .. } => InterventionV1Static::RequestGracefulStop,
            Self::RequestHardCancel { .. } => InterventionV1Static::RequestHardCancel,
            Self::Escalate { .. } => InterventionV1Static::Escalate,
        }
    }

    /// Canonical intervention digest for TB-7 rule 6 (intervention request
    /// ids obey the tuple law: same id + same digest ⇒ same receipt).
    pub fn canonical_digest(&self) -> String {
        let value = serde_json::to_value(self).expect("InterventionV1 serializes");
        memcore::canonical_digest::canonical_json_digest_hex(&value)
    }
}

/// Stop mode for `request_stop` and the stop-alias interventions.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StopMode {
    /// Graceful stop: ask the work to wind down.
    Graceful,
    /// Hard cancel: terminate.
    Hard,
}

impl StopMode {
    /// Wire token for the mode.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Graceful => "graceful",
            Self::Hard => "hard",
        }
    }
}

/// The multi-stage stop fact (TB-12): requested → forwarded → confirmed →
/// terminal. The projection NEVER mints `cancelled` without an
/// authoritative lifecycle-owner confirmation; owner disappearance after
/// possible side effects maps to `outcome_unknown`, never optimistic
/// success/failure/cancel.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StopStage {
    /// Stop requested and bound (not yet forwarded).
    Requested,
    /// Forwarded to the lifecycle owner (still not cancelled).
    Forwarded,
    /// The lifecycle owner authoritatively confirmed the terminal
    /// cancellation.
    Confirmed,
    /// The owner disappeared after possible side effects — outcome unknown.
    OutcomeUnknown,
}

/// One `request_stop` receipt (TB-12). Exactly one stop receipt type exists;
/// the stop variants of `intervene` return this payload (TB-11 type
/// connection).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StopReceipt {
    /// The task stopped.
    pub task_ref: TaskRef,
    /// Tachi-minted stop operation id.
    pub stop_id: String,
    /// `graceful` or `hard`.
    pub mode: StopMode,
    /// Current stage of the multi-stage stop fact.
    pub stage: StopStage,
    /// The RequestId this stop was idempotency-bound to.
    pub request_id: String,
}

/// Typed intervention receipt envelope. The stop variants carry exactly the
/// [`StopReceipt`] of TB-12 (one stop authority, one receipt type).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum InterventionReceipt {
    /// Additional context was forwarded.
    ContextProvided {
        /// Tachi-minted intervention id.
        intervention_id: String,
    },
    /// Correction was requested.
    CorrectionRequested {
        /// Tachi-minted intervention id.
        intervention_id: String,
    },
    /// Continuation was requested (recorded as continuation — never
    /// independent review; TB-17).
    ContinuationRequested {
        /// Tachi-minted intervention id.
        intervention_id: String,
    },
    /// User input was requested.
    UserInputRequested {
        /// Tachi-minted intervention id.
        intervention_id: String,
    },
    /// Pause was forwarded.
    Paused {
        /// Tachi-minted intervention id.
        intervention_id: String,
    },
    /// Resume was forwarded.
    Resumed {
        /// Tachi-minted intervention id.
        intervention_id: String,
    },
    /// Stop-alias interventions resolve to the single stop authority
    /// (TB-11/TB-12).
    Stop(StopReceipt),
}

/// Typed intervention/stop failure surface.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum InterventionError {
    /// The lifecycle owner does not support the requested operation — typed
    /// refusal, ZERO state mutation, NO fresh-task fallback (TB-11).
    #[error("unsupported_by_lifecycle_owner")]
    UnsupportedByLifecycleOwner {
        /// The refused operation.
        operation: InterventionV1Static,
    },
    /// The operation maps to NEW task/adjudication lineage (tachi#1623/
    /// #1675), which this leaf does not fabricate: typed refusal, zero
    /// mutation, no fresh-task fallback.
    #[error("operation requires new task/adjudication lineage (not a session intervention)")]
    RequiresNewTaskLineage {
        /// The refused operation.
        operation: InterventionV1Static,
    },
    /// `expected_task_revision` mismatch — typed conflict, never a
    /// best-effort apply (TB-11).
    #[error("expected_task_revision mismatch: expected {expected}, snapshot is {actual}")]
    RevisionConflict {
        /// The revision the caller expected.
        expected: u64,
        /// The snapshot's actual revision.
        actual: u64,
    },
    /// Same `(requester, request_id)` bound to a different digest (TB-7
    /// rule 3, applied to interventions per rule 6).
    #[error("request id conflict: {0}")]
    RequestIdConflict(#[source] super::idempotency::RequestConflict),
    /// The task does not exist, or the requester does not own it (both map
    /// to this refusal — existence is not leaked to non-owners).
    #[error("task not found")]
    NotFound,
    /// The requester is not admitted by the authority source.
    #[error("requester not admitted")]
    RequesterNotAdmitted,
    /// An intervention text matched a TB-4 forbidden category.
    #[error("intervention rejected: {category} in field `{field}`")]
    ForbiddenContent {
        /// The matched category.
        category: super::admission::ForbiddenCategory,
        /// The offending field.
        field: &'static str,
    },
    /// The request tuple is bound but its receipt has not materialized
    /// (ambiguous in-flight window — the parallel of submit's
    /// `ReconciliationUnknown`).
    #[error("intervention pending reconciliation")]
    ReconciliationUnknown,
    /// The lifecycle owner disappeared while the request was in flight;
    /// nothing was mutated (the stop path records disappearance as a fact
    /// instead — TB-12).
    #[error("lifecycle owner disappeared")]
    OwnerDisappeared,
    /// The bridge transport/truth source is unavailable (TB-20).
    #[error("bridge unavailable")]
    Unavailable,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_vocabulary_is_exactly_the_frozen_ten() {
        assert_eq!(INTERVENTION_V1_OPERATIONS.len(), 10);
        // Each discriminant round-trips from exactly one variant shape.
        let samples: Vec<(InterventionV1, InterventionV1Static)> = vec![
            (
                InterventionV1::ProvideAdditionalContext {
                    note: BoundedText::new("n").expect("bounded"),
                },
                InterventionV1Static::ProvideAdditionalContext,
            ),
            (
                InterventionV1::RequestCorrection {
                    note: BoundedText::new("n").expect("bounded"),
                },
                InterventionV1Static::RequestCorrection,
            ),
            (
                InterventionV1::RequestContinuation {
                    note: BoundedText::new("n").expect("bounded"),
                },
                InterventionV1Static::RequestContinuation,
            ),
            (
                InterventionV1::RequestIndependentReview {
                    independence_class: super::super::wire::IndependenceClass::HumanReview,
                },
                InterventionV1Static::RequestIndependentReview,
            ),
            (
                InterventionV1::RequestUserInput {
                    prompt: BoundedText::new("q").expect("bounded"),
                },
                InterventionV1Static::RequestUserInput,
            ),
            (
                InterventionV1::RequestPause,
                InterventionV1Static::RequestPause,
            ),
            (
                InterventionV1::RequestResume,
                InterventionV1Static::RequestResume,
            ),
            (
                InterventionV1::RequestGracefulStop {
                    reason: BoundedText::new("r").expect("bounded"),
                },
                InterventionV1Static::RequestGracefulStop,
            ),
            (
                InterventionV1::RequestHardCancel {
                    reason: BoundedText::new("r").expect("bounded"),
                },
                InterventionV1Static::RequestHardCancel,
            ),
            (
                InterventionV1::Escalate {
                    reason: BoundedText::new("r").expect("bounded"),
                },
                InterventionV1Static::Escalate,
            ),
        ];
        for (variant, discriminant) in samples {
            assert_eq!(variant.discriminant(), discriminant);
        }
    }

    #[test]
    fn stop_stage_never_overstates() {
        // TB-12 law encoded structurally: Requested and Forwarded are
        // distinct from Confirmed; only Confirmed is a terminal cancel.
        assert_ne!(StopStage::Requested, StopStage::Confirmed);
        assert_ne!(StopStage::Forwarded, StopStage::Confirmed);
    }
}
