//! Leaf utilities and capability traits carved out of `tachi-server`
//! (#2077 stage 2, PR C).
//!
//! House rules: this crate holds only leaf modules with no
//! `tachi-server` dependency, plus the `MemoryServer` capability traits.
//! It must never depend on `tachi-server` or on a domain crate; the
//! allowlist contract test in `tachi-contract-tests` enforces that.

#[doc(hidden)]
pub mod daemon_lock;
#[doc(hidden)]
pub mod network_safety;
#[doc(hidden)]
pub mod physical_db_identity;
#[doc(hidden)]
pub mod task_control;

mod capabilities;

pub use capabilities::{LlmAccess, RuntimeHome, StoreAccess, VaultAccess};
