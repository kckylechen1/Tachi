//! Safe-merge gate logic for `tachi_gh safe_merge`.
//!
//! This module is split into two layers:
//!
//! 1. **Pure decision logic** — `evaluate_merge_gate(&PrState) -> MergeDecision`.
//!    No I/O, no clock, no `gh` subprocess. Fully unit-testable from struct
//!    literals. This is the "safe" part of safe-merge: a deterministic,
//!    auditable function that decides whether a PR is allowed to merge.
//!
//! 2. **`GhClient` trait** — the GitHub-side I/O surface that
//!    `tachi_gh safe_merge` needs to drive the gate end-to-end:
//!      - `pr_view`         — fetch latest PR state for a gate evaluation
//!      - `pr_merge`        — actually merge (only called when gate is Ready)
//!      - `issue_view`      — fetch linked issue state (e.g. for closes-link)
//!      - `issue_create`    — open a tracking issue from a brainstorm flow
//!      - `checks_list`     — granular per-check status for events log
//!
//!    Two impls are provided:
//!      - `CliGhClient` — wraps the existing sanitized `gh` subprocess in
//!        `gh_ops` (production path).
//!      - `MockGhClient` — in-memory fixture builder for integration tests
//!        of the orchestrator without spawning `gh` or hitting the network.
//!
//! Wiring into the `tachi_gh` MCP action lands in the next commit; the trait
//! and decision function are shipped here with full unit-test coverage so
//! the orchestrator commit can land green in one shot.
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
#[cfg(test)]
mod mock;
#[cfg(test)]
mod tests;
mod types;

#[allow(unused_imports)]
pub use client::{GhClient, GhError, IssueState, MergeResult, MergeStrategy};
#[cfg(test)]
pub(crate) use gate::evaluate_merge_gate;
pub use gate::evaluate_merge_gate_with_policy;
#[cfg(test)]
pub(crate) use mock::MockGhClient;
#[allow(unused_imports)]
pub use types::{
    CheckRun, ChecksState, MergeDecision, MergeGatePolicy, MergeGatePolicyMode, Mergeable,
    PrLifecycleState, PrState, ReviewDecision,
};
