//! #736 — Plan C alias *identity drift* reconciliation.
//!
//! `plan_c_dir_name_from_root` (see `alias.rs`) derives the Plan C alias
//! directory name from a repo root; PR #692 changed that derivation (casefold
//! the canonical root before hashing, issue #493) so any repo whose alias dir
//! predates #692 now derives a NEW name that differs from its existing
//! on-disk alias. #692 added a fallback for *legacy un-hashed* alias dirs but
//! nothing migrates an *old-hash* alias dir to the new name — it is simply
//! orphaned (still resolvable by hand, never adopted).
//!
//! This module is the "adopt to the new name" half of the fix: given the
//! repo-local canonical DB and its project root, if an alias directory OTHER
//! than the current derived name exists on disk and its `memory.db` is a
//! *symlink* resolving to that exact canonical file, adopt the current name
//! (create/repair its symlink) and retire the stale alias.
//!
//! Safety invariants (never violated):
//!   * The repo-local canonical DB is never read, moved, or modified.
//!   * Only a `memory.db` that is *provably a symlink* to the canonical file
//!     is ever removed. A regular file at that path is real (possibly
//!     diverged) data — split-brain territory handled by
//!     [`super::plan_c_split_brain`] / `repair::plan_c`, never silently
//!     deleted here.
//!   * Backup files (`*.bak.*`, `*.migration-bak.*`, or any other sibling
//!     file) living next to a retired alias's symlink are never touched; the
//!     alias directory is only removed once it is provably empty.
//!   * If no drifted alias exists at all (e.g. issue #736's requirement 5
//!     scenario — a repo whose current name has no alias dir and no
//!     manifest entry under that name), nothing is created: repo-local +
//!     manifest resolution already serves that case without a physical
//!     alias, so this reconciliation only acts when there is something
//!     concrete to reconcile.

use super::alias::plan_c_dir_name_from_root;
use super::home::tachi_home;
use super::symlink::ensure_plan_c_symlink;
use super::types::PlanCLinkOutcome;
use std::path::Path;

/// Outcome of a single reconciliation attempt for one project root.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum PlanCReconcileAction {
    /// No other-named alias resolving to this canonical file was found on
    /// disk; nothing to reconcile.
    NoDrift,
    /// A drifted alias was found and retired; the current-derived name now
    /// carries the (repaired) symlink.
    Reconciled {
        new_name: String,
        retired: Vec<String>,
    },
    /// A drifted alias was found but reconciliation could not proceed safely
    /// (e.g. adopting the new name hit a split-brain regular file, or the
    /// root has no usable directory name).
    Skipped { reason: String },
}

/// Reconcile Plan C alias identity drift for a single repo-local project DB.
///
/// `local_db` must be the already-canonicalized `<repo>/.tachi/memory.db`
/// path; `project_root` its parent-of-`.tachi` directory.
pub(crate) fn reconcile_plan_c_alias_drift(
    local_db: &Path,
    project_root: &Path,
) -> PlanCReconcileAction {
    let Some(new_name) = plan_c_dir_name_from_root(project_root) else {
        return PlanCReconcileAction::NoDrift;
    };
    let Some(canonical_target) = std::fs::canonicalize(local_db).ok() else {
        return PlanCReconcileAction::NoDrift;
    };

    let projects_root = tachi_home().join("projects");
    let Ok(entries) = std::fs::read_dir(&projects_root) else {
        return PlanCReconcileAction::NoDrift;
    };

    // First pass: find every OTHER alias dir (name != current derived name)
    // whose memory.db is a symlink resolving to the exact same canonical
    // file. Only act if at least one exists — an absent alias is not drift
    // (requirement 5: repo-local + manifest resolution already covers it).
    let mut drifted_dirs: Vec<std::path::PathBuf> = Vec::new();
    for entry in entries.flatten() {
        let Ok(file_type) = entry.file_type() else {
            continue;
        };
        if !file_type.is_dir() {
            continue;
        }
        if entry.file_name().to_str() == Some(new_name.as_str()) {
            continue;
        }
        let candidate = entry.path().join("memory.db");
        let Ok(meta) = std::fs::symlink_metadata(&candidate) else {
            continue;
        };
        if !meta.file_type().is_symlink() {
            // Regular file: possibly diverged real data. Never auto-retire;
            // that is split-brain territory (repair::plan_c / R11).
            continue;
        }
        if std::fs::canonicalize(&candidate).ok().as_ref() != Some(&canonical_target) {
            continue;
        }
        drifted_dirs.push(entry.path());
    }

    if drifted_dirs.is_empty() {
        return PlanCReconcileAction::NoDrift;
    }

    match ensure_plan_c_symlink(local_db, project_root) {
        PlanCLinkOutcome::AlreadyLinked | PlanCLinkOutcome::Created(_) => {}
        PlanCLinkOutcome::SplitBrain(issue) => {
            return PlanCReconcileAction::Skipped {
                reason: format!(
                    "current-name alias '{new_name}' is a diverged regular file: {}",
                    issue.warning_message()
                ),
            };
        }
        PlanCLinkOutcome::Skipped(reason) => {
            return PlanCReconcileAction::Skipped {
                reason: reason.to_string(),
            };
        }
        PlanCLinkOutcome::Failed { path, error } => {
            return PlanCReconcileAction::Skipped {
                reason: format!(
                    "failed to create alias symlink at {}: {error}",
                    path.display()
                ),
            };
        }
    }

    let mut retired = Vec::new();
    for dir in drifted_dirs {
        let candidate = dir.join("memory.db");
        if std::fs::remove_file(&candidate).is_err() {
            continue;
        }
        let Some(name) = dir.file_name().and_then(|n| n.to_str()).map(str::to_string) else {
            continue;
        };
        retired.push(name);
        // Retire the directory itself only if nothing else remains in it —
        // backup files (migration-bak, .bak.*, etc.) are never deleted, so a
        // directory that still holds them is left in place (memory.db gone
        // means it no longer resolves as a live alias).
        let is_empty = std::fs::read_dir(&dir)
            .map(|mut it| it.next().is_none())
            .unwrap_or(false);
        if is_empty {
            let _ = std::fs::remove_dir(&dir);
        }
    }

    if retired.is_empty() {
        PlanCReconcileAction::NoDrift
    } else {
        PlanCReconcileAction::Reconciled { new_name, retired }
    }
}
