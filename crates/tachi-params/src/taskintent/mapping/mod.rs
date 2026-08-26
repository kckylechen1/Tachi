//! TB-16 state projections: THREE per-dimension mapping tables over
//! canonical Tachi state — `execution`, `adjudication`, `delivery` — one
//! module each, never one merged table, never an independent status enum
//! defined outside them, and cross-dimension transitions never rewrite each
//! other (tachi#1636 law).
//!
//! Inputs to these tables are EXISTING canonical Tachi truth surfaces
//! (documented per mapping): `dispatch_outcomes.execution_outcome`, the
//! `StaffRunReceipt.state` vocabulary, `session_claims` `ClaimState`, the
//! #1679 frozen three-dimension model, adjudication verdicts from
//! `dispatch_adjudications`. Outputs are the bridge-visible alias
//! vocabularies, defined here and nowhere else.

pub mod adjudication;
pub mod delivery;
pub mod execution;
