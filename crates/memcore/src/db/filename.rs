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

/// True if `name` is either the canonical or the legacy memory-database
/// filename. For detection/classification code (doctor scans, manifest
/// classification, "does this directory look like a tachi home" checks) that
/// must recognize a DB file regardless of which side of the #1132 rename it's
/// on. Path-construction code that decides what to CREATE should use
/// [`MEMORY_DB_FILENAME`] directly, never this predicate — this exists only
/// for "is this file relevant" checks, not "what should I name a new file".
pub fn is_memory_db_filename(name: &str) -> bool {
    name == MEMORY_DB_FILENAME || name == LEGACY_MEMORY_DB_FILENAME
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn is_memory_db_filename_accepts_both_generations() {
        assert!(is_memory_db_filename(MEMORY_DB_FILENAME));
        assert!(is_memory_db_filename(LEGACY_MEMORY_DB_FILENAME));
        assert!(!is_memory_db_filename("other.db"));
        assert!(!is_memory_db_filename("tachi-memory.db.bak.20260101"));
    }

    #[test]
    fn migrate_is_a_no_op_for_a_non_canonical_target_name() {
        let dir = tempfile::tempdir().expect("tempdir");
        // A caller opening some other, non-standard filename — the seam must
        // not touch it, even if a `memory.db` sibling happens to exist.
        let other_target = dir.path().join("scratch.db");
        let legacy = dir.path().join(LEGACY_MEMORY_DB_FILENAME);
        std::fs::write(&legacy, b"legacy bytes").expect("write legacy sibling");

        migrate_legacy_filename_if_present(&other_target).expect("no-op must not error");

        assert!(
            !other_target.exists(),
            "non-canonical target must not be created"
        );
        assert!(
            legacy.exists(),
            "unrelated legacy sibling must be left alone"
        );
        assert!(
            std::fs::symlink_metadata(&legacy)
                .expect("legacy metadata")
                .file_type()
                .is_file(),
            "unrelated legacy sibling must not be touched/converted to a symlink"
        );
    }

    #[test]
    fn migrate_is_a_no_op_when_the_canonical_target_already_exists() {
        let dir = tempfile::tempdir().expect("tempdir");
        let target = dir.path().join(MEMORY_DB_FILENAME);
        std::fs::write(&target, b"already migrated").expect("write target");

        migrate_legacy_filename_if_present(&target).expect("no-op must not error");

        assert_eq!(
            std::fs::read(&target).expect("read target"),
            b"already migrated",
            "an existing canonical-named file must not be clobbered"
        );
    }

    #[test]
    fn migrate_is_a_no_op_when_genuinely_fresh() {
        let dir = tempfile::tempdir().expect("tempdir");
        let target = dir.path().join(MEMORY_DB_FILENAME);

        migrate_legacy_filename_if_present(&target).expect("no-op must not error");

        assert!(
            !target.exists(),
            "migrate() only renames — it must not create the target itself"
        );
    }

    #[cfg(unix)]
    #[test]
    fn migrate_is_idempotent_once_the_legacy_path_is_already_a_symlink() {
        let dir = tempfile::tempdir().expect("tempdir");
        let target = dir.path().join(MEMORY_DB_FILENAME);
        let legacy = dir.path().join(LEGACY_MEMORY_DB_FILENAME);
        // Simulate a directory already migrated by a prior run, where the
        // real file has since been deleted (or never landed) but the compat
        // symlink is still sitting there pointing at a name that doesn't
        // exist. A second migration attempt must not error or resurrect it.
        std::os::unix::fs::symlink(MEMORY_DB_FILENAME, &legacy).expect("plant compat symlink");

        migrate_legacy_filename_if_present(&target).expect("no-op must not error");

        assert!(
            !target.exists(),
            "an orphaned compat symlink must not be treated as data to rename"
        );
        assert!(
            std::fs::symlink_metadata(&legacy)
                .expect("legacy metadata")
                .file_type()
                .is_symlink(),
            "the pre-existing symlink must be left exactly as it was"
        );
    }

    #[cfg(unix)]
    #[test]
    fn migrate_renames_a_legacy_regular_file_and_leaves_a_compat_symlink() {
        let dir = tempfile::tempdir().expect("tempdir");
        let target = dir.path().join(MEMORY_DB_FILENAME);
        let legacy = dir.path().join(LEGACY_MEMORY_DB_FILENAME);
        std::fs::write(&legacy, b"pre-#1132 data").expect("write legacy file");

        migrate_legacy_filename_if_present(&target).expect("migration must succeed");

        assert!(
            target.is_file(),
            "the legacy file must now live at the canonical name"
        );
        assert_eq!(
            std::fs::read(&target).expect("read migrated target"),
            b"pre-#1132 data",
            "the rename must preserve the file's bytes exactly"
        );
        let legacy_meta = std::fs::symlink_metadata(&legacy).expect("legacy metadata");
        assert!(
            legacy_meta.file_type().is_symlink(),
            "the old name must become a compat symlink, not vanish or stay a regular file"
        );
        assert_eq!(
            std::fs::read_link(&legacy).expect("read compat symlink target"),
            std::path::PathBuf::from(MEMORY_DB_FILENAME),
            "the compat symlink must point at the canonical filename"
        );
        // Old-name readers still resolving through the symlink must see the
        // same bytes as the canonical path — that's the entire point of the
        // one-release-window compat shim.
        assert_eq!(
            std::fs::read(&legacy).expect("read through compat symlink"),
            b"pre-#1132 data",
        );
    }

    #[cfg(unix)]
    #[test]
    fn migrate_rename_failure_is_loud_not_silent() {
        let dir = tempfile::tempdir().expect("tempdir");
        let target = dir.path().join(MEMORY_DB_FILENAME);
        let legacy = dir.path().join(LEGACY_MEMORY_DB_FILENAME);
        std::fs::write(&legacy, b"pre-#1132 data").expect("write legacy file");

        // Make the containing directory non-writable so the rename() syscall
        // itself fails with a permission error, simulating a read-only-fs /
        // permissions class of failure without needing an actual read-only
        // mount.
        use std::os::unix::fs::PermissionsExt;
        let original_mode = std::fs::metadata(dir.path())
            .expect("dir metadata")
            .permissions()
            .mode();
        std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o555))
            .expect("make dir read-only");

        let result = migrate_legacy_filename_if_present(&target);

        // Restore write permission BEFORE any assertion can panic and skip
        // cleanup, so the tempdir's Drop can still remove its contents.
        std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(original_mode))
            .expect("restore dir permissions");

        let err = result.expect_err("a failed rename must surface as a loud error, not a no-op");
        let message = err.to_string();
        assert!(
            message.contains("legacy DB filename migration failed"),
            "unexpected error message: {message}"
        );
        assert!(
            message.contains("Refusing to silently open/create under the legacy name"),
            "error must make the fail-loud intent explicit: {message}"
        );
        assert!(
            !target.exists(),
            "a failed migration must not leave a partially-created target"
        );
        assert!(
            legacy.is_file(),
            "a failed migration must leave the legacy data exactly where it was, unrenamed"
        );
    }

    #[cfg(unix)]
    #[test]
    fn memory_store_open_migrates_a_pre_existing_legacy_db_transparently() {
        let dir = tempfile::tempdir().expect("tempdir");
        let legacy_path = dir.path().join(LEGACY_MEMORY_DB_FILENAME);
        let canonical_path = dir.path().join(MEMORY_DB_FILENAME);

        // Simulate a pre-#1132 install: a real, schema-initialized DB sitting
        // under the OLD literal filename, created by opening that path
        // directly (not through the seam — the seam never fires for a
        // non-canonical target, by design).
        {
            let store = crate::MemoryStore::open(legacy_path.to_str().expect("utf8 path"))
                .expect("create legacy-named db");
            let now = chrono::Utc::now().to_rfc3339();
            store
                .connection()
                .execute(
                    "INSERT INTO memories
                     (id, path, summary, text, importance, timestamp, category, topic, keywords, entities, source, scope, archived, created_at, updated_at, access_count, revision, metadata)
                     VALUES (?1, '/facts/rename-on-open', 'marker', 'rename-on-open marker row', 0.5, ?2, 'fact', 'test', '[]', '[]', 'manual', 'project', 0, ?2, ?2, 0, 1, '{}')",
                    rusqlite::params!["rename-on-open-marker", now],
                )
                .expect("insert marker row into legacy-named db");
        }
        assert!(
            legacy_path.is_file(),
            "test precondition: legacy file exists"
        );
        assert!(
            !canonical_path.exists(),
            "test precondition: canonical name not yet created"
        );

        // Opening the CANONICAL path in the same directory must transparently
        // pick up the pre-existing legacy data via the rename-on-open seam.
        let migrated_store = crate::MemoryStore::open(canonical_path.to_str().expect("utf8 path"))
            .expect("open canonical path");

        assert!(
            canonical_path.is_file(),
            "the legacy file must now be the file at the canonical path"
        );
        let legacy_meta = std::fs::symlink_metadata(&legacy_path).expect("legacy metadata");
        assert!(
            legacy_meta.file_type().is_symlink(),
            "the old name must be left behind as a compat symlink"
        );

        let marker: String = migrated_store
            .connection()
            .query_row(
                "SELECT summary FROM memories WHERE id = ?1",
                rusqlite::params!["rename-on-open-marker"],
                |row| row.get(0),
            )
            .expect("marker row must survive the rename");
        assert_eq!(marker, "marker");

        // A second open of the canonical path must be a plain, uneventful
        // open — the seam's own `db_path.exists()` early-return — not a
        // second migration attempt.
        drop(migrated_store);
        let reopened = crate::MemoryStore::open(canonical_path.to_str().expect("utf8 path"))
            .expect("reopen canonical path");
        let marker_again: String = reopened
            .connection()
            .query_row(
                "SELECT summary FROM memories WHERE id = ?1",
                rusqlite::params!["rename-on-open-marker"],
                |row| row.get(0),
            )
            .expect("marker row must still be there on reopen");
        assert_eq!(marker_again, "marker");
    }
}
