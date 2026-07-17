//! Canonical on-disk filename for a memory database, plus the one-time
//! rename-on-open migration away from the pre-#1132 `memory.db` name.
//!
//! Naming ruling (owner-ratified 2026-07-17, tachi#1132): "system in the
//! filename, scope stays in the directory" — every standalone-Tachi store
//! (`~/.tachi/global/`, `~/.tachi/projects/<name>/`, repo-local `.tachi/`)
//! renames its file `memory.db` -> `tachi-memory.db`. This module is the
//! single seam: every write-path DB open funnels through
//! [`MemoryStore::open_with_label_inner`] in `store/open.rs`, which calls
//! [`migrate_legacy_filename_if_present`] before touching the file, so the
//! migration logic lives in exactly one place instead of being re-derived at
//! each of the many call sites that construct a `db_path`.

use std::path::Path;

use crate::error::MemoryError;

/// Canonical memory-database filename. Every fresh DB created from here on
/// uses this name.
pub const MEMORY_DB_FILENAME: &str = "tachi-memory.db";

/// Pre-#1132 filename. Never used to CREATE a new DB — only consulted here to
/// detect and migrate an existing file forward, and left behind as a
/// one-release-window compat symlink pointing at [`MEMORY_DB_FILENAME`].
pub const LEGACY_MEMORY_DB_FILENAME: &str = "memory.db";

/// Compat contract for the #1132 rename, applied right before a DB file at
/// `db_path` is opened/created:
///
/// - `db_path`'s file name isn't [`MEMORY_DB_FILENAME`] -> no-op (caller is
///   opening something else — an in-memory DB, an explicit non-standard
///   path, etc. — not this seam's concern).
/// - `db_path` already exists -> no-op (already migrated, or a fresh DB
///   already created under the new name).
/// - no legacy `memory.db` sits next to it either -> no-op (genuinely fresh;
///   the normal open path creates `db_path` from scratch).
/// - a legacy `memory.db` **regular file** exists -> rename it to the new
///   name, then leave a `memory.db -> tachi-memory.db` symlink behind as a
///   one-release compat shim for anything still hard-coded to the old name.
/// - the legacy path is already a symlink (e.g. a previous run already
///   migrated this directory) -> treated as already-migrated, no-op.
/// - the rename itself fails (permissions, cross-device, read-only fs, ...)
///   -> returns a loud [`MemoryError`] rather than silently opening/creating
///   under the legacy name.
pub fn migrate_legacy_filename_if_present(db_path: &Path) -> Result<(), MemoryError> {
    let is_canonical_name = db_path
        .file_name()
        .and_then(|f| f.to_str())
        .map(|f| f == MEMORY_DB_FILENAME)
        .unwrap_or(false);
    if !is_canonical_name || db_path.exists() {
        return Ok(());
    }

    let legacy_path = db_path.with_file_name(LEGACY_MEMORY_DB_FILENAME);
    let legacy_meta = match std::fs::symlink_metadata(&legacy_path) {
        Ok(meta) => meta,
        Err(_) => return Ok(()), // no legacy file present — genuinely fresh
    };
    if legacy_meta.file_type().is_symlink() {
        // Already a compat shim from a prior migration (or some other
        // deliberate link at this path) — don't touch it again.
        return Ok(());
    }

    std::fs::rename(&legacy_path, db_path).map_err(|e| {
        MemoryError::Io(std::io::Error::new(
            e.kind(),
            format!(
                "#1132 legacy DB filename migration failed: could not rename {} -> {}: {e}. \
                 Refusing to silently open/create under the legacy name — fix the underlying \
                 filesystem/permissions issue and retry.",
                legacy_path.display(),
                db_path.display()
            ),
        ))
    })?;

    leave_compat_symlink(&legacy_path)
}

#[cfg(unix)]
fn leave_compat_symlink(legacy_path: &Path) -> Result<(), MemoryError> {
    std::os::unix::fs::symlink(MEMORY_DB_FILENAME, legacy_path).map_err(|e| {
        MemoryError::Io(std::io::Error::new(
            e.kind(),
            format!(
                "#1132 legacy DB filename migration: renamed {} -> {} but failed to leave the \
                 compat symlink at {}: {e}. The data is safe under the new name; this only \
                 means old-name readers won't find it this release window.",
                legacy_path.display(),
                MEMORY_DB_FILENAME,
                legacy_path.display()
            ),
        ))
    })
}

#[cfg(not(unix))]
fn leave_compat_symlink(_legacy_path: &Path) -> Result<(), MemoryError> {
    // No symlink primitive on this platform — matches the existing Plan-C
    // alias precedent (`path_utils::ensure_plan_c_symlink`'s `#[cfg(not(unix))]`
    // branch), which is also a no-op there. The rename already completed, so
    // the data is not stranded; only the compat shim for old-name readers is
    // unavailable on non-Unix hosts.
    Ok(())
}
