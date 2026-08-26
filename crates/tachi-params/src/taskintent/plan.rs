//! `ExecutionPlanV1` projection (tachi#1840, zeroclaw #205 TB-1/TB-2/
//! TB-15).
//!
//! A PROJECTION, not a second canonical ledger: the bridge module owns no
//! DDL and stores nothing — every field of
//! [`ExecutionPlanProjection`] binds to an existing Tachi primitive or its
//! revision/digest:
//!
//! * `work_claim_id` / `transition_version` → `session_claims` WorkClaim
//!   truth (`crates/memcore/src/db/session_claims.rs`);
//! * `exec_env_id` → the ExecEnv plane (`bind_work_claim_exec_env`);
//! * `dispatch_id` → the dispatch kernel (`LaunchSpec`/`StaffRunReceipt`
//!   spine, `crates/tachi-params/src/facade/dispatch.rs`);
//! * `lifecycle_mode`/`backend` → the TB-15 mode of the owning lane
//!   (#1676 managed / #1678 attached / #1636 specialist).
//!
//! TB-2: Tachi re-targets freely — one TaskIntent under two different
//! admitted plans yields different plan identities with an observably
//! equivalent intent-derived contract projection (which lives on the
//! `TaskSubmitted` event, not here).

use serde::{Deserialize, Serialize};

use super::refs::TaskRef;

/// Lifecycle modes (TB-15). Capability advertisement
/// (`supported_interventions`) is carried on the [`super::snapshot`]
/// `TaskSnapshot`, typed and revisioned.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LifecycleMode {
    /// Tachi-managed headless batch lane (tachi#1676): typed `LaunchSpec`,
    /// ExecEnv/grants, process handle, cancel/timeout/result/cleanup
    /// receipts.
    TachiManagedBatch,
    /// Harness-native attached session (tachi#1678): the host owns
    /// spawn/session/wait/cancel; Tachi attaches WorkClaim identity and
    /// ingests authoritative lifecycle facts.
    HarnessNativeAttached,
    /// Specialist-owned environment (tachi#1636 contract only): the
    /// specialist's adapter declares identity/capabilities/protocol and
    /// produces receipts.
    SpecialistOwned,
}

impl LifecycleMode {
    /// The static intervention advertisement baseline for the mode; the
    /// snapshot combines this with owner-declared capability facts
    /// (attachment `session_capabilities`, managed-backend cancel support).
    pub fn baseline_supported_interventions(
        self,
    ) -> &'static [super::intervention::InterventionV1Static] {
        use super::intervention::InterventionV1Static as Op;
        match self {
            // The managed lane owns the process: stop operations are
            // forwardable to the backend (tachi#1676/#1825 cancellation
            // lane); prompt/pause/resume are not managed-lane operations.
            LifecycleMode::TachiManagedBatch => &[Op::RequestGracefulStop, Op::RequestHardCancel],
            // Attached sessions advertise exactly what the harness adapter
            // declared (#1678 closed capability set); the static baseline is
            // empty — only observed capabilities advertise support.
            LifecycleMode::HarnessNativeAttached => &[],
            // Specialist contract only: nothing is supported by default.
            LifecycleMode::SpecialistOwned => &[],
        }
    }
}

/// A versioned `ExecutionPlanV1` projection over existing staffing truth
/// (TB-1). Constructed by the staffing admission port when a plan is
/// admitted; every identity here is canonical-Tachi-minted.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExecutionPlanProjection {
    /// Tachi-minted plan identity (projection id, not a stored ledger row).
    pub plan_ref: String,
    /// The task this plan serves.
    pub task_ref: TaskRef,
    /// Lifecycle mode owning execution.
    pub lifecycle_mode: LifecycleMode,
    /// Lifecycle owner identity (backend name, adapter identity, or
    /// specialist id).
    pub lifecycle_owner: String,
    /// Bound WorkClaim id (existing `session_claims.claim_id`), if the plan
    /// holds a claim.
    pub work_claim_id: Option<String>,
    /// WorkClaim transition revision bound at admission
    /// (`transition_version` — compare-and-swap truth).
    pub work_claim_transition_version: Option<i64>,
    /// Bound ExecEnv id, if the plan placed execution in an environment.
    pub exec_env_id: Option<String>,
    /// Bound dispatch id (dispatch kernel spine), if launched.
    pub dispatch_id: Option<String>,
    /// Backend token for managed lanes (e.g. `custom`), else `None`.
    pub backend: Option<String>,
    /// Digest of the launch contract (LaunchSpec-shaped canonical digest),
    /// when a launch exists.
    pub launch_digest: Option<String>,
    /// Plan revision (bumped when the staffing plane re-admits/re-targets).
    pub revision: u64,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn managed_lane_advertises_only_stop_ops() {
        let ops = LifecycleMode::TachiManagedBatch.baseline_supported_interventions();
        assert!(
            ops.contains(&super::super::intervention::InterventionV1Static::RequestGracefulStop)
        );
        assert!(!ops.contains(&super::super::intervention::InterventionV1Static::RequestPause));
        // Attached and specialist baselines are empty — support must be
        // owner-declared, never implied (TB-15).
        assert!(LifecycleMode::HarnessNativeAttached
            .baseline_supported_interventions()
            .is_empty());
        assert!(LifecycleMode::SpecialistOwned
            .baseline_supported_interventions()
            .is_empty());
    }
}
