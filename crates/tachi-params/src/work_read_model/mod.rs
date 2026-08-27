//! Unified Work Read Model (#1693) — one rebuildable projection over the
//! work authorities, built on the just-landed CurrentTruth consumer
//! surface (#1696 / PR #1864).
//!
//! # What this is
//!
//! One typed read model joining, per work item: the CurrentTruth consumer
//! view (GitHub issue/PR/merge state as reconciled evidence heads), WorkClaim
//! leases (#1239), staffing run receipts (#1692/#1623), ExecEnv identity
//! (#894/#1118), verification evidence, adjudication-spine facts (#1636),
//! and delivery. `next_action` is a typed projection of established state
//! and prerequisites — never an LLM plan.
//!
//! # What this is not
//!
//! Not a truth store, not a second ledger, not a board/status/task store,
//! not a GitHub shadow authority, not a planner, and not an executor. The
//! projection consumes typed [`sources::SourceSnapshot`]s minted by each
//! authority's own read adapter and produces plain data:
//!
//! * **No write path back to any source.** The projector holds no store
//!   handle and exposes no mutation; a projection can be dropped and
//!   rebuilt from the same snapshots without any authority noticing.
//! * **Full rebuild == incremental build.** Both fold snapshots through the
//!   same no-regression `apply`; arrival order is never an input.
//! * **Honest degradation.** Missing sources stay `Unavailable`; delivery
//!   reports `not_integrated` until #1679 lands — a pending-delivery table
//!   is never fabricated.
//! * **Conflicts block success-shaped projection**; unknown is never
//!   success-shaped; `implemented != merged != accepted != owner_closed`
//!   (#1297 law, consumed via CurrentTruth).
//! * **Visibility is fail-closed.** Private work is hidden entirely;
//!   health counts cover the visible set only (#1693 discrimination 12).
//!
//! # R6-2 transition debt (owner-ruled)
//!
//! The owner has ruled the transition-vs-state semantics on #1693:
//! `merge_reverted` and `issue_reopened` are **transition facts** that a
//! later steady-state snapshot must NOT clear, even when the frozen
//! max-key family law lets that newer node/state snapshot win the
//! CurrentTruth lifecycle resolution. Debt clears only through the causal
//! resolutions the ruling names (a later authoritative `issue_closed`; a
//! post-revert merged repair PR explicitly linked to the issue; an
//! explicit owner-reviewed `no_repair_required` disposition observed
//! strictly after the revert). Clearing evidence must be **admissible**:
//! a `Conflicted` row is retained contradiction evidence and never
//! discharges debt. A reverted PR that no issue's current link set claims
//! keeps its debt as its own attributable, blocked work item (unlinking
//! is not a causal resolution). See [`types::TransitionDebtV1`] and
//! `transition_debt_for` in `projector.rs`. The #1696 assertion history
//! and reducer are untouched.

pub mod projector;
pub mod sources;
pub mod types;
pub mod views;

#[cfg(test)]
mod tests;

pub use projector::{project, rebuild, ApplyOutcome, WorkProjectionIndex};
pub use sources::{
    AdjudicationFactV1, ClaimModeV1, ClaimStateV1, CurrentTruthFactsV1, DeliveryObservationV1,
    ExecEnvFactV1, OwnerDispositionFactV1, OwnerDispositionV1, RunReceiptFactV1, SnapshotError,
    SourceFacts, SourceKind, SourceSnapshot, SourceStamp, VerificationFactV1, WorkClaimFactV1,
};
pub use types::{
    AdjudicationSectionV1, BlockerKindV1, BlockerV1, ClaimRowV1, ClaimSectionV1, DebtClearingV1,
    DebtStateV1, DeliverySectionV1, ExecEnvRowV1, ExecEnvSectionV1, ExecutionStateV1,
    GithubSectionV1, HeadDriftV1, ImplementationStatusV1, NextActionKindV1, NextActionV1,
    ProjectionOptions, ProjectionOptionsError, RequiredAuthorityV1, RunRowV1, RunSectionV1,
    SectionState, TransitionDebtV1, VerificationRowV1, VerificationSectionV1, WorkKey,
    WorkProjectionHealthV1, WorkReadModelSetV1, WorkReadModelV1,
};
pub use views::{
    board_view, brief_view, status_view, WorkBoardRowV1, WorkBriefV1, WorkStatusRowV1,
};
