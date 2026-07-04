//! Per-domain `impl MemoryStore` extension blocks.
//!
//! Background: `MemoryStore` (defined in `crate::lib`) is the single
//! handle exposed to language bindings. Historically every domain
//! (hub, sandbox, pack, vault, audit, …) added its thin
//! delegation methods directly to the same `impl` block, which grew the
//! root `lib.rs` past 1100 lines of repetitive shim code.
//!
//! This module groups those delegations by domain. Each submodule
//! contains exactly one `impl MemoryStore { … }` block; Rust merges
//! them at compile time, so the public API is unchanged.
//!
//! Adding a new domain:
//!   1. Create `store/<domain>.rs` with `use super::super::*;` then a
//!      single `impl MemoryStore` block.
//!   2. `pub mod <domain>;` here.
//!   3. Implementations may freely access `self.conn`, `self.db_label`,
//!      etc. — those fields are `pub(crate)`.

pub mod agent_state;
pub mod audit;
pub mod crud;
pub mod derived;
pub mod distill;
pub mod domain;
pub mod enrichment;
pub mod events;
pub mod graph;
pub mod hub;
pub mod linking;
pub mod llm_usage;
pub mod maintenance;
pub mod open;
pub mod pack;
pub mod recall_cache;
pub mod rem;
pub mod sandbox;
pub mod state;
pub mod tasks;
pub mod vault;
