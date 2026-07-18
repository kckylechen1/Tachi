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

/// How the legacy `memory.db` sibling classifies on disk. The three cases are
/// kept explicit (rather than collapsed to a bool) because each drives a
/// different, data-safety-critical branch in the state machine below, and
/// because "definitively absent" MUST NOT be conflated with "present but
/// un-stat-able" (see [`stat_error`] and BUG #1132-2).
enum LegacySibling {
    /// `symlink_metadata` returned `NotFound` — the file genuinely does not
    /// exist.
    Absent,
    /// A symlink at the legacy path — either a compat shim from a prior
    /// migration or some other deliberate link. Never renamed.
    Symlink,
    /// A real (regular/other non-symlink) file holding pre-#1132 data.
    RealFile,
}

/// Compat contract for the #1132 rename, applied right before a DB file at
/// `db_path` is opened/created. This is a crash-safe, idempotent state machine
/// over the pair (canonical file present?, legacy sibling kind). It is the ONLY
/// place that may rename/relink these files, so every combination is handled
/// explicitly — the seam never falls through to letting a later
/// `Connection::open` create an empty DB when the on-disk truth is ambiguous or
/// unknown.
///
/// - `db_path`'s file name isn't [`MEMORY_DB_FILENAME`] -> no-op (caller is
///   opening something else — an in-memory DB, an explicit non-standard path,
///   etc. — not this seam's concern).
///
/// Otherwise, both `db_path` (canonical) and the legacy `memory.db` sibling are
/// `symlink_metadata`-stat'd, distinguishing `NotFound` (definitively absent)
/// from any other error (present but un-stat-able -> FAIL LOUD, never treated as
/// absent — BUG #1132-2). Then, by (canonical, legacy):
///
/// - (absent, absent) / (absent, symlink) -> no-op. Genuinely fresh, or an
///   orphaned compat symlink pointing at a not-yet-created canonical file; the
///   normal open path creates the canonical file from scratch.
/// - (absent, real file) -> migrate: `rename` the legacy file to the canonical
///   name (atomic on POSIX — the data is never in two places at once), then
///   leave a `memory.db -> tachi-memory.db` compat symlink behind idempotently.
/// - (present, symlink) -> no-op. Fully migrated; this is the pure "second
///   open" fast path.
/// - (present, absent) -> CONVERGE: the canonical file exists but its compat
///   symlink is missing — a crash between the atomic rename and the symlink
///   step, or a fresh canonical-only store. Re-establish the symlink
///   idempotently so a half-done migration reaches its defined terminal state.
///   The canonical data is never touched.
/// - (present, real file) -> FAIL LOUD (BUG #1132-1): both a real canonical AND
///   a real legacy file exist side by side — a partially-failed/ambiguous
///   migration. We cannot know which holds the authoritative rows; silently
///   picking either could shadow or discard real data. Refuse without touching
///   either file and demand manual reconciliation.
/// - any stat or the rename itself failing (permissions, cross-device,
///   read-only fs, ...) -> returns a loud [`MemoryError`] rather than silently
///   opening/creating under the wrong name.
pub fn migrate_legacy_filename_if_present(db_path: &Path) -> Result<(), MemoryError> {
    let is_canonical_name = db_path
        .file_name()
        .and_then(|f| f.to_str())
        .map(|f| f == MEMORY_DB_FILENAME)
        .unwrap_or(false);
    if !is_canonical_name {
        return Ok(());
    }

    let legacy_path = db_path.with_file_name(LEGACY_MEMORY_DB_FILENAME);

    // Classify the legacy sibling FIRST, with strict absent-vs-error discipline.
    // A non-`NotFound` error here means the legacy file may exist but be
    // unreadable (permissions, transient I/O); treating that as "absent" (the
    // pre-fix bug) would let us proceed to create an empty canonical DB that
    // shadows the still-present real data. Fail loud instead.
    let legacy_kind = match std::fs::symlink_metadata(&legacy_path) {
        Ok(meta) if meta.file_type().is_symlink() => LegacySibling::Symlink,
        Ok(_) => LegacySibling::RealFile,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => LegacySibling::Absent,
        Err(e) => return Err(stat_error("legacy", &legacy_path, e)),
    };

    // Classify the canonical target with the same discipline. `NotFound` ->
    // genuinely not there; any other error -> present-but-un-stat-able, fail
    // loud rather than risk renaming/creating over data we couldn't read.
    let canonical_present = match std::fs::symlink_metadata(db_path) {
        Ok(_) => true,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => false,
        Err(e) => return Err(stat_error("canonical", db_path, e)),
    };

    match (canonical_present, legacy_kind) {
        // Genuinely fresh, or an orphaned compat symlink pointing at a
        // not-yet-created canonical file. Nothing to migrate.
        (false, LegacySibling::Absent) | (false, LegacySibling::Symlink) => Ok(()),

        // Pre-#1132 install: a real legacy file, no canonical file yet.
        // `rename` is atomic on POSIX, so the bytes never live under two names
        // at once; the compat symlink is then (re-)established idempotently.
        (false, LegacySibling::RealFile) => {
            std::fs::rename(&legacy_path, db_path)
                .map_err(|e| rename_error(&legacy_path, db_path, e))?;
            leave_compat_symlink(&legacy_path)
        }

        // Fully migrated (canonical real file + legacy compat symlink), or a
        // canonical file whose legacy sibling is a deliberate link: pure no-op.
        // This is the common "second open" fast path — zero fs mutation.
        (true, LegacySibling::Symlink) => Ok(()),

        // Half-done migration / fresh canonical-only store: the canonical file
        // exists but the compat symlink is missing. Converge idempotently by
        // (re-)leaving the symlink. The canonical data is never touched.
        (true, LegacySibling::Absent) => leave_compat_symlink(&legacy_path),

        // AMBIGUOUS / partially-failed migration (BUG #1132-1): BOTH a real
        // canonical file AND a real legacy file exist. We cannot know which is
        // authoritative; opening/renaming either could shadow or discard real
        // rows. Refuse loudly, touch nothing, demand manual reconciliation.
        (true, LegacySibling::RealFile) => Err(MemoryError::Io(std::io::Error::new(
            std::io::ErrorKind::AlreadyExists,
            format!(
                "#1132 legacy DB filename migration: refusing to open — both a canonical {} and a \
                 legacy {} regular file exist in the same directory. This is a partially-failed or \
                 ambiguous migration; opening either could shadow real data. No file was touched. \
                 Reconcile manually (verify which holds the authoritative rows, back it up, then \
                 remove or merge the other) and retry.",
                db_path.display(),
                legacy_path.display(),
            ),
        ))),
    }
}

