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
/// - (absent, absent) -> no-op. Genuinely fresh; the normal open path creates
///   the canonical file from scratch.
/// - (absent, symlink) / (present, symlink) -> VALIDATE the link then no-op. A
///   pre-existing link at the legacy path (an orphaned shim, or the healthy
///   shim of a migrated dir) is accepted ONLY if it is exactly the relative
///   `memory.db -> tachi-memory.db` compat shim this seam writes; a link to any
///   other target (a stale/old DB location) is corrupted state that would
///   resolve old-name readers to the wrong data -> FAIL LOUD (RESIDUAL-2).
/// - (absent, real file) -> migrate: ATOMIC NO-CLOBBER rename of the legacy
///   file to the canonical name (`renamex_np`/`renameat2` — atomic AND refusing
///   to overwrite a canonical file a concurrent process created between our
///   stat and the rename; #1226 cross-process TOCTOU close), then leave a
///   `memory.db -> tachi-memory.db` compat symlink behind idempotently. If the
///   kernel reports the canonical file already exists (EEXIST) or the legacy
///   source vanished under a concurrent migrator (ENOENT), we converge on the
///   canonical instead of clobbering. If no atomic no-clobber primitive exists
///   (ENOSYS/ENOTSUP / unsupported platform), we FAIL CLOSED — never degrade to
///   a plain rename, which would reopen the clobber race.
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

    // Classify the legacy sibling FIRST, with strict absent-vs-error discipline
    // (see `classify_legacy_sibling`).
    let legacy_kind = classify_legacy_sibling(&legacy_path)?;

    // Classify the canonical target with the same discipline. `NotFound` ->
    // genuinely not there; any other error -> present-but-un-stat-able, fail
    // loud rather than risk renaming/creating over data we couldn't read.
    let canonical_present = match std::fs::symlink_metadata(db_path) {
        Ok(_) => true,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => false,
        Err(e) => return Err(stat_error("canonical", db_path, e)),
    };

    match (canonical_present, legacy_kind) {
        // Genuinely fresh: no canonical file, no legacy sibling. Nothing to do.
        (false, LegacySibling::Absent) => Ok(()),

        // A pre-existing link sits at the legacy path with no canonical file yet
        // — either an orphaned compat shim or a stale/corrupted link. Validate
        // it (RESIDUAL-2); the (true, Symlink) case is handled by
        // `converge_on_existing_canonical` below.
        (false, LegacySibling::Symlink) => validate_compat_symlink(&legacy_path),

        // Pre-#1132 install: a real legacy file, no canonical file yet. Migrate
        // it forward with an ATOMIC NO-CLOBBER rename so a second process that
        // created the canonical file between our stat and this call cannot be
        // clobbered (round-4 cross-process TOCTOU close — #1226).
        (false, LegacySibling::RealFile) => migrate_real_legacy(&legacy_path, db_path),

        // The canonical file already exists. Converge on it without ever
        // touching its bytes: validate the shim, (re-)leave a missing shim, or
        // fail loud if a SECOND real legacy file sits beside it (BUG #1132-1).
        (true, kind) => converge_on_existing_canonical(kind, &legacy_path, db_path),
    }
}

/// Classify the legacy `memory.db` sibling with strict absent-vs-error
/// discipline. A non-`NotFound` error here means the legacy file may exist but
/// be unreadable (permissions, transient I/O); treating that as "absent" (the
/// pre-fix bug) would let us proceed to create an empty canonical DB that
/// shadows the still-present real data. Fail loud instead.
fn classify_legacy_sibling(legacy_path: &Path) -> Result<LegacySibling, MemoryError> {
    match std::fs::symlink_metadata(legacy_path) {
        Ok(meta) if meta.file_type().is_symlink() => Ok(LegacySibling::Symlink),
        Ok(_) => Ok(LegacySibling::RealFile),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(LegacySibling::Absent),
        Err(e) => Err(stat_error("legacy", legacy_path, e)),
    }
}

