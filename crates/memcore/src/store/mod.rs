//! Per-domain `impl MemoryStore` extension blocks.
//!
//! Background: `MemoryStore` (defined in `crate::lib`) is the single
//! handle exposed to language bindings. Historically every domain
//! (hub, sandbox, vault, audit, …) added its thin
//! delegation methods directly to the same `impl` block, which grew the
//! root `lib.rs` past 1100 lines of repetitive shim code.
//!
//! This module groups those delegations by domain. Each submodule
//! contains exactly one `impl MemoryStore { … }` block; Rust merges
//! them at compile time, so the public API is unchanged.
//!
//! **Admin domains** (`hub`, `vault`, `sandbox`, `llm_usage`,
//! `dispatch_outcomes`) compile only with the `admin` feature (default on).
//! Portable builds omit those methods — and since #1585 a
//! `StoreProfile::PortableKernel` database does not even carry the tables
//! behind them, so compiling them in would be a promise the schema cannot
//! keep.
//!
//! Adding a new domain:
//!   1. Create `store/<domain>.rs` with `use super::super::*;` then a
//!      single `impl MemoryStore` block.
//!   2. `pub mod <domain>;` here (gate with `cfg(feature = "admin")`
//!      if the domain is operator-only).
//!   3. Implementations may freely access `self.conn`, `self.db_label`,
//!      etc. — those fields are `pub(crate)`.

pub mod agent_state;
#[cfg(feature = "admin")]
pub mod audit;
pub mod crud;
pub mod daily_pipeline;
pub mod derived;
#[cfg(feature = "admin")]
pub mod dispatch_outcomes;
pub mod distill;
pub mod enrichment;
pub mod events;
pub mod exact_dedupe;
pub mod gc_candidates;
pub mod graph;
#[cfg(feature = "admin")]
pub mod hub;
pub mod immutable_supersession;
pub mod lessons;
pub mod lifecycle_consistency;
pub mod linking;
#[cfg(feature = "admin")]
pub mod llm_usage;
pub mod maintenance;
pub mod memory_lifecycle;
pub mod open;
pub mod outbox;
pub mod outbox_destination_apply;
pub mod outbox_protocol;
pub mod policy;
#[cfg(test)]
mod profile_identity_tests;
pub mod recall_cache;
pub mod rem;
#[cfg(feature = "admin")]
pub mod sandbox;
pub mod snapshot_import;
pub mod state;
pub mod tasks;
#[cfg(feature = "admin")]
pub mod vault;
