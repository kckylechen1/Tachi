//! D2 precedent-shaped memory forge + 50-row blinded cold-start A/B (#1073).
//!
//! Frozen contract: `kckylechen1/tachi#1073`. Design authority:
//! `docs/engineering/architecture/issue-refinery-memory-lanes.md` §8.
//!
//! ## What this module is
//!
//! - [`pilot`] — freeze exactly the 50-row pilot manifest (spend gate:
//!   nothing forges outside a frozen manifest).
//! - [`forge`] — validate a model-authored draft against the frozen
//!   contract's hard requirements and assign deterministic pending
//!   candidate identity. Does not call a model itself (see that module's
//!   doc for why).
//! - [`discrimination`] — blind + score one pilot case's 3 treated / 3
//!   baseline cold runs against the frozen contract's 4 pass criteria,
//!   with a structural dual-track-independence guarantee.
//! - [`runner`] — the only authoritative report path, requiring exact
//!   accounting for all 400 uniquely keyed calls.
//! - [`report`] — legacy, non-authoritative preview aggregation. It cannot
//!   render or claim D2 pilot completion.
//! - `storage` (crate-private) — persist a forged candidate as a pending
//!   row, never an established `/precedents` row.
//!
//! ## What this module explicitly does NOT do (honestly, not silently)
//!
//! It does not call a live model to author a candidate draft, run a cold
//! task, or adjudicate one — none of those are available inside a bounded,
//! non-interactive implementer session with no live-model-spend authority.
//! Every one of those calls is a seam this module defines
//! (`ForgeDraft`, `ColdRunText`/`ColdRunScore`, `AdjudicatorReceipt`) so a
//! caller with that authority (a follow-up harness runner, wired through
//! whatever dispatch/engine call is appropriate) can plug in a real
//! producer/cold-task/adjudicator without this module's validation, spend
//! gate, blinding, or scoring logic changing at all. Running the actual
//! 50-row pilot against real models and publishing its authoritative runner report is
//! explicitly NOT claimed as done by this leaf — see the PR description's
//! not-done section.

pub mod discrimination;
pub mod forge;
pub mod pilot;
pub mod privacy;
pub mod progress;
pub mod report;
pub mod runner;
pub mod source;
// `storage` is `pub(crate)`, not `pub`: its one entry point takes
// `&crate::MemoryServer`, which is itself `pub(crate)` — a public function
// can never expose a less-visible type in its signature (E0446), so this
// module cannot be widened the way its siblings were without also widening
// `MemoryServer`'s own visibility, which is out of this leaf's scope.
pub(crate) mod storage;