/// Migrate a real pre-#1132 legacy file forward onto the canonical name using
/// an ATOMIC NO-CLOBBER rename (`renamex_np`/`renameat2`; see
/// [`atomic_rename_no_clobber`]). This closes the cross-process migration
/// TOCTOU (#1226): the process-local startup mutex the caller holds serializes
/// only same-process opens, so a SECOND daemon opening the same store dir could
/// create the canonical file (via its own migration or a fresh
/// `Connection::open`) between our stat and a plain `rename`, and the plain
/// rename would clobber it -> data loss. The atomic primitive fails with
/// EEXIST instead of clobbering; on EEXIST we converge on the winner's file.
fn migrate_real_legacy(legacy_path: &Path, db_path: &Path) -> Result<(), MemoryError> {
    match atomic_rename_no_clobber(legacy_path, db_path) {
        // We won the race: the legacy file is now the canonical file, and the
        // canonical name did not previously exist. Leave the compat symlink.
        Ok(AtomicRename::Renamed) => leave_compat_symlink(legacy_path),

        // EEXIST: another process created the canonical file between our stat
        // and this rename. We did NOT clobber it. Re-classify the legacy
        // sibling against the now-present canonical and converge exactly as the
        // (true, *) arms would — rather than blindly proceeding, which could
        // leave a (canonical present, real legacy) ambiguity to fail loud on
        // the next open, or worse, silently diverge.
        Ok(AtomicRename::DestinationExists) => {
            let legacy_kind = classify_legacy_sibling(legacy_path)?;
            converge_on_existing_canonical(legacy_kind, legacy_path, db_path)
        }

        // ENOENT: a concurrent migrator moved the legacy file out from under us
        // between our stat and the rename. If that peer already renamed it onto
        // the canonical name, the canonical now exists — converge on it rather
        // than fail spuriously. Only if the canonical is ALSO absent (the data
        // genuinely vanished) do we fail loud.
        Ok(AtomicRename::SourceVanished) => converge_after_source_vanished(legacy_path, db_path),

        // No atomic no-clobber primitive on this kernel/filesystem/platform
        // (ENOSYS/ENOTSUP). FAIL CLOSED — a plain rename would reopen the
        // cross-process clobber race (#1226). Never degrade.
        Ok(AtomicRename::Unsupported) => Err(atomic_rename_unsupported_error(legacy_path, db_path)),

        // Any other errno (permissions, cross-device, read-only fs, ...) is a
        // genuine failure: surface it loudly rather than falling through to an
        // open/create under the wrong name.
        Err(e) => Err(rename_error(legacy_path, db_path, e)),
    }
}

/// Handle the ENOENT / source-vanished case: a concurrent migrator moved the
/// legacy file before our rename. Re-check the canonical file — if the peer
/// already migrated it (canonical now present) CONVERGE exactly as the
/// (canonical present, *) arms do; if the canonical is also absent the data
/// genuinely vanished, so fail loud rather than open an empty DB.
fn converge_after_source_vanished(legacy_path: &Path, db_path: &Path) -> Result<(), MemoryError> {
    let canonical_present = match std::fs::symlink_metadata(db_path) {
        Ok(_) => true,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => false,
        Err(e) => return Err(stat_error("canonical", db_path, e)),
    };
    if canonical_present {
        let legacy_kind = classify_legacy_sibling(legacy_path)?;
        converge_on_existing_canonical(legacy_kind, legacy_path, db_path)
    } else {
        Err(legacy_vanished_error(legacy_path, db_path))
    }
}

