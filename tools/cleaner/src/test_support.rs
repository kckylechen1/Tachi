//! Shared `cfg(test)` helpers.
//!
//! `HOME` is a process-global mutable resource. Several test modules in
//! this crate (`registry`, `scrap_ledger`, `wt_clean`, `wt_open`) sandbox
//! filesystem-touching code by temporarily overriding it. Each module used
//! to keep its own private `Mutex<()>` guard — that only serializes tests
//! *within that module*. Under `cargo test`'s default multi-threaded
//! runner, a test in one module could still race a test in a different
//! module for control of `HOME`: thread A sets `HOME` to its sandbox, gets
//! preempted, thread B (a different module, different lock) sets `HOME` to
//! its OWN sandbox, and thread A resumes reading/writing under the wrong
//! home directory. This is exactly the "stable, not flake" tachi#1212 gate
//! failure in `registry::tests::register_then_remove_updates_registry` and
//! `scrap_ledger::tests::finds_a_scrap_record_by_branch_regardless_of_path`:
//! with enough concurrent `HOME`-mutating tests across the crate (12+ cargo
//! test threads on this machine), the race reproduces on effectively every
//! run, not intermittently.
//!
//! One process-wide lock, shared by every module that mutates `HOME`,
//! closes the gap: no two `HOME`-mutating tests anywhere in the crate can
//! interleave, regardless of which module they live in.
#![cfg(test)]

use std::sync::{Mutex, OnceLock};

pub(crate) fn home_env_lock() -> &'static Mutex<()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
}