/// Loud error for a stat failure on either the canonical or legacy DB path.
/// The entire point is to NEVER treat "I couldn't read this path's metadata"
/// as "this path is absent": that conflation (BUG #1132-2) is exactly what
/// lets an empty canonical DB get created beside real-but-unreadable data.
fn stat_error(which: &str, path: &Path, e: std::io::Error) -> MemoryError {
    MemoryError::Io(std::io::Error::new(
        e.kind(),
        format!(
            "#1132 legacy DB filename migration: could not stat the {which} memory-database path \
             {}: {e}. Refusing to open/create — the file may exist but be unreadable (permissions, \
             transient I/O), and treating it as absent could create an empty DB that shadows real \
             data. Fix the underlying filesystem/permissions issue and retry.",
            path.display()
        ),
    ))
}

/// Loud error for a failed `rename` of the legacy file onto the canonical name.
fn rename_error(legacy_path: &Path, db_path: &Path, e: std::io::Error) -> MemoryError {
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
    match std::os::unix::fs::symlink(MEMORY_DB_FILENAME, legacy_path) {
        Ok(()) => Ok(()),
        // Idempotent: a concurrent open (or a prior convergence run) may have
        // already planted the compat symlink between our stat and this call.
        // That's success as long as it points where we want; if some OTHER
        // entry grabbed the legacy name, refuse rather than clobber it.
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
            match std::fs::read_link(legacy_path) {
                Ok(target) if target == Path::new(MEMORY_DB_FILENAME) => Ok(()),
                Ok(target) => Err(MemoryError::Io(std::io::Error::new(
                    std::io::ErrorKind::AlreadyExists,
                    format!(
                        "#1132 legacy DB filename migration: {} already exists but points at {} \
                         instead of the canonical {}. Refusing to overwrite an unexpected entry at \
                         the legacy path.",
                        legacy_path.display(),
                        target.display(),
                        MEMORY_DB_FILENAME
                    ),
                ))),
                Err(read_err) => Err(MemoryError::Io(std::io::Error::new(
                    read_err.kind(),
                    format!(
                        "#1132 legacy DB filename migration: an entry already exists at {} but its \
                         link target could not be read: {read_err}. Refusing to overwrite it.",
                        legacy_path.display()
                    ),
                ))),
            }
        }
        Err(e) => Err(MemoryError::Io(std::io::Error::new(
            e.kind(),
            format!(
                "#1132 legacy DB filename migration: the canonical {} is in place but leaving the \
                 compat symlink at {} failed: {e}. The data is safe under the new name; this only \
                 means old-name readers won't find it this release window.",
                MEMORY_DB_FILENAME,
                legacy_path.display()
            ),
        ))),
    }
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

    #[cfg(unix)]
    #[test]
    fn migrate_converges_the_compat_symlink_when_canonical_exists_without_it() {
        // A crash between the atomic rename and the symlink step (or a fresh
        // canonical-only store) leaves the canonical file present but no compat
        // symlink. A subsequent open must CONVERGE — leave the symlink — without
        // clobbering the canonical data, and be a pure no-op thereafter.
        let dir = tempfile::tempdir().expect("tempdir");
        let target = dir.path().join(MEMORY_DB_FILENAME);
        let legacy = dir.path().join(LEGACY_MEMORY_DB_FILENAME);
        std::fs::write(&target, b"already migrated").expect("write target");

        migrate_legacy_filename_if_present(&target).expect("convergence must not error");

        assert_eq!(
            std::fs::read(&target).expect("read target"),
            b"already migrated",
            "an existing canonical-named file must not be clobbered"
        );
        let legacy_meta = std::fs::symlink_metadata(&legacy)
            .expect("convergence must leave a compat symlink where none existed");
        assert!(
            legacy_meta.file_type().is_symlink(),
            "the half-done migration must converge by leaving the compat symlink"
        );
        assert_eq!(
            std::fs::read_link(&legacy).expect("read compat symlink target"),
            std::path::PathBuf::from(MEMORY_DB_FILENAME),
            "the converged compat symlink must point at the canonical filename"
        );

        // Second run: canonical file + compat symlink both in place -> pure
        // no-op. Nothing may change.
        migrate_legacy_filename_if_present(&target).expect("idempotent second open");
        assert_eq!(
            std::fs::read(&target).expect("read target"),
            b"already migrated"
        );
        assert!(std::fs::symlink_metadata(&legacy)
            .expect("legacy metadata")
            .file_type()
            .is_symlink());
    }

    #[cfg(unix)]
    #[test]
    fn migrate_fails_loud_when_both_canonical_and_legacy_real_files_exist() {
        // BUG #1132-1: a partially-failed/ambiguous migration where BOTH a real
        // canonical file AND a real legacy file exist. The seam must NOT silently
        // pick one (that could shadow/discard real rows) — it must fail loud and
        // touch nothing.
        let dir = tempfile::tempdir().expect("tempdir");
        let target = dir.path().join(MEMORY_DB_FILENAME);
        let legacy = dir.path().join(LEGACY_MEMORY_DB_FILENAME);
        std::fs::write(&target, b"canonical real data").expect("write canonical");
        std::fs::write(&legacy, b"legacy real data").expect("write legacy");

        let err = migrate_legacy_filename_if_present(&target).expect_err(
            "both-files-present is ambiguous and must fail loud, not silently pick one",
        );
        let message = err.to_string();
        assert!(
            message.contains("both a canonical"),
            "error must name the both-files ambiguity: {message}"
        );
        assert!(
            message.contains("No file was touched"),
            "error must make the touch-nothing guarantee explicit: {message}"
        );

        // Neither file may have been renamed, clobbered, or turned into a link.
        assert_eq!(
            std::fs::read(&target).expect("read canonical"),
            b"canonical real data",
            "canonical file must be left exactly as it was"
        );
        assert_eq!(
            std::fs::read(&legacy).expect("read legacy"),
            b"legacy real data",
            "legacy file must be left exactly as it was"
        );
        assert!(
            std::fs::symlink_metadata(&legacy)
                .expect("legacy metadata")
                .file_type()
                .is_file(),
            "legacy real file must not have been converted to a symlink"
        );
    }

    #[cfg(unix)]
    #[test]
    fn migrate_fails_loud_when_a_db_path_is_present_but_un_stat_able() {
        // BUG #1132-2: a metadata error (permission denied, transient I/O) on a
        // memory-database path must FAIL LOUD, never be read as "absent" and fall
        // through to creating a fresh empty canonical DB that shadows the real,
        // still-present data. We strip search (x) permission on the store dir so
        // that lstat() of the paths inside returns EACCES rather than NotFound —
        // the "present but un-stat-able" class.
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().expect("tempdir");
        let store_dir = dir.path().join("store");
        std::fs::create_dir(&store_dir).expect("mk store dir");
        // Real pre-#1132 data that the buggy path would have shadowed.
        let legacy = store_dir.join(LEGACY_MEMORY_DB_FILENAME);
        std::fs::write(&legacy, b"real pre-#1132 data").expect("write legacy");
        let target = store_dir.join(MEMORY_DB_FILENAME);

        let original_mode = std::fs::metadata(&store_dir)
            .expect("store dir metadata")
            .permissions()
            .mode();
        std::fs::set_permissions(&store_dir, std::fs::Permissions::from_mode(0o000))
            .expect("make store dir unsearchable");

        let result = migrate_legacy_filename_if_present(&target);

        // Restore permission BEFORE any assertion can panic and skip cleanup.
        std::fs::set_permissions(&store_dir, std::fs::Permissions::from_mode(original_mode))
            .expect("restore store dir permissions");

        let err =
            result.expect_err("an un-stat-able DB path must fail loud, not be treated as absent");
        let message = err.to_string();
        assert!(
            message.contains("could not stat"),
            "error must name the stat failure: {message}"
        );
        assert!(
            message.contains("could create an empty DB that shadows real data"),
            "error must make the shadow-avoidance intent explicit: {message}"
        );
        assert!(
            !target.exists(),
            "a failed stat must NOT have created an empty canonical DB"
        );
        assert!(
            legacy.is_file(),
            "the real legacy data must be left exactly where it was"
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
