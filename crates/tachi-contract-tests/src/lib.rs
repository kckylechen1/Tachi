//! Workspace contract/census tests extracted from tachi-server's test binary
//! (#1610 Track T). The library is intentionally empty: every test here asserts
//! on repo files, docs fixtures, or already-`pub` items in sibling crates, so
//! nothing needs to be exported from tachi-server for them to run.
//!
//! # What this extraction does and does NOT buy (#1610 Track T, honest ledger)
//!
//! It does **not** shrink the relink radius of a `tachi-server` edit. Crate
//! splitting reduces *recompilation*, not *linking*: a one-line change in
//! tachi-server still recompiles that crate and relinks its lib test binary.
//! The edit that removes a whole 245MB-class link is `test = false` /
//! `bench = false` on tachi-server's zero-test `[[bin]]` targets, which is a
//! separate, already-landed change — not this one.
//!
//! What it does buy, measured at extraction time (87 tests / 5,969 lines):
//!
//! 1. These 87 tests stop being rebuilt and relinked whenever tachi-server
//!    changes. This crate's dev-dependencies are `tachi-params`, `tachi-hub`
//!    and `tachi-merge-ops` — there is deliberately **no** edge to
//!    `tachi-server`, so nothing here is downstream of it.
//! 2. A fast standalone loop for census/contract authors:
//!    `cargo test -p tachi-contract-tests` compiles three small leaf crates
//!    instead of the 350K-LOC server.
//!
//! # Layout invariant (do not flatten)
//!
//! Every file here sits at the SAME module depth it had under
//! `crates/tachi-server/src/tests/`, because the tests reach the repo by
//! relative path: `docs_tests/*.rs` use `include_str!("../../../../../docs/…")`
//! (5 pops → repo root), `portable_mirror_tests.rs` uses
//! `include_str!("../../../<crate>/src/…")` (3 pops → `crates/`), and several
//! `repo_root()` helpers pop `CARGO_MANIFEST_DIR` twice. Moving a file up or
//! down a directory silently repoints those at the wrong tree.

#[cfg(test)]
mod tests;
