//! Project-DB relocation audit (DRY-RUN by default).
//! Extracted from memory-server (#833).
//!
//! Background: most `~/.tachi/projects/<name>/memory.db` files are *symlinks*
//! into a repo-local `<repo>/.tachi/memory.db`. A handful are **real files**
//! living centrally — real per-project data that was never linked back into its
//! owning repo (e.g. `quant`, `hyperion-*`, `Quant_Analyzer_2026_audit_*`). And
//! some are UUID-named smoke-test directories that are pure garbage.
//!
//! This module enumerates each centralized project DB, classifies it, and emits
//! a **relocation plan**. It performs NO filesystem mutation by itself — the CLI
//! layer (`bootstrap::manifest_cli` in `memory-server`) decides whether to print the plan
//! (default) or, behind an explicit `--apply` flag, act on it with a backup and
//! an ambiguity refusal.
//!
//! Design notes:
//!   * The classification core ([`classify_project_db`]) is a pure function over
//!     a small [`ProjectDbInput`] struct so it can be unit-tested with synthetic
//!     inputs (symlink vs real file vs uuid name vs owned-vs-home) without ever
//!     touching a real `~/.tachi`.
//!   * The filesystem enumeration ([`gather_project_inputs`]) is a thin shell that
//!     gathers facts (is it a symlink? where does it point? does the target
//!     exist? is there an owning git repo?) and hands each one to the pure core.

mod classify;
mod gather;
mod plan;
mod render;
mod resolve;
mod types;

#[cfg(test)]
mod tests;

#[cfg(test)]
pub use classify::{classify_project_db, is_uuid_smoke_test_name};
pub use gather::gather_project_inputs;
pub use plan::build_plan;
pub use render::render_plan;
#[cfg(test)]
pub use resolve::resolve_owning_repo;
pub use types::{PlannedAction, ProjectDbClass, ProjectDbInput, RelocationItem, RelocationPlan};
