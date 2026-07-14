//! Doctor v2 — extension-aware DB classification + safe auto-fix.
//!
//! Classifies every memory.db (or .db.bak / .broken / .corrupted / .old.sqlite)
//! it finds under known Tachi/OpenClaw/Antigravity roots into one of:
//!
//!   * Healthy             — opens read-only with sqlite-vec, has memories table
//!   * VecExtensionMissing — opens but vec virtual tables unreadable (false-broken)
//!   * WalOrphan           — DB has stale .db-wal sidecar
//!   * Corrupt             — pragma quick_check fails for non-extension reasons
//!   * LegacySchema        — has old OpenClaw `chunks` table, no `memories`
//!   * Placeholder         — 0-byte file or obviously empty
//!   * Backup              — filename matches .bak.* / .broken / .corrupted /
//!                            .old.sqlite / pre-split / .checkpointed.db pattern
//!
//! Auto-fix on first run handles two safe categories:
//!   * Placeholder → quarantine to ~/.tachi/quarantine/placeholders/<ts>/
//!   * WalOrphan   → copy aside, wal_checkpoint(TRUNCATE) on the COPY,
//!                   write to <orig>.checkpointed.db. Original untouched.

mod autofix;
mod classify;
mod cross_domain;
mod hub_lint;
mod render;
mod scan;
mod secrets;
mod types;

pub use autofix::auto_fix_safe;
pub use classify::classify_one;
pub use hub_lint::hub_capability_discovery_status_warnings;
pub use render::render_report;
pub use scan::{default_scan_roots, scan, ScanOptions};
// Unit tests assert on backup filename classification via `super::*`.
#[cfg(test)]
pub use scan::is_backup_filename;
pub use secrets::project_secret_file_warnings;
pub use types::{
    AutoFixAction, DbClassification, DoctorFinding, DoctorReport, DoctorWarning, JobBreakdown,
    SummaryByClass,
};

#[cfg(test)]
use autofix::quarantine_dest_filename;
#[cfg(test)]
use classify::scope_hint_for;

#[cfg(test)]
mod tests;