/// Converge on an already-present canonical file WITHOUT touching its bytes,
/// dispatching on the legacy sibling's kind. Shared by the (canonical present,
/// *) arms of the state machine and by the EEXIST branch of
/// [`migrate_real_legacy`] so both reach the exact same terminal states.
fn converge_on_existing_canonical(
    legacy_kind: LegacySibling,
    legacy_path: &Path,
    db_path: &Path,
) -> Result<(), MemoryError> {
    match legacy_kind {
        // A pre-existing link at the legacy path (an orphaned shim, or the
        // healthy shim of a migrated dir). Accept ONLY the exact compat shim
        // this seam writes; a link to any other target is corrupted state that
        // would resolve old-name readers to the wrong data -> FAIL LOUD
        // (RESIDUAL-2).
        LegacySibling::Symlink => validate_compat_symlink(legacy_path),

        // Half-done migration / fresh canonical-only store: the canonical file
        // exists but the compat symlink is missing. Converge idempotently by
        // (re-)leaving the symlink. The canonical data is never touched.
        LegacySibling::Absent => leave_compat_symlink(legacy_path),

        // AMBIGUOUS / partially-failed migration (BUG #1132-1): BOTH a real
        // canonical file AND a real legacy file exist. We cannot know which is
        // authoritative; opening/renaming either could shadow or discard real
        // rows. Refuse loudly, touch nothing, demand manual reconciliation.
        LegacySibling::RealFile => Err(both_real_files_error(db_path, legacy_path)),
    }
}

/// Loud error for the ambiguous both-real-files state (BUG #1132-1). Extracted
/// so the direct (canonical present, real legacy) arm and the EEXIST-converge
/// branch produce the identical message and posture.
fn both_real_files_error(db_path: &Path, legacy_path: &Path) -> MemoryError {
    MemoryError::Io(std::io::Error::new(
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
    ))
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

/// Outcome of an atomic no-clobber rename attempt ([`atomic_rename_no_clobber`]).
enum AtomicRename {
    /// The legacy file was atomically renamed onto the canonical name; the
    /// canonical name did not previously exist.
    Renamed,
    /// The kernel reported the destination already exists (EEXIST): another
    /// process created the canonical file first. Nothing was clobbered — the
    /// caller must CONVERGE on the existing canonical file.
    DestinationExists,
    /// The kernel reported the source is gone (ENOENT): another process moved
    /// the legacy file out from under us (a concurrent migrator) between our
    /// stat and the rename. The caller must re-check the canonical file and
    /// CONVERGE if the peer already migrated it, rather than fail spuriously.
    SourceVanished,
    /// The running kernel/filesystem does not implement the no-clobber rename
    /// primitive (ENOSYS/ENOTSUP), or the platform has no such primitive at
    /// all. There is NO safe degraded path (a plain rename would reopen the
    /// cross-process clobber race this whole seam exists to close), so the
    /// caller MUST FAIL CLOSED — never fall back to a clobbering rename.
    Unsupported,
}

/// Atomically rename `from` -> `to`, FAILING with `DestinationExists` (never
/// clobbering) if `to` already exists, using the platform's no-clobber rename
/// primitive:
///   - macOS/iOS: `renamex_np(from, to, RENAME_EXCL)`
///   - Linux:     `renameat2(AT_FDCWD, from, AT_FDCWD, to, RENAME_NOREPLACE)`
///
/// This is the cross-process guard the process-local startup mutex cannot
/// provide (#1226): the rename either moves the legacy file onto a
/// not-yet-existing canonical name, or the kernel refuses it because a
/// concurrent process already created the canonical file. There is no window in
/// which an existing canonical file is overwritten.
///
/// Outcomes: `DestinationExists` on EEXIST (a concurrent process won — converge
/// on it) and `SourceVanished` on ENOENT (a concurrent migrator moved the legacy
/// file — re-check the canonical and converge). `Unsupported` is reported when
/// the primitive is unavailable (old kernel/filesystem -> ENOSYS/ENOTSUP, or a
/// platform without it) — the caller MUST fail closed there (see
/// [`atomic_rename_unsupported_error`]); it deliberately does NOT fall back to a
/// plain `rename`, which is guarded only by the process-local mutex and could
/// clobber a concurrently-created canonical DB -> money-DB data loss. Migrate
/// offline or on a supported filesystem instead. Any other errno -> `Err`.
#[cfg(any(target_os = "macos", target_os = "ios", target_os = "linux"))]
fn atomic_rename_no_clobber(from: &Path, to: &Path) -> Result<AtomicRename, std::io::Error> {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;

    // Kernel paths must be NUL-terminated C strings. A path containing an
    // interior NUL is not a real filesystem path — surface it as an error
    // rather than truncating silently.
    let from_c = CString::new(from.as_os_str().as_bytes()).map_err(|_| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "legacy DB path contains an interior NUL byte",
        )
    })?;
    let to_c = CString::new(to.as_os_str().as_bytes()).map_err(|_| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "canonical DB path contains an interior NUL byte",
        )
    })?;

    // SAFETY: `from_c` and `to_c` are live, NUL-terminated C strings that
    // outlive this call; the syscalls only READ them (const pointers) and do
    // not retain them past return. The flag/fd arguments are plain integer
    // constants from `libc`. The call returns 0 on success or -1 with `errno`
    // set, which we read immediately via `last_os_error()` before any other
    // libc call can clobber `errno`.
    let rc = unsafe {
        #[cfg(any(target_os = "macos", target_os = "ios"))]
        {
            libc::renamex_np(from_c.as_ptr(), to_c.as_ptr(), libc::RENAME_EXCL)
        }
        #[cfg(target_os = "linux")]
        {
            libc::renameat2(
                libc::AT_FDCWD,
                from_c.as_ptr(),
                libc::AT_FDCWD,
                to_c.as_ptr(),
                libc::RENAME_NOREPLACE,
            )
        }
    };

    if rc == 0 {
        return Ok(AtomicRename::Renamed);
    }

    let err = std::io::Error::last_os_error();
    match err.raw_os_error() {
        // Destination already exists: a concurrent process won the race. This
        // is the no-clobber refusal we asked for, not a failure.
        Some(libc::EEXIST) => Ok(AtomicRename::DestinationExists),
        // Source is gone: a concurrent migrator moved the legacy file already.
        // The caller re-checks the canonical file and converges if so.
        Some(libc::ENOENT) => Ok(AtomicRename::SourceVanished),
        // The syscall/flag isn't implemented on this kernel/filesystem. There
        // is no safe degraded path — the caller MUST fail closed.
        Some(libc::ENOSYS) | Some(libc::ENOTSUP) => Ok(AtomicRename::Unsupported),
        _ => Err(err),
    }
}

