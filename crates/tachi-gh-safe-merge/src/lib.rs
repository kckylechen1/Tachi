//! Pure GitHub safe-merge contracts for `tachi_gh safe_merge`.
//!
//! This crate owns the deterministic, side-effect-free gate, the serializable
//! PR/check/decision types, and the `GhClient` trait contract. The gate has no
//! I/O, clock, or `gh` subprocess dependency: callers provide a `PrState`, and
//! `evaluate_merge_gate` decides whether a PR is allowed to merge.
//!
//! The server boundary stays in `tachi-server`:
//!
//! - `gh_ops/safe_merge/cli.rs` and `gh_ops/safe_merge/http.rs` own the
//!   `CliGhClient` and other GitHub transports;
//! - `gh_ops/safe_merge/handler.rs` and its neighboring modules own the
//!   `tachi_gh safe_merge` orchestration, verification, events, and ledger
//!   effects.
//!
//! `GhClient` is the narrow contract those server paths consume:
//!
//! - `pr_view` fetches the latest PR state for a gate evaluation;
//! - `pr_merge` performs a merge only after the caller has a `Ready` decision;
//! - `issue_create` opens a tracking issue from a brainstorm flow;
//! - `checks_list` reads the granular check status used by the event/log path.
//!
//! `MockGhClient` is an in-memory fixture for this crate's tests and for
//! downstream tests that explicitly enable the non-default `test-support`
//! feature. It never spawns `gh` or contacts the network.
//!
//! Section 五 of `tachi-shell-github-convoy-touchy-agent-prompt.md` is the
//! authoritative spec for the `merge_state` vocabulary and event kinds; the
//! `MergeDecision` variants here map 1:1 to that vocabulary:
//!
//! - `Ready`              ↔ `merge_state = "ready"`           (no events emitted, caller decides)
//! - `Blocked { .. }`     ↔ `merge_state = "blocked"`         (`github_merge_blocked` event)
//! - `Pending { .. }`     ↔ `merge_state = "pending"`         (no events, just keep polling)

mod client;
mod gate;
#[cfg(any(test, feature = "test-support"))]
mod mock;
#[cfg(test)]
mod tests;
mod types;

pub use client::{GhClient, GhError, IssueState, MergeResult, MergeStrategy};
#[cfg(test)]
pub(crate) use gate::evaluate_merge_gate;
pub use gate::evaluate_merge_gate_with_policy;
#[cfg(any(test, feature = "test-support"))]
pub use mock::MockGhClient;
pub use types::{
    CheckRun, ChecksState, ClosingIssueLabels, MergeDecision, MergeGatePolicy, MergeGatePolicyMode,
    Mergeable, PrLifecycleState, PrState, ReviewDecision,
};
