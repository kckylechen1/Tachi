//! #1297 leaf 1 — the pure current-truth fold over the `tachi_events` ledger.
//!
//! # What this is, and what it is deliberately not
//!
//! Its subject is **external project objects** — issues, PRs, merges,
//! deployments, handoffs — and the distinction the issue exists to keep:
//!
//! ```text
//! implemented != merged != accepted != deployed(host) != owner_closed
//! ```
//!
//! It is **not** about `memories` rows. Current truth over memory rows is
//! already centrally enforced by one shared SQL predicate set
//! (`db::memory_crud::search`'s `superseded_by IS NULL` + valid-window +
//! `archived = 0` legs, e.g. `search.rs:145,147`) together with
//! `store::linking::mark_superseded_closing_validity` and the CAS refusal in
//! `store::immutable_supersession`. Adding a `truth_state` column to
//! `memories` would duplicate derivable state and manufacture a drift
//! surface; that design was rejected.
//!
//! # Schema delta: zero
//!
//! Assertions ride the existing `tachi_events` table as typed-payload events
//! (`event_type = "truth.assertion.v1"`) behind a validating builder — the
//! same shape the governed-precedent path (#950 / `tachi-server`'s
//! `governed_precedent_establishment.rs`) shipped without a new table. No
//! columns on `memories`, no new tables, no migration.
//!
//! # Shape: governed observation append + pure read-time reduction
//!
//! This module generalises the pattern already shipped for one predicate
//! family in `tachi-server`'s `rebuild_active_precedent_projection`
//! (`governed_precedent_establishment.rs:390-393` — "intentionally has no
//! write side effect, so a rebuild can neither establish nor erase a
//! precedent") from precedents to a `(subject_ref, predicate)` space.
//!
//! Everything here is **pure**: no network, no `gh`, no database handle, and
//! no wall clock except the explicit `as_of` argument callers pass to
//! [`CurrentTruthFold::project`]. Identical input in any arrival order
//! produces byte-identical output.
//!
//! # Self-referential redlines this module is built to hold
//!
//! 1. **Projection output is never written back as memory rows.** Nothing in
//!    this module takes a `Connection`. The existing continuity projections
//!    *do* write into `memories` (`tachi-server`'s
//!    `continuity_ops/projection.rs:240` calls `upsert_projection_memory`);
//!    copying that here would put reducer output into recall, where it gets
//!    ranked, access-boosted, and resurfaced stale. Renderer-fed only.
//!
//!    The append side is guarded too: [`TruthAssertionInputV1`] has no
//!    `projection_hints` field at all — the offending value is unrepresentable
//!    rather than validated away — and [`build_truth_assertion_event`] stamps
//!    `projection_hints = []` and `effects = [none]`. That is load-bearing, not
//!    decoration — the background sweep in `tachi-server`'s
//!    `continuity_ops::projection` projects an event into `memories` only when
//!    `event_projections(event)` is non-empty, and that function
//!    (`projection/entry.rs:65-74`) returns the event's own
//!    `projection_hints`, falling back to a prefix table
//!    (`inferred_projection_from_event_type`, same file, lines 35-63) that
//!    `truth.assertion.v1` matches no entry of. Empty hints + a non-matching
//!    event type is what keeps reducer *input* out of `memories`.
//!
//! 2. **Evidence gathering is typed enumeration, never semantic search.**
//!    The fold's only input is a slice of
//!    [`TachiEventRecord`](crate::types::TachiEventRecord), which callers
//!    obtain via `db::list_tachi_events` (filtered by type/domain/project).
//!    No scorer, no ranker, no embedding is reachable from here — so the
//!    reducer's input cannot inherit exposure-manufactured rank.
//!
//! 3. **Agent-authored assertions are capped at `candidate`, structurally, in
//!    two independent places.** On the way in,
//!    [`build_truth_assertion_event`] overwrites the requested authority with
//!    [`AGENT_AUTHORITY_CAP`] for any [`TruthIssuerV1::Agent`] issuer, so such
//!    a row cannot even be written carrying a decision-eligible authority
//!    column. On the way out, [`classify_assertion`] returns
//!    [`CandidateReasonV1::AgentAuthored`] for any agent issuer *before* it
//!    looks at anything else — so a row that reached the ledger by some other
//!    path still cannot decide anything. The second check is the load-bearing
//!    one; the first only removes a way to be confusing.
//!    Candidates are collected into
//!    [`CurrentTruthDiagnosticsV1::candidates`] — a surface entirely outside
//!    [`CurrentTruthProjectionV1::subjects`]. An agent-authored event
//!    therefore cannot change a truth value, and cannot even introduce a
//!    subject into the truth surface. The authority column is read from the
//!    *event row*, never from the payload, so a payload cannot self-declare
//!    its own authority.
//!
//! 4. **`unknown` / `conflicted` counts never feed ranking, save policy, or
//!    GC.** Structurally: this module has no dependency on `crate::scorer`,
//!    `crate::search`, or any GC path, and produces no writes at all.
//!
//! # Honest scope notes
//!
//! - The cap in redline 3 is *structural at the type level*: a well-formed
//!   event whose issuer says `source_snapshot` is treated as a source
//!   snapshot. Cryptographic non-forgeability of issuer identity is not in
//!   scope for leaf 1 and is not claimed here.
//! - Deployment **receipts** are out of leaf 1. [`TruthPredicate::Deployed`]
//!   exists so the projection can represent the distinction the issue's
//!   headline equation names (and report it as `unknown`), and its admission
//!   policy demands a host+service+receipt issuer — but nothing in this repo
//!   constructs one yet.
//! - Live ingestion (persisting observations where snapshots are already
//!   fetched) is **not** in this module and not in this crate. The pure half
//!   of it — deterministic append-if-absent key derivation and event
//!   construction — is [`build_truth_assertion_event`]; the live wrapper
//!   belongs in `tachi-server` alongside `facade_memory_ops::current_work_anchor`.

mod action_queue;
mod admission;
mod reduce;
mod types;

#[cfg(test)]
mod tests;

pub use action_queue::{derive_action_queue, ActionItemV1, ActionKindV1};
pub use admission::{
    build_truth_assertion_event, classify_assertion, decode_truth_assertion,
    truth_assertion_event_id, TruthAssertionInputV1, TruthAssertionPayloadV1, TruthEventEnvelopeV1,
    AGENT_AUTHORITY_CAP, TRUTH_ASSERTION_ADAPTER,
};
pub use reduce::{reduce_current_truth, CurrentTruthFold};
pub use types::{
    AssertionRefV1, AssertionRelationV1, CandidateReasonV1, CandidateRecordV1,
    CurrentTruthDiagnosticsV1, CurrentTruthError, CurrentTruthProjectionV1, CurrentTruthStatsV1,
    IssuerClass, PredicateTruthV1, RejectedAssertionV1, SubjectTruthV1, TruthAssertionV1,
    TruthIssuerV1, TruthPredicate, TruthStateV1, TruthValue, CURRENT_TRUTH_PROJECTION_VERSION,
    TRUTH_ASSERTION_DOMAIN, TRUTH_ASSERTION_EVENT_TYPE, TRUTH_ASSERTION_PAYLOAD_VERSION,
    VALUE_ISSUE_CLOSED, VALUE_ISSUE_OPEN, VALUE_NO, VALUE_YES,
};