/// Platforms without a no-clobber rename primitive: report `Unsupported` so the
/// caller FAILS CLOSED. We deliberately do NOT degrade to a plain rename — that
/// would reopen the cross-process clobber race (#1226) this seam closes.
#[cfg(not(any(target_os = "macos", target_os = "ios", target_os = "linux")))]
fn atomic_rename_no_clobber(_from: &Path, _to: &Path) -> Result<AtomicRename, std::io::Error> {
    Ok(AtomicRename::Unsupported)
}

/// Loud hard error for the case where no atomic no-clobber rename primitive is
/// available (ENOSYS/ENOTSUP, or a platform without one). We FAIL CLOSED here
/// rather than fall back to a plain `rename`: a plain rename is guarded only by
/// the process-local startup mutex, which cannot exclude a SECOND daemon, so it
/// would atomically overwrite a canonical file a concurrent process created —
/// the exact money-DB data loss this seam exists to prevent (#1226, round-5).
///
/// FUTURE: to genuinely support such a platform/filesystem, replace the plain
/// rename with a cross-process protocol — e.g. open+`F_SETLK` (advisory lock) an
/// intent file in the store dir, then link-then-rename under the lock — rather
/// than trading the no-clobber guarantee for compatibility. Not implemented
/// now: prod runs modern macOS (`renamex_np` always present) and Linux
/// (`renameat2`), which never reach this path.
fn atomic_rename_unsupported_error(legacy_path: &Path, db_path: &Path) -> MemoryError {
    MemoryError::Io(std::io::Error::new(
        std::io::ErrorKind::Unsupported,
        format!(
            "#1132/#1226 legacy DB filename migration: cannot migrate {} -> {} because this \
             platform/filesystem provides no atomic no-clobber rename (renamex_np/renameat2 \
             unsupported). Refusing to fall back to a plain rename, which could clobber a \
             canonical DB another process created concurrently and lose data. Migrate offline \
             (with no other Tachi process running) or on a supported filesystem, then retry.",
            legacy_path.display(),
            db_path.display(),
        ),
    ))
}

