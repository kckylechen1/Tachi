//! CurrentTruth v1 — revisioned GitHub assertions, reduction, stale
//! handoffs, and the derived open-action projection (#1696, first
//! executable slice of #1297; canonical spec:
//! `docs/engineering/architecture/tachi-continuity-memory-architecture.md`
//! §1.2–1.3).
//!
//! # What this is
//!
//! One append-only assertion authority for typed GitHub-object facts, one
//! deterministic reducer emitting exactly `current | superseded |
//! conflicted | unknown`, one disposable/rebuildable projection, and one
//! consumer read surface (#1693 boundary).
//!
//! # What this is not
//!
//! Not a timeline narrative, not a local GitHub shadow authority, not a
//! deployment tracker, not a precedent engine, not a planner, and not a
//! model-generated truth system. The reducer and the open-action
//! projection invoke no model and execute no external action. There is no
//! second board/status/current-truth store here or anywhere else — the
//! existing flow artifacts (`status.json`, kanban, cycle status) keep their
//! scopes untouched.
//!
//! # Hard rules enforced by this module
//!
//! * **Source revision ≠ arrival order.** Ordering keys are source-supplied
//!   (`observed_at`, immutable source revision, assertion id). The store
//!   records arrival for operations only and the reducer never sees it.
//! * **No title/body similarity.** Issue/PR links come only from typed
//!   adapter relations or reviewed dispositions.
//! * **A merged PR does not close an issue or establish owner acceptance**;
//!   **an open issue does not mean implementation is absent** (#1297 worked
//!   example B).
//! * **Conflicting authoritative facts stay conflicted** and block
//!   success-shaped projection.
//! * **Model prose is candidate evidence only** — it is stored (history with
//!   provenance) but never admitted by the reducer.
//! * **Unavailable refresh ⇒ unknown/stale posture**, never a fabricated
//!   current result.

pub mod consumer;
pub mod handoff;
pub mod projection;
pub mod reducer;
pub mod refresh;
pub mod store;
pub mod types;

#[cfg(test)]
mod tests;
