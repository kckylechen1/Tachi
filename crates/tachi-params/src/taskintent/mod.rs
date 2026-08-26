//! Host TaskIntent bridge (tachi#1840, vertical V2a of the gated-open
//! program; frozen wire contract zeroclaw #205 rev 3).
//!
//! Assembles EXISTING Tachi primitives — WorkClaim/ExecEnv/dispatch truth
//! ([`memcore`] rows via the fact-source ports), the
//! `dispatch_outcomes`/`dispatch_adjudications` spines, the attached-session
//! vocabulary — into the bridge surface:
//!
//! ```text
//! submit(TaskIntentV1, RequestId)      -> SubmitReceipt
//! get(TaskRef)                         -> TaskSnapshot
//! watch(TaskRef, after_seq)            -> TaskEventPage
//! intervene(TaskRef, InterventionV1, RequestId, expected_task_revision?)
//!                                       -> InterventionReceipt
//! request_stop(TaskRef, mode, RequestId, expected_task_revision?)
//!                                       -> StopReceipt
//! collect(TaskRef, result_revision?)   -> ResultProjectionV1
//! ```
//!
//! `TaskRef`, the `ExecutionPlanV1` projection, and `ResultProjectionV1`
//! are facades/projections over existing truth — NOT new ledgers (TB-1 +
//! owner hard law): this module owns no DDL and creates no store. Open
//! decisions carried per the contract's deny-by-default rule:
//!
//! * **DECISION (OPEN) TB-5/A** — capability catalog: shipped as a CLOSED
//!   enum ([`wire::Capability`]); an owner flip to a runtime-admitted
//!   catalog is a deliberate golden-changing PR. The variant set was
//!   extended once by the owner's surfaced override (2026-08-26:
//!   `repository_implementation`, the ratified V-program acceptance
//!   capability — see [`wire::Capability`] docs).
//! * **DECISION (OPEN) TB-7/B** — interim idempotency carrier: NO interim
//!   journal shipped; see [`idempotency`] for the honest record of what the
//!   in-process binding can and cannot survive (the Tachi-restart half
//!   awaits tachi#1623 or an owner-picked carrier).
//!
//! Golden: `golden/task-intent.v1.json` pins the wire (this leaf owns the
//! Tachi decoder half; the ZeroClaw encoder half and cross-repo round trip
//! are V2b, zeroclaw #234).

pub mod admission;
pub mod bridge;
pub mod events;
pub mod idempotency;
pub mod intervention;
pub mod mapping;
pub mod memcore_ingest;
pub mod plan;
pub mod refs;
pub mod result;
pub mod snapshot;
pub mod wire;

pub use admission::{AdmissionRejection, AdmittedAuthority, ForbiddenCategory};
pub use bridge::{
    BridgeClock, InMemoryTaskFacts, LifecycleOwnerPort, OwnerForwardResult, PlanAdmissionError,
    PlanAdmissionPort, RequesterAuthorityError, RequesterAuthorityPort, SubmitReceipt,
    SystemBridgeClock, TaskFactSource, TaskIntentBridge, UnavailableError, UnavailableTaskFacts,
};
pub use events::{
    EventSource, ExpectedArtifactProjection, IntentContractProjection, OutcomeObservation,
    TaskEvent, TaskEventPage, TaskEventPayload, VisibilityClass,
};
pub use idempotency::{
    BoundRef, InProcessRequestBindings, RequestBinding, RequestBindingStore, RequestConflict,
};
pub use intervention::{
    InterventionError, InterventionReceipt, InterventionV1, InterventionV1Static, StopMode,
    StopReceipt, StopStage, INTERVENTION_V1_OPERATIONS,
};
pub use plan::{ExecutionPlanProjection, LifecycleMode};
pub use refs::{
    AttemptRef, ConversationSessionRef, DeliveryIntentRef, HarnessSessionRef, ParentRunRef,
    ProcedureRunRef, RefError, RequestId, RequesterRef, SubAgentRunRef, TaskRef,
};
pub use result::{
    CollectError, ContractViolation, ProvenanceProjection, ResultProjectionV1, VerificationSummary,
};
pub use snapshot::{GetError, TaskSnapshot};
#[cfg(test)]
mod bridge_tests;

pub use wire::{
    ApprovalRequirement, ArtifactClass, ArtifactExpectation, BoundedText, Capability,
    CapabilityRequest, EvaluationRequirement, IndependenceClass, PrivacyClass, RoutingPreference,
    SourceKind, SourceRef, TaskConstraint, TaskIntentV1, Timestamp, WireError, WorkspaceSourceRef,
    BOUNDED_TEXT_MAX, SCHEMA_TAG,
};

/// The checked-in golden vector pinning the `task-intent.v1` wire. V2b
/// (zeroclaw #234) consumes THIS file as its encoder target: same JSON,
/// same digest rule ([`TaskIntentV1::canonical_digest`]).
pub const GOLDEN_TASK_INTENT_V1: &str = include_str!("golden/task-intent.v1.json");
