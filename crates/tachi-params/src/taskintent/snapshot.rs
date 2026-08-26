//! `TaskSnapshot` — the `get` read projection (tachi#1840, zeroclaw #205
//! TB-8/TB-15).
//!
//! Lifecycle authority is NEVER sourced from `tachi_task` read-model rows
//! (TB-8: `tachi_task` is a unified work READ model, not a worker-status
//! authority — `crates/tachi-params/src/facade/task.rs` documents its own
//! typed rejection of worker-state reads). The snapshot's state fields
//! derive from the three TB-16 dimension mappings over canonical facts;
//! `lifecycle_mode`, lifecycle-owner identity, and
//! `supported_interventions` are a typed, revisioned advertisement on the
//! snapshot (TB-15) — support/refusal is inspectable, never implied.

use serde::{Deserialize, Serialize};

use super::intervention::InterventionV1Static;
use super::mapping::adjudication::AdjudicationState;
use super::mapping::delivery::DeliveryState;
use super::mapping::execution::ExecutionState;
use super::plan::{ExecutionPlanProjection, LifecycleMode};
use super::refs::TaskRef;

/// Typed get failure.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum GetError {
    /// The task does not exist.
    #[error("task not found")]
    NotFound,
    /// The bridge truth source is unavailable (TB-20).
    #[error("bridge unavailable")]
    Unavailable,
}

/// The `get` read projection over canonical truth.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskSnapshot {
    /// The task identity.
    pub task_ref: TaskRef,
    /// Snapshot revision — monotonic over the task's fact count; a change
    /// to ANY projected dimension (including the capability advertisement)
    /// bumps it (TB-15 check).
    pub task_revision: u64,
    /// The execution-dimension projection (TB-16 mapping table output).
    pub execution: ExecutionState,
    /// The adjudication-dimension projection.
    pub adjudication: AdjudicationState,
    /// The delivery-dimension projection (pull-only V2).
    pub delivery: DeliveryState,
    /// The currently admitted ExecutionPlan projection (TB-1/TB-2).
    pub plan: Option<ExecutionPlanProjection>,
    /// Lifecycle mode owner of execution (TB-15).
    pub lifecycle_mode: Option<LifecycleMode>,
    /// Lifecycle-owner identity (backend/adapter/specialist id).
    pub lifecycle_owner: Option<String>,
    /// The typed, revisioned intervention advertisement (TB-15): the
    /// authoritative admission list. Every unsupported operation ⇒ typed
    /// refusal (TB-11).
    pub supported_interventions: Vec<InterventionV1Static>,
    /// The intent-derived contract projection (TB-2: plan-independent).
    pub contract: Option<super::events::IntentContractProjection>,
    /// Canonical intent digest for this task (TB-7).
    pub intent_digest: String,
}