/// Loud hard error for the ENOENT case where the legacy file vanished AND no
/// canonical file exists to converge on: the pre-#1132 data was removed out
/// from under us by something other than a migration (a delete, an external
/// move). There is nothing left to bring forward — fail loud rather than
/// silently open an empty DB.
fn legacy_vanished_error(legacy_path: &Path, db_path: &Path) -> MemoryError {
    MemoryError::Io(std::io::Error::new(
        std::io::ErrorKind::NotFound,
        format!(
            "#1132 legacy DB filename migration: the legacy file {} vanished during migration and \
             no canonical file {} exists to converge on. The pre-#1132 data appears to have been \
             removed (not migrated) concurrently. Refusing to open an empty DB under the canonical \
             name — investigate what removed it, restore from backup if needed, and retry.",
            legacy_path.display(),
            db_path.display(),
        ),
    ))
}

/// Validate that a pre-existing link at the legacy path is exactly the compat
/// shim this seam itself writes: a *relative* symlink to the canonical sibling
/// filename [`MEMORY_DB_FILENAME`]. Any other target — an absolute path, an old
/// DB location, a leftover from a prior move — is stale/corrupted state that
/// would silently resolve old-name readers to the WRONG data (split-brain).
/// Refuse it loudly (RESIDUAL-2), same posture as the both-real-files case,
/// rather than accepting it as a healthy shim.
fn validate_compat_symlink(legacy_path: &Path) -> Result<(), MemoryError> {
    match std::fs::read_link(legacy_path) {
        Ok(target) if target == Path::new(MEMORY_DB_FILENAME) => Ok(()),
        Ok(target) => Err(MemoryError::Io(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!(
                "#1132 legacy DB filename migration: the legacy path {} is a symlink pointing at \
                 {} instead of the canonical sibling {}. This is a stale/corrupted compat link \
                 that would resolve old-name readers to the wrong data. Refusing to proceed — \
                 reconcile the link manually (repoint it at ./{} or remove it) and retry.",
                legacy_path.display(),
                target.display(),
                MEMORY_DB_FILENAME,
                MEMORY_DB_FILENAME,
            ),
        ))),
        Err(e) => Err(MemoryError::Io(std::io::Error::new(
            e.kind(),
            format!(
                "#1132 legacy DB filename migration: the legacy path {} is a symlink but its target \
                 could not be read: {e}. Refusing to proceed until the link state is known, rather \
                 than assume it points at the canonical DB.",
                legacy_path.display()
            ),
        ))),
    }
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

    // #1226 round-4: the atomic no-clobber rename primitive must refuse to
    // overwrite a canonical file a concurrent process already created, reporting
    // EEXIST (DestinationExists) and leaving BOTH files byte-for-byte untouched.
    // Deterministic: we pre-create the canonical file ourselves to stand in for
    // the concurrent winner, then call the primitive directly (the outer state
    // machine never reaches the rename when the canonical already exists at stat
    // time, so this exercises the no-clobber guard in isolation).
    #[cfg(all(unix, any(target_os = "macos", target_os = "ios", target_os = "linux")))]
    #[test]
    fn atomic_rename_refuses_to_clobber_an_existing_canonical() {
        let dir = tempfile::tempdir().expect("tempdir");
        let legacy = dir.path().join(LEGACY_MEMORY_DB_FILENAME);
        let canonical = dir.path().join(MEMORY_DB_FILENAME);
        // A concurrent process already created + populated the canonical file.
        std::fs::write(&canonical, b"winner's canonical data").expect("write canonical");
        std::fs::write(&legacy, b"our legacy data").expect("write legacy");

        let outcome = atomic_rename_no_clobber(&legacy, &canonical)
            .expect("a pre-existing destination must be a no-clobber refusal, not an error");
        assert!(
            matches!(outcome, AtomicRename::DestinationExists),
            "a pre-existing canonical must yield DestinationExists (EEXIST), never a clobber"
        );
        // The concurrent winner's bytes must be intact — NOT clobbered.
        assert_eq!(
            std::fs::read(&canonical).expect("read canonical"),
            b"winner's canonical data",
            "the concurrently-created canonical file must not be clobbered"
        );
        // Our legacy file must be left exactly where it was.
        assert_eq!(
            std::fs::read(&legacy).expect("read legacy"),
            b"our legacy data",
            "the legacy file must be untouched when the atomic rename is refused"
        );
        assert!(
            std::fs::symlink_metadata(&legacy)
                .expect("legacy metadata")
                .file_type()
                .is_file(),
            "the refused legacy file must not have been converted to a symlink"
        );
    }

    // The success path of the same primitive: when the canonical name is free,
    // the atomic rename moves the legacy file onto it and vacates the old name.
    #[cfg(all(unix, any(target_os = "macos", target_os = "ios", target_os = "linux")))]
    #[test]
    fn atomic_rename_moves_the_legacy_file_when_canonical_is_absent() {
        let dir = tempfile::tempdir().expect("tempdir");
        let legacy = dir.path().join(LEGACY_MEMORY_DB_FILENAME);
        let canonical = dir.path().join(MEMORY_DB_FILENAME);
        std::fs::write(&legacy, b"pre-#1132 data").expect("write legacy");

        let outcome = atomic_rename_no_clobber(&legacy, &canonical)
            .expect("rename onto a free name must succeed");
        assert!(
            matches!(outcome, AtomicRename::Renamed),
            "renaming onto a free canonical name must report Renamed"
        );
        assert_eq!(
            std::fs::read(&canonical).expect("read canonical"),
            b"pre-#1132 data",
            "the atomic move must preserve the legacy bytes exactly"
        );
        assert!(
            !legacy.exists(),
            "the legacy name must be free after a successful atomic move"
        );
    }

    // #1226 round-5: when the legacy source is gone, the primitive must report
    // SourceVanished (ENOENT), not a generic error — so the caller can converge
    // on a concurrent migrator instead of failing spuriously. Deterministic:
    // with BOTH names absent the only possible errno is ENOENT (missing source),
    // with no EEXIST ambiguity.
    #[cfg(all(unix, any(target_os = "macos", target_os = "ios", target_os = "linux")))]
    #[test]
    fn atomic_rename_reports_source_vanished_when_legacy_is_absent() {
        let dir = tempfile::tempdir().expect("tempdir");
        let legacy = dir.path().join(LEGACY_MEMORY_DB_FILENAME); // never created
        let canonical = dir.path().join(MEMORY_DB_FILENAME); // also absent

        let outcome = atomic_rename_no_clobber(&legacy, &canonical)
            .expect("a missing source must be a SourceVanished signal, not an error");
        assert!(
            matches!(outcome, AtomicRename::SourceVanished),
            "a vanished legacy source (ENOENT) must report SourceVanished"
        );
    }

    // #1226 round-5 (codex secondary): a concurrent migrator moved the legacy
    // file onto the canonical name before our rename. Our ENOENT must CONVERGE
    // on the peer's canonical file (not hard-error), leaving the compat symlink
    // and never touching the peer's bytes.
    #[cfg(unix)]
    #[test]
    fn source_vanished_converges_when_canonical_now_exists() {
        let dir = tempfile::tempdir().expect("tempdir");
        let legacy = dir.path().join(LEGACY_MEMORY_DB_FILENAME); // peer already moved it away
        let canonical = dir.path().join(MEMORY_DB_FILENAME);
        std::fs::write(&canonical, b"migrated by a concurrent peer").expect("write canonical");

        converge_after_source_vanished(&legacy, &canonical)
            .expect("a peer-migrated canonical must converge, not fail");

        assert_eq!(
            std::fs::read(&canonical).expect("read canonical"),
            b"migrated by a concurrent peer",
            "the concurrent peer's canonical data must not be touched"
        );
        let legacy_meta = std::fs::symlink_metadata(&legacy)
            .expect("convergence must leave a compat symlink where the legacy file was");
        assert!(
            legacy_meta.file_type().is_symlink(),
            "converging on a peer's migration must (re-)leave the compat symlink"
        );
        assert_eq!(
            std::fs::read_link(&legacy).expect("read compat symlink target"),
            std::path::PathBuf::from(MEMORY_DB_FILENAME),
            "the compat symlink must point at the canonical filename"
        );
    }

    // The other side of ENOENT: the legacy file genuinely vanished (a delete /
    // external move, NOT a migration) and no canonical exists to converge on.
    // There is nothing to bring forward — fail loud, never open an empty DB.
    #[cfg(unix)]
    #[test]
    fn source_vanished_fails_loud_when_canonical_also_absent() {
        let dir = tempfile::tempdir().expect("tempdir");
        let legacy = dir.path().join(LEGACY_MEMORY_DB_FILENAME); // gone
        let canonical = dir.path().join(MEMORY_DB_FILENAME); // also gone

        let err = converge_after_source_vanished(&legacy, &canonical)
            .expect_err("a genuinely-vanished legacy with no canonical must fail loud");
        assert!(
            err.to_string().contains("vanished during migration"),
            "error must name the vanished-data condition: {err}"
        );
        assert!(
            !canonical.exists(),
            "a failed convergence must NOT have created an empty canonical DB"
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
    fn migrate_fails_loud_on_a_legacy_symlink_pointing_at_the_wrong_target() {
        // RESIDUAL-2: a stale/corrupted `memory.db` symlink pointing at some
        // OTHER path (not the canonical sibling) must NOT be silently accepted —
        // following it would resolve old-name readers to the wrong data. Fail
        // loud in BOTH the canonical-absent and canonical-present cases.
        for canonical_exists in [false, true] {
            let dir = tempfile::tempdir().expect("tempdir");
            let target = dir.path().join(MEMORY_DB_FILENAME);
            let legacy = dir.path().join(LEGACY_MEMORY_DB_FILENAME);
            // A link to a plausible-but-wrong old location.
            std::os::unix::fs::symlink("../old-place/memory.db", &legacy)
                .expect("plant wrong-target symlink");
            if canonical_exists {
                std::fs::write(&target, b"canonical data").expect("write canonical");
            }

            let err = migrate_legacy_filename_if_present(&target).expect_err(
                "a legacy symlink pointing at the wrong target must fail loud, not be accepted",
            );
            let message = err.to_string();
            assert!(
                message.contains("stale/corrupted compat link"),
                "error must name the corrupted-link condition (canonical_exists={canonical_exists}): {message}"
            );
            assert!(
                message.contains("wrong data"),
                "error must make the wrong-data risk explicit: {message}"
            );
            // The seam must not have touched anything.
            assert_eq!(
                std::fs::read_link(&legacy).expect("legacy link"),
                std::path::PathBuf::from("../old-place/memory.db"),
                "the suspect symlink must be left exactly as it was"
            );
            if canonical_exists {
                assert_eq!(
                    std::fs::read(&target).expect("read canonical"),
                    b"canonical data",
                    "canonical data must be untouched"
                );
            } else {
                assert!(!target.exists(), "no canonical file may be created");
            }
        }
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
