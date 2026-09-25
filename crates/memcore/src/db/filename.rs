//! Canonical on-disk filename for a memory database, plus the one-time
//! explicit offline conversion away from the pre-#1132 `memory.db` name.
//!
//! Naming ruling (owner-ratified 2026-07-17, tachi#1132): "system in the
//! filename, scope stays in the directory" — every standalone-Tachi store
//! (`~/.tachi/global/`, `~/.tachi/projects/<name>/`, repo-local `.tachi/`)
//! converts its file `memory.db` -> `tachi-memory.db` offline. Ordinary write
//! opens call [`migrate_legacy_filename_if_present`] only to refuse unsafe
//! states; conversion is an explicit operator action.

use std::path::{Path, PathBuf};

use rusqlite::{types::ValueRef, Connection, OpenFlags};
use sha2::{Digest, Sha256};

use crate::error::MemoryError;

/// Canonical memory-database filename. Every fresh DB created from here on
/// uses this name.
pub const MEMORY_DB_FILENAME: &str = "tachi-memory.db";

/// Pre-#1132 filename. Never used to CREATE a new DB — only consulted here to
/// detect and migrate an existing file forward, and left behind as a
/// one-release-window compat symlink pointing at [`MEMORY_DB_FILENAME`].
pub const LEGACY_MEMORY_DB_FILENAME: &str = "memory.db";

/// An immutable read ignores pending WAL and rollback journals. Refuse to
/// classify their main file as a complete committed view. A zero-byte sidecar
/// is harmless, but an uninspectable one is never treated as absent.
pub fn pending_legacy_sidecar(db_path: &Path) -> Result<Option<PathBuf>, MemoryError> {
    for suffix in ["-wal", "-journal"] {
        let sidecar = sqlite_sidecar_path(db_path, suffix);
        match std::fs::symlink_metadata(&sidecar) {
            Ok(meta) if !meta.file_type().is_file() || meta.len() != 0 => {
                return Ok(Some(sidecar));
            }
            Ok(_) => {}
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
            Err(err) => return Err(stat_error("sidecar", &sidecar, err)),
        }
    }
    Ok(None)
}

fn sqlite_sidecar_path(db_path: &Path, suffix: &str) -> PathBuf {
    // SQLite appends suffixes to the raw pathname, not to its lossy Display
    // rendering. Non-UTF8 directories must not hide a live WAL.
    let mut name = db_path.as_os_str().to_os_string();
    name.push(suffix);
    PathBuf::from(name)
}

fn refuse_orphaned_sidecars(canonical: &Path) -> Result<(), MemoryError> {
    let legacy = canonical.with_file_name(LEGACY_MEMORY_DB_FILENAME);
    if let Some(sidecar) = pending_legacy_sidecar(&legacy)? {
        return Err(MemoryError::InvalidArg(format!(
            "offline filename reconciliation required: legacy sidecar {} may hold committed data; refusing to open or create {}",
            sidecar.display(), canonical.display()
        )));
    }
    Ok(())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OfflineFilenameOutcome {
    Converted,
    RecoveredLink,
    AlreadyConverted,
}

pub struct OfflineFilenameAuthority {
    _private: (),
}

impl OfflineFilenameAuthority {
    /// Construct only at the explicit `--rename-legacy --apply --offline`
    /// operator call site. This is an attestation, not proof against old bins.
    pub fn operator_attestation() -> Self {
        Self { _private: () }
    }
}

/// Distinct operator authority: only call after the CLI has required an
/// explicit offline attestation and checked daemon liveness. This does NOT
/// fence old binaries: every reader/writer must be stopped and prevented from
/// restarting by the operator until conversion and schema migration finish.
pub fn convert_legacy_filename_offline(
    canonical: &Path,
    _authority: &OfflineFilenameAuthority,
) -> Result<OfflineFilenameOutcome, MemoryError> {
    if canonical.file_name().and_then(|name| name.to_str()) != Some(MEMORY_DB_FILENAME) {
        return Err(MemoryError::InvalidArg(
            "offline filename conversion requires a canonical tachi-memory.db target".to_string(),
        ));
    }
    let parent = canonical
        .parent()
        .ok_or_else(|| MemoryError::InvalidArg("missing store directory".to_string()))?;
    let legacy = canonical.with_file_name(LEGACY_MEMORY_DB_FILENAME);
    // A persistent lock inode avoids unlink/recreate races. flock only
    // serializes cooperating NEW converters, never a deployed old binary.
    let lock = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .open(parent.join(".tachi-filename-conversion.lock"))?;
    #[cfg(any(target_os = "macos", target_os = "ios", target_os = "linux"))]
    let _lock = {
        use std::os::fd::AsRawFd;
        if unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0 {
            return Err(MemoryError::InvalidArg(format!(
                "offline filename conversion is already running for {}",
                parent.display()
            )));
        }
        lock
    };
    #[cfg(not(any(target_os = "macos", target_os = "ios", target_os = "linux")))]
    {
        let _ = lock;
        return Err(MemoryError::InvalidArg(
            "offline filename conversion requires Unix filesystem locking and no-clobber rename"
                .to_string(),
        ));
    }

    let legacy_kind = classify_legacy_sibling(&legacy)?;
    let canonical_present = match std::fs::symlink_metadata(canonical) {
        Ok(meta) if meta.file_type().is_file() => true,
        Ok(_) => {
            return Err(MemoryError::InvalidArg(format!(
                "canonical target {} is not a regular file",
                canonical.display()
            )))
        }
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => false,
        Err(err) => return Err(stat_error("canonical", canonical, err)),
    };
    if canonical_present {
        match legacy_kind {
            LegacySibling::RealFile => return Err(both_real_files_error(canonical, &legacy)),
            LegacySibling::Symlink => validate_compat_symlink(&legacy)?,
            LegacySibling::Absent => {}
        }
        refuse_orphaned_sidecars(canonical)?;
        // This is also the post-rename/pre-link interruption recovery path:
        // never infer success solely from the file's existence.
        let _ = inspect_offline_database(canonical)?;
        if matches!(legacy_kind, LegacySibling::Absent) {
            leave_compat_symlink(&legacy)?;
            sync_parent(parent)?;
            return Ok(OfflineFilenameOutcome::RecoveredLink);
        }
        return Ok(OfflineFilenameOutcome::AlreadyConverted);
    }
    if !matches!(legacy_kind, LegacySibling::RealFile) {
        return Err(MemoryError::InvalidArg(format!("offline conversion requires a real legacy file at {}; no empty canonical database will be created", legacy.display())));
    }
    if !std::fs::symlink_metadata(&legacy)?.file_type().is_file() {
        return Err(MemoryError::InvalidArg(format!(
            "legacy source {} is not a regular file",
            legacy.display()
        )));
    }

    let source_identity = file_identity(&legacy)?;
    let before = recover_and_checkpoint(&legacy)?;
    pause_for_interruption_test("after_checkpoint");
    if file_identity(&legacy)? != source_identity {
        return Err(MemoryError::InvalidArg(
            "legacy source identity changed during SQLite recovery; refusing rename".to_string(),
        ));
    }
    if pending_legacy_sidecar(&legacy)?.is_some() {
        return Err(MemoryError::InvalidArg(
            "legacy WAL/journal remains pending after checkpoint; refusing rename".to_string(),
        ));
    }
    // Reopen the self-contained main file with immutable mode: no stale WAL
    // may contribute to this second observation. The operator's offline
    // precondition is what excludes a new old-binary writer after this check.
    let after_checkpoint = inspect_offline_database(&legacy)?;
    if before != after_checkpoint {
        return Err(MemoryError::InvalidArg(
            "committed rows or schema changed during checkpoint; refusing rename".to_string(),
        ));
    }
    match std::fs::symlink_metadata(canonical) {
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
        Ok(_) => return Err(both_real_files_error(canonical, &legacy)),
        Err(err) => return Err(stat_error("canonical", canonical, err)),
    }
    // Full snapshot and parent are synced before moving the now self-contained
    // main file. No WAL/SHM/journal file is ever moved or discarded manually.
    std::fs::File::open(&legacy)?.sync_all()?;
    sync_parent(parent)?;
    pause_for_interruption_test("before_rename");
    match atomic_rename_no_clobber(&legacy, canonical)
        .map_err(|err| rename_error(&legacy, canonical, err))?
    {
        AtomicRename::Renamed => {}
        AtomicRename::Unsupported => {
            return Err(atomic_rename_unsupported_error(&legacy, canonical))
        }
        AtomicRename::DestinationExists | AtomicRename::SourceVanished => {
            return Err(MemoryError::InvalidArg("offline filename rename did not exclusively move the verified source; reconcile files before retry".to_string()));
        }
    }
    sync_parent(parent)?;
    pause_for_interruption_test("after_rename");
    if file_identity(canonical)? != source_identity
        || inspect_offline_database(canonical)? != before
    {
        return Err(MemoryError::InvalidArg("post-rename identity or committed content differs; canonical-only state preserved for manual reconciliation".to_string()));
    }
    refuse_orphaned_sidecars(canonical)?;
    leave_compat_symlink(&legacy)?;
    sync_parent(parent)?;
    pause_for_interruption_test("after_link");
    Ok(OfflineFilenameOutcome::Converted)
}

#[cfg(unix)]
fn file_identity(path: &Path) -> Result<(u64, u64), MemoryError> {
    use std::os::unix::fs::MetadataExt;
    let meta = std::fs::symlink_metadata(path)?;
    if !meta.file_type().is_file() {
        return Err(MemoryError::InvalidArg(format!(
            "{} is not a real regular file",
            path.display()
        )));
    }
    Ok((meta.dev(), meta.ino()))
}

#[cfg(not(unix))]
fn file_identity(_path: &Path) -> Result<(u64, u64), MemoryError> {
    Err(MemoryError::InvalidArg(
        "offline conversion is unsupported on this platform".to_string(),
    ))
}

fn sync_parent(parent: &Path) -> Result<(), MemoryError> {
    std::fs::File::open(parent)?.sync_all()?;
    Ok(())
}

#[derive(PartialEq, Eq)]
struct OfflineSnapshot {
    version: u32,
    memories: (i64, [u8; 32]),
    edges: (i64, [u8; 32]),
}

fn snapshot(conn: &Connection) -> Result<OfflineSnapshot, MemoryError> {
    let version = super::migrations::read_schema_version(conn)?;
    if version == 0 || version > super::migrations::EXPECTED_SCHEMA_VERSION {
        return Err(MemoryError::InvalidArg(format!(
            "offline filename conversion refuses unknown/unstamped or newer schema {version}"
        )));
    }
    let known: i64 = conn.query_row("SELECT count(*) FROM sqlite_schema WHERE type='table' AND name IN ('memories','memory_edges','hard_state')", [], |row| row.get(0))?;
    if known != 3 {
        return Err(MemoryError::InvalidArg("offline filename conversion requires a stamped Tachi database with memories, memory_edges and hard_state".to_string()));
    }
    let migration_evidence: i64 = conn.query_row(
        "SELECT count(*) FROM hard_state WHERE namespace = 'migrations'",
        [],
        |row| row.get(0),
    )?;
    if migration_evidence == 0 {
        return Err(MemoryError::InvalidArg(
            "offline filename conversion refuses a file without Tachi migration evidence"
                .to_string(),
        ));
    }
    let private: i64 = conn.query_row("SELECT count(*) FROM hard_state WHERE namespace = 'store_identity' AND key = 'private_partition'", [], |row| row.get(0))?;
    if private != 0 {
        return Err(MemoryError::InvalidArg(
            "offline filename conversion refuses a private partition".to_string(),
        ));
    }
    let integrity: String = conn.query_row("PRAGMA integrity_check", [], |row| row.get(0))?;
    if integrity != "ok" {
        return Err(MemoryError::InvalidArg(format!(
            "offline filename conversion integrity check failed: {integrity}"
        )));
    }
    Ok(OfflineSnapshot {
        version,
        memories: table_fingerprint(conn, "SELECT * FROM memories ORDER BY id")?,
        edges: table_fingerprint(
            conn,
            "SELECT * FROM memory_edges ORDER BY source_id, target_id, relation",
        )?,
    })
}

fn table_fingerprint(conn: &Connection, sql: &str) -> Result<(i64, [u8; 32]), MemoryError> {
    let mut statement = conn.prepare(sql)?;
    let columns = statement.column_count();
    let mut rows = statement.query([])?;
    let mut count = 0i64;
    let mut hash = Sha256::new();
    hash.update((columns as u64).to_le_bytes());
    while let Some(row) = rows.next()? {
        count += 1;
        for column in 0..columns {
            match row.get_ref(column)? {
                ValueRef::Null => hash.update([0]),
                ValueRef::Integer(value) => {
                    hash.update([1]);
                    hash.update(value.to_le_bytes());
                }
                ValueRef::Real(value) => {
                    hash.update([2]);
                    hash.update(value.to_bits().to_le_bytes());
                }
                ValueRef::Text(bytes) => {
                    hash.update([3]);
                    hash.update((bytes.len() as u64).to_le_bytes());
                    hash.update(bytes);
                }
                ValueRef::Blob(bytes) => {
                    hash.update([4]);
                    hash.update((bytes.len() as u64).to_le_bytes());
                    hash.update(bytes);
                }
            }
        }
    }
    Ok((count, hash.finalize().into()))
}

fn inspect_offline_database(path: &Path) -> Result<OfflineSnapshot, MemoryError> {
    if let Some(sidecar) = pending_legacy_sidecar(path)? {
        return Err(MemoryError::InvalidArg(format!(
            "immutable verification cannot ignore pending sidecar {}",
            sidecar.display()
        )));
    }
    super::enable_simple_auto_extension()
        .map_err(|err| MemoryError::InvalidArg(format!("simple tokenizer init: {err}")))?;
    super::register_sqlite_vec();
    // Encode raw filesystem bytes; lossy UTF-8 would open another filename.
    #[cfg(unix)]
    let bytes = {
        use std::os::unix::ffi::OsStrExt;
        path.as_os_str().as_bytes()
    };
    #[cfg(not(unix))]
    let owned = path.to_string_lossy().into_owned();
    #[cfg(not(unix))]
    let bytes = owned.as_bytes();
    let mut encoded = String::new();
    for &byte in bytes {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'/' | b'-' | b'.' | b'_' | b'~' | b':') {
            encoded.push(char::from(byte));
        } else {
            use std::fmt::Write as _;
            write!(&mut encoded, "%{byte:02X}").expect("encode URI byte");
        }
    }
    let uri = format!("file:{encoded}?mode=ro&immutable=1");
    let conn = Connection::open_with_flags(
        uri,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_URI,
    )?;
    snapshot(&conn)
}

fn recover_and_checkpoint(path: &Path) -> Result<OfflineSnapshot, MemoryError> {
    super::enable_simple_auto_extension()
        .map_err(|err| MemoryError::InvalidArg(format!("simple tokenizer init: {err}")))?;
    super::register_sqlite_vec();
    let conn = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_WRITE)?;
    conn.busy_timeout(std::time::Duration::from_secs(5))?;
    // SQLite itself recovers a hot rollback journal on this pathname. An
    // exclusive transaction discriminates an active writer/reader (no override).
    conn.execute_batch("BEGIN EXCLUSIVE")?;
    let before = snapshot(&conn);
    conn.execute_batch("ROLLBACK")?;
    let before = before?;
    pause_for_interruption_test("before_checkpoint");
    super::checkpoint_wal_truncate(&conn)?;
    drop(conn);
    Ok(before)
}

/// Only the memcore unit-test binary compiles this pause. The child writes a
/// marker and waits for a parent SIGKILL; production has no env-controlled
/// pause, migration authority or safety bypass.
#[cfg(test)]
fn pause_for_interruption_test(stage: &str) {
    if std::env::var("TACHI_TEST_CONVERSION_PAUSE").as_deref() == Ok(stage) {
        let marker = std::env::var("TACHI_TEST_CONVERSION_MARKER").expect("test marker path");
        std::fs::write(marker, stage).expect("write test pause marker");
        loop {
            std::thread::park();
        }
    }
}

#[cfg(not(test))]
fn pause_for_interruption_test(_stage: &str) {}

/// Resolve the database a strictly read-only caller may inspect without
/// performing the write-side filename migration. The same split-brain and
/// compat-symlink rules apply, but this function never renames, creates, or
/// relinks either sibling.
pub fn resolve_memory_db_read_path(db_path: &Path) -> Result<PathBuf, MemoryError> {
    let is_canonical_name = db_path
        .file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| name == MEMORY_DB_FILENAME);
    if !is_canonical_name {
        return Ok(db_path.to_path_buf());
    }

    let legacy_path = db_path.with_file_name(LEGACY_MEMORY_DB_FILENAME);
    let legacy_kind = classify_legacy_sibling(&legacy_path)?;
    let canonical_present = match std::fs::symlink_metadata(db_path) {
        Ok(_) => true,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => false,
        Err(error) => return Err(stat_error("canonical", db_path, error)),
    };

    match (canonical_present, legacy_kind) {
        (false, LegacySibling::RealFile) => Ok(legacy_path),
        (false, LegacySibling::Symlink) => {
            validate_compat_symlink(&legacy_path)?;
            Ok(db_path.to_path_buf())
        }
        (false, LegacySibling::Absent) | (true, LegacySibling::Absent) => Ok(db_path.to_path_buf()),
        (true, LegacySibling::Symlink) => {
            validate_compat_symlink(&legacy_path)?;
            Ok(db_path.to_path_buf())
        }
        (true, LegacySibling::RealFile) => Err(both_real_files_error(db_path, &legacy_path)),
    }
}

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

/// Ordinary-open validation for canonical filenames. The historical name is
/// retained for existing callers, but this function performs **no filename
/// writes**. A real `memory.db` always requires the explicit offline converter
/// regardless of its sidecar sizes or schema authority. Both-real-files and
/// wrong-target links remain hard refusals. An orphan link never creates an
/// empty replacement, and pending legacy WAL/journal blocks canonical opens
/// even when an apparently valid compatibility link exists. Fresh/canonical-
/// only paths proceed without synthesizing a compatibility link.
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
        (true, LegacySibling::RealFile) => {
            Err(both_real_files_error(db_path, &legacy_path))
        }
        (false, LegacySibling::RealFile) => Err(MemoryError::InvalidArg(format!(
            "offline filename conversion required for {} -> {}: ordinary opens never move a legacy SQLite database; stop all old and new readers/writers, take a whole-store backup, then run `tachi migrate --rename-legacy --apply --offline`",
            legacy_path.display(), db_path.display()
        ))),
        (false, LegacySibling::Symlink) => {
            validate_compat_symlink(&legacy_path)?;
            Err(MemoryError::InvalidArg(format!(
                "orphaned compatibility link {} has no canonical target {}; refusing to create an empty database",
                legacy_path.display(), db_path.display()
            )))
        }
        (true, LegacySibling::Symlink) => {
            validate_compat_symlink(&legacy_path)?;
            refuse_orphaned_sidecars(db_path)
        }
        (_, LegacySibling::Absent) => refuse_orphaned_sidecars(db_path),
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

/// Handle the ENOENT / source-vanished case: a concurrent migrator moved the
/// legacy file before our rename. Re-check the canonical file — if the peer
/// already migrated it (canonical now present) CONVERGE exactly as the
/// (canonical present, *) arms do; if the canonical is also absent the data
/// genuinely vanished, so fail loud rather than open an empty DB.
#[cfg(test)]
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
#[cfg(test)]
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
#[cfg(test)]
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

    #[cfg(unix)]
    #[test]
    fn offline_conversion_child_process() {
        let Ok(canonical) = std::env::var("TACHI_TEST_CONVERSION_TARGET") else {
            return;
        };
        convert_legacy_filename_offline(
            Path::new(&canonical),
            &OfflineFilenameAuthority::operator_attestation(),
        )
        .expect("uninterrupted child conversion");
    }

    #[cfg(unix)]
    #[test]
    fn hot_journal_child_process() {
        let Ok(path) = std::env::var("TACHI_TEST_HOT_JOURNAL_SOURCE") else {
            return;
        };
        let conn = Connection::open(&path).unwrap();
        conn.execute_batch("PRAGMA journal_mode=DELETE; BEGIN IMMEDIATE;")
            .unwrap();
        conn.execute(
            "UPDATE memories SET text = 'uncommitted change' WHERE id = 'offline-preservation-row'",
            [],
        )
        .unwrap();
        let marker = std::env::var("TACHI_TEST_HOT_JOURNAL_MARKER").unwrap();
        std::fs::write(marker, b"journal transaction is open").unwrap();
        loop {
            std::thread::park();
        }
    }

    #[cfg(unix)]
    #[test]
    fn sqlite_recovers_real_hot_rollback_journal_before_filename_conversion() {
        use std::process::{Command, Stdio};
        let dir = tempfile::tempdir().unwrap();
        let legacy = dir.path().join(LEGACY_MEMORY_DB_FILENAME);
        let canonical = dir.path().join(MEMORY_DB_FILENAME);
        let marker = dir.path().join("hot-journal-marker");
        let entry = fixture_entry();
        {
            let mut store = crate::MemoryStore::open(legacy.to_str().unwrap()).unwrap();
            store.upsert(&entry).unwrap();
        }
        let mut child = Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "db::filename::tests::hot_journal_child_process",
                "--nocapture",
            ])
            .env("TACHI_TEST_HOT_JOURNAL_SOURCE", &legacy)
            .env("TACHI_TEST_HOT_JOURNAL_MARKER", &marker)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        for _ in 0..250 {
            if marker.exists() {
                break;
            }
            if let Some(status) = child.try_wait().unwrap() {
                panic!("hot journal child exited early: {status}");
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        assert!(marker.exists(), "hot journal transaction never began");
        let journal = sqlite_sidecar_path(&legacy, "-journal");
        assert!(std::fs::metadata(&journal).unwrap().len() > 0);
        child.kill().unwrap();
        child.wait().unwrap();

        // The converter invokes SQLite recovery on the source name. If the
        // journal cannot be proven clean, it may refuse; success must recover
        // and preserve the last committed bytes, not the interrupted update.
        match convert_legacy_filename_offline(
            &canonical,
            &OfflineFilenameAuthority::operator_attestation(),
        ) {
            Ok(OfflineFilenameOutcome::Converted) => {
                let actual: String = Connection::open(&canonical)
                    .unwrap()
                    .query_row(
                        "SELECT text FROM memories WHERE id = ?1",
                        [entry.id.as_str()],
                        |row| row.get(0),
                    )
                    .unwrap();
                assert_eq!(actual.as_bytes(), entry.text.as_bytes());
                assert_eq!(
                    std::fs::read_link(&legacy).unwrap(),
                    Path::new(MEMORY_DB_FILENAME)
                );
            }
            Ok(outcome) => panic!("unexpected hot journal outcome: {outcome:?}"),
            Err(err) => {
                assert!(
                    !canonical.exists(),
                    "refusal cannot leave a partial target: {err}"
                );
                let actual: String = Connection::open(&legacy)
                    .unwrap()
                    .query_row(
                        "SELECT text FROM memories WHERE id = ?1",
                        [entry.id.as_str()],
                        |row| row.get(0),
                    )
                    .unwrap();
                assert_eq!(actual.as_bytes(), entry.text.as_bytes());
            }
        }
    }

    #[cfg(unix)]
    #[test]
    fn interrupted_offline_conversion_recovers_at_each_filesystem_phase() {
        use std::process::{Command, Stdio};
        for phase in [
            "before_checkpoint",
            "after_checkpoint",
            "before_rename",
            "after_rename",
            "after_link",
        ] {
            let dir = tempfile::tempdir().expect("temp dir");
            let legacy = dir.path().join(LEGACY_MEMORY_DB_FILENAME);
            let canonical = dir.path().join(MEMORY_DB_FILENAME);
            let marker = dir.path().join("test-pause-marker");
            let entry = fixture_entry();
            {
                let mut store = crate::MemoryStore::open(legacy.to_str().unwrap()).unwrap();
                store.upsert(&entry).unwrap();
            }
            let mut child = Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "db::filename::tests::offline_conversion_child_process",
                    "--nocapture",
                ])
                .env("TACHI_TEST_CONVERSION_TARGET", &canonical)
                .env("TACHI_TEST_CONVERSION_PAUSE", phase)
                .env("TACHI_TEST_CONVERSION_MARKER", &marker)
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
                .expect("spawn converter test child");
            for _ in 0..250 {
                if marker.exists() {
                    break;
                }
                if let Some(exit) = child.try_wait().unwrap() {
                    panic!("converter child exited before {phase}: {exit}");
                }
                std::thread::sleep(std::time::Duration::from_millis(20));
            }
            assert!(marker.exists(), "child never reached {phase}");
            child.kill().expect("interrupt child at controlled phase");
            child.wait().expect("reap interrupted child");
            let outcome = convert_legacy_filename_offline(
                &canonical,
                &OfflineFilenameAuthority::operator_attestation(),
            )
            .expect("explicit retry after interrupted conversion");
            assert!(matches!(
                outcome,
                OfflineFilenameOutcome::Converted
                    | OfflineFilenameOutcome::RecoveredLink
                    | OfflineFilenameOutcome::AlreadyConverted
            ));
            let actual: String = Connection::open(&canonical)
                .unwrap()
                .query_row(
                    "SELECT text FROM memories WHERE id = ?1",
                    [entry.id.as_str()],
                    |row| row.get(0),
                )
                .unwrap();
            assert_eq!(
                actual.as_bytes(),
                entry.text.as_bytes(),
                "committed row lost at {phase}"
            );
            assert_eq!(
                std::fs::read_link(&legacy).unwrap(),
                Path::new(MEMORY_DB_FILENAME)
            );
        }
    }

    fn fixture_entry() -> crate::MemoryEntry {
        crate::MemoryEntry {
            id: "offline-preservation-row".to_string(),
            path: "/facts/offline".to_string(),
            summary: "preserve committed row".to_string(),
            text: "exact committed bytes: µ".to_string(),
            importance: 0.8,
            timestamp: chrono::Utc::now().to_rfc3339(),
            valid_from: String::new(),
            valid_until: None,
            category: "fact".to_string(),
            topic: "filename".to_string(),
            keywords: vec![],
            persons: vec![],
            entities: vec![],
            location: String::new(),
            source: "test".to_string(),
            scope: "project".to_string(),
            archived: false,
            access_count: 0,
            scored_count: 0,
            last_access: None,
            last_use_at: None,
            revision: 1,
            metadata: serde_json::json!({}),
            vector: None,
            retention_policy: Some("durable".to_string()),
            domain: Some("coding".to_string()),
            recall_count: 0,
            query_diversity: 0,
            tier: "raw".to_string(),
        }
    }

    #[cfg(unix)]
    #[test]
    fn ordinary_open_refuses_legacy_with_committed_row_and_offline_converter_preserves_it() {
        let dir = tempfile::tempdir().unwrap();
        let old = dir.path().join(LEGACY_MEMORY_DB_FILENAME);
        let new = dir.path().join(MEMORY_DB_FILENAME);
        let entry = fixture_entry();
        let old_version;
        {
            let mut store = crate::MemoryStore::open(old.to_str().unwrap()).unwrap();
            store.upsert(&entry).unwrap();
            old_version =
                super::super::migrations::read_schema_version(store.connection()).unwrap();
        }
        // Reopen via SQLite itself, not an immutable main-only probe: pending
        // WAL rows are part of committed state even after the old handle exits.
        let expected: String = Connection::open(&old)
            .unwrap()
            .query_row(
                "SELECT text FROM memories WHERE id = ?1",
                [entry.id.as_str()],
                |row| row.get(0),
            )
            .unwrap();
        for authority in [
            crate::DbOpenContext::open_existing_deny(),
            crate::DbOpenContext::open_existing_allow("test explicit schema authority"),
        ] {
            match crate::MemoryStore::open_with_label_and_context(
                new.to_str().unwrap(),
                "unknown",
                &authority,
            ) {
                Err(err) => assert!(
                    err.to_string()
                        .contains("offline filename conversion required"),
                    "{err}"
                ),
                Ok(_) => panic!("ordinary write open must not rename legacy DB"),
            }
        }
        match crate::MemoryStore::open_read_only(new.to_str().unwrap()) {
            Err(err) => assert!(err
                .to_string()
                .contains("offline filename conversion required")),
            Ok(_) => panic!("ordinary read open must not hide legacy DB"),
        }
        assert!(!new.exists());
        assert!(old.is_file());
        assert_eq!(
            convert_legacy_filename_offline(
                &new,
                &OfflineFilenameAuthority::operator_attestation()
            )
            .unwrap(),
            OfflineFilenameOutcome::Converted
        );
        assert_eq!(
            super::super::migrations::read_schema_version(&Connection::open(&new).unwrap())
                .unwrap(),
            old_version
        );
        let actual: String = Connection::open(&new)
            .unwrap()
            .query_row(
                "SELECT text FROM memories WHERE id = ?1",
                [entry.id.as_str()],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(actual.as_bytes(), expected.as_bytes());
        assert_eq!(
            convert_legacy_filename_offline(
                &new,
                &OfflineFilenameAuthority::operator_attestation()
            )
            .unwrap(),
            OfflineFilenameOutcome::AlreadyConverted
        );
    }

    #[cfg(unix)]
    #[test]
    fn explicit_retry_repairs_only_verified_canonical_without_a_link() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join(MEMORY_DB_FILENAME);
        crate::MemoryStore::open(target.to_str().unwrap()).unwrap();
        let legacy = dir.path().join(LEGACY_MEMORY_DB_FILENAME);
        assert!(!legacy.exists());
        migrate_legacy_filename_if_present(&target).expect("ordinary open accepts canonical only");
        assert!(!legacy.exists());
        assert_eq!(
            convert_legacy_filename_offline(
                &target,
                &OfflineFilenameAuthority::operator_attestation()
            )
            .unwrap(),
            OfflineFilenameOutcome::RecoveredLink
        );
        assert_eq!(
            std::fs::read_link(&legacy).unwrap(),
            Path::new(MEMORY_DB_FILENAME)
        );
    }

    #[cfg(unix)]
    #[test]
    fn orphaned_legacy_wal_blocks_canonical_even_with_valid_link() {
        let dir = tempfile::tempdir().unwrap();
        let canonical = dir.path().join(MEMORY_DB_FILENAME);
        crate::MemoryStore::open(canonical.to_str().unwrap()).unwrap();
        let legacy = dir.path().join(LEGACY_MEMORY_DB_FILENAME);
        std::os::unix::fs::symlink(MEMORY_DB_FILENAME, &legacy).unwrap();
        let wal = dir.path().join("memory.db-wal");
        std::fs::write(&wal, b"unrecovered WAL frames").unwrap();
        match crate::MemoryStore::open(canonical.to_str().unwrap()) {
            Err(err) => assert!(err.to_string().contains("legacy sidecar")),
            Ok(_) => panic!("canonical open must not ignore an orphaned WAL"),
        }
        assert_eq!(std::fs::read(&wal).unwrap(), b"unrecovered WAL frames");
        assert!(convert_legacy_filename_offline(
            &canonical,
            &OfflineFilenameAuthority::operator_attestation(),
        )
        .is_err());
        assert_eq!(std::fs::read(&wal).unwrap(), b"unrecovered WAL frames");
    }

    #[cfg(unix)]
    #[test]
    fn pending_wal_under_non_utf8_directory_is_not_hidden_by_display_loss() {
        use std::os::unix::ffi::{OsStrExt, OsStringExt};
        let dir = tempfile::tempdir().unwrap();
        let odd = dir
            .path()
            .join(std::ffi::OsString::from_vec(b"store-\xff".to_vec()));
        let db = odd.join(LEGACY_MEMORY_DB_FILENAME);
        let wal = sqlite_sidecar_path(&db, "-wal");
        assert!(wal
            .as_os_str()
            .as_bytes()
            .ends_with(b"store-\xff/memory.db-wal"));
        assert_ne!(wal, PathBuf::from(format!("{}-wal", db.display())));
        if let Err(err) = std::fs::create_dir(&odd) {
            // Some macOS filesystems reject invalid UTF-8 path components
            // altogether. The raw-byte invariant is still checked above.
            assert_eq!(err.raw_os_error(), Some(libc::EILSEQ));
            return;
        }
        std::fs::write(&wal, b"committed frames under non-UTF8 parent").unwrap();
        assert_eq!(pending_legacy_sidecar(&db).unwrap(), Some(wal));
    }

    #[cfg(unix)]
    #[test]
    fn active_writer_and_both_real_files_refuse_offline_conversion() {
        let dir = tempfile::tempdir().unwrap();
        let legacy = dir.path().join(LEGACY_MEMORY_DB_FILENAME);
        let canonical = dir.path().join(MEMORY_DB_FILENAME);
        {
            let mut store = crate::MemoryStore::open(legacy.to_str().unwrap()).unwrap();
            store.upsert(&fixture_entry()).unwrap();
        }
        let holder = Connection::open(&legacy).unwrap();
        holder.execute_batch("BEGIN EXCLUSIVE").unwrap();
        assert!(
            convert_legacy_filename_offline(
                &canonical,
                &OfflineFilenameAuthority::operator_attestation(),
            )
            .is_err(),
            "an active transaction cannot be treated as offline"
        );
        assert!(!canonical.exists());
        drop(holder);
        std::fs::write(&canonical, b"independent canonical data").unwrap();
        let legacy_before = std::fs::read(&legacy).unwrap();
        let canonical_before = std::fs::read(&canonical).unwrap();
        assert!(
            convert_legacy_filename_offline(
                &canonical,
                &OfflineFilenameAuthority::operator_attestation(),
            )
            .is_err(),
            "two real files cannot be reconciled automatically"
        );
        assert_eq!(std::fs::read(&legacy).unwrap(), legacy_before);
        assert_eq!(std::fs::read(&canonical).unwrap(), canonical_before);
    }

    #[cfg(unix)]
    #[test]
    fn offline_conversion_refuses_private_partition_without_moving_its_file() {
        let dir = tempfile::tempdir().unwrap();
        let legacy = dir.path().join(LEGACY_MEMORY_DB_FILENAME);
        let canonical = dir.path().join(MEMORY_DB_FILENAME);
        crate::MemoryStore::open(legacy.to_str().unwrap()).unwrap();
        let conn = Connection::open(&legacy).unwrap();
        conn.execute(
            "INSERT INTO hard_state (namespace, key, value_json, version, created_at, updated_at)
             VALUES ('store_identity', 'private_partition', '{\"value\":\"sealed\"}', 1, 'test', 'test')",
            [],
        ).unwrap();
        drop(conn);
        let before = std::fs::read(&legacy).unwrap();
        let err = convert_legacy_filename_offline(
            &canonical,
            &OfflineFilenameAuthority::operator_attestation(),
        )
        .expect_err("sealed private partition must never use generic converter");
        assert!(err.to_string().contains("private partition"), "{err}");
        assert_eq!(std::fs::read(&legacy).unwrap(), before);
        assert!(!canonical.exists());
    }

    #[cfg(unix)]
    #[test]
    fn offline_conversion_refuses_unknown_unstamped_file_without_adopting_it() {
        let dir = tempfile::tempdir().unwrap();
        let legacy = dir.path().join(LEGACY_MEMORY_DB_FILENAME);
        let canonical = dir.path().join(MEMORY_DB_FILENAME);
        Connection::open(&legacy)
            .unwrap()
            .execute_batch("CREATE TABLE foreign_data(x TEXT);")
            .unwrap();
        let before = std::fs::read(&legacy).unwrap();
        let err = convert_legacy_filename_offline(
            &canonical,
            &OfflineFilenameAuthority::operator_attestation(),
        )
        .expect_err("unknown database must not be adopted");
        assert!(err.to_string().contains("unknown/unstamped"), "{err}");
        assert_eq!(std::fs::read(&legacy).unwrap(), before);
        assert!(!canonical.exists());
    }

    #[test]
    fn is_memory_db_filename_accepts_both_generations() {
        assert!(is_memory_db_filename(MEMORY_DB_FILENAME));
        assert!(is_memory_db_filename(LEGACY_MEMORY_DB_FILENAME));
        assert!(!is_memory_db_filename("other.db"));
        assert!(!is_memory_db_filename("tachi-memory.db.bak.20260101"));
    }

    #[test]
    fn read_path_uses_legacy_only_without_mutating_it() {
        let dir = tempfile::tempdir().expect("tempdir");
        let canonical = dir.path().join(MEMORY_DB_FILENAME);
        let legacy = dir.path().join(LEGACY_MEMORY_DB_FILENAME);
        std::fs::write(&legacy, b"legacy data").expect("write legacy");

        assert_eq!(
            resolve_memory_db_read_path(&canonical).expect("resolve legacy-only read path"),
            legacy
        );
        assert!(!canonical.exists());
        assert_eq!(
            std::fs::read(&legacy).expect("legacy unchanged"),
            b"legacy data"
        );
    }

    #[cfg(unix)]
    #[test]
    fn read_path_accepts_only_the_exact_compat_link_and_rejects_two_real_files() {
        let dir = tempfile::tempdir().expect("tempdir");
        let canonical = dir.path().join(MEMORY_DB_FILENAME);
        let legacy = dir.path().join(LEGACY_MEMORY_DB_FILENAME);
        std::fs::write(&canonical, b"canonical data").expect("write canonical");
        std::os::unix::fs::symlink(MEMORY_DB_FILENAME, &legacy).expect("compat link");
        assert_eq!(
            resolve_memory_db_read_path(&canonical).expect("resolve compat link"),
            canonical
        );

        std::fs::remove_file(&legacy).expect("remove compat link");
        std::fs::write(&legacy, b"other real data").expect("write real legacy");
        let error =
            resolve_memory_db_read_path(&canonical).expect_err("two real files must fail closed");
        assert!(error.to_string().contains("both a canonical"), "{error}");
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

        migrate_legacy_filename_if_present(&target).expect("ordinary open must not error");

        assert!(
            !legacy.exists(),
            "ordinary open does not create a compatibility link"
        );
        // The direct converter verifies actual SQLite content, so this
        // byte-only state-machine fixture tests the link primitive itself.
        leave_compat_symlink(&legacy).expect("explicit converter leaves link");

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
    fn orphaned_compat_symlink_refuses_without_creating_a_replacement() {
        let dir = tempfile::tempdir().expect("tempdir");
        let target = dir.path().join(MEMORY_DB_FILENAME);
        let legacy = dir.path().join(LEGACY_MEMORY_DB_FILENAME);
        // Simulate a directory already migrated by a prior run, where the
        // real file has since been deleted (or never landed) but the compat
        // symlink is still sitting there pointing at a name that doesn't
        // exist. An ordinary open must refuse rather than create an empty DB.
        std::os::unix::fs::symlink(MEMORY_DB_FILENAME, &legacy).expect("plant compat symlink");

        let err = migrate_legacy_filename_if_present(&target)
            .expect_err("orphaned link must refuse an ordinary open");
        assert!(err.to_string().contains("orphaned compatibility link"));

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
        crate::MemoryStore::open(legacy.to_str().unwrap()).expect("seed genuine SQLite store");
        let old_version =
            crate::db::migrations::read_schema_version(&Connection::open(&legacy).unwrap())
                .unwrap();

        let err = migrate_legacy_filename_if_present(&target)
            .expect_err("ordinary open must refuse old filename");
        assert!(err
            .to_string()
            .contains("offline filename conversion required"));
        assert!(!target.exists());
        assert!(legacy.is_file());

        assert_eq!(
            convert_legacy_filename_offline(
                &target,
                &OfflineFilenameAuthority::operator_attestation()
            )
            .expect("explicit offline conversion"),
            OfflineFilenameOutcome::Converted
        );

        assert!(
            target.is_file(),
            "the legacy file must now live at the canonical name"
        );
        assert_eq!(
            crate::db::migrations::read_schema_version(&Connection::open(&target).unwrap())
                .unwrap(),
            old_version,
            "filename conversion must not upgrade the schema"
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
            std::fs::read(&target).unwrap(),
        );
    }

    #[cfg(unix)]
    #[test]
    fn migrate_rename_failure_is_loud_not_silent() {
        let dir = tempfile::tempdir().expect("tempdir");
        let target = dir.path().join(MEMORY_DB_FILENAME);
        let legacy = dir.path().join(LEGACY_MEMORY_DB_FILENAME);
        crate::MemoryStore::open(legacy.to_str().unwrap()).expect("seed genuine SQLite store");

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

        let result = convert_legacy_filename_offline(
            &target,
            &OfflineFilenameAuthority::operator_attestation(),
        );

        // Restore write permission BEFORE any assertion can panic and skip
        // cleanup, so the tempdir's Drop can still remove its contents.
        std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(original_mode))
            .expect("restore dir permissions");

        let _err = result.expect_err("a conversion on a non-writable directory must fail loudly");
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
    fn memory_store_open_requires_explicit_offline_conversion_and_preserves_durable_row() {
        let dir = tempfile::tempdir().expect("tempdir");
        let legacy_path = dir.path().join(LEGACY_MEMORY_DB_FILENAME);
        let canonical_path = dir.path().join(MEMORY_DB_FILENAME);

        // Simulate a pre-#1132 install: a real, schema-initialized DB sitting
        // under the OLD literal filename, created by opening that path
        // directly (not through the seam — the seam never fires for a
        // non-canonical target, by design).
        {
            let mut store = crate::MemoryStore::open(legacy_path.to_str().expect("utf8 path"))
                .expect("create legacy-named db");
            let now = chrono::Utc::now().to_rfc3339();
            let marker_entry = crate::MemoryEntry {
                id: "rename-on-open-marker".to_string(),
                path: "/facts/rename-on-open".to_string(),
                summary: "marker".to_string(),
                text: "rename-on-open marker row".to_string(),
                importance: 0.5,
                timestamp: now.clone(),
                valid_from: now,
                valid_until: None,
                category: "fact".to_string(),
                topic: "test".to_string(),
                keywords: vec![],
                persons: vec![],
                entities: vec![],
                location: String::new(),
                source: "manual".to_string(),
                scope: "project".to_string(),
                archived: false,
                access_count: 0,
                scored_count: 0,
                last_access: None,
                last_use_at: None,
                revision: 1,
                vector: None,
                retention_policy: Some("durable".to_string()),
                domain: None,
                metadata: serde_json::json!({}),
                recall_count: 0,
                query_diversity: 0,
                tier: "raw".to_string(),
            };
            assert_eq!(
                store
                    .insert_if_absent(&marker_entry)
                    .expect("insert durable marker through MemoryStore"),
                crate::db::InsertMemoryResult::Inserted,
                "the legacy fixture must create one durable marker, not coalesce or replace one"
            );
            let marker_before_migration = store
                .get(&marker_entry.id)
                .expect("read durable marker before migration")
                .expect("durable marker must exist before migration");
            assert_eq!(marker_before_migration.summary, marker_entry.summary);
            assert_eq!(
                marker_before_migration.retention_policy.as_deref(),
                Some("durable"),
                "test precondition: marker must be durable before migration"
            );
        }
        assert!(
            legacy_path.is_file(),
            "test precondition: legacy file exists"
        );
        assert!(
            !canonical_path.exists(),
            "test precondition: canonical name not yet created"
        );

        // Even a clean-looking legacy DB cannot be moved by an ordinary
        // MemoryStore open: old binaries are not fenced by this process.
        match crate::MemoryStore::open(canonical_path.to_str().expect("utf8 path")) {
            Err(err) => assert!(err
                .to_string()
                .contains("offline filename conversion required")),
            Ok(_) => panic!("ordinary open must refuse real legacy filename"),
        }
        assert!(!canonical_path.exists());
        assert!(legacy_path.is_file());

        assert_eq!(
            convert_legacy_filename_offline(
                &canonical_path,
                &OfflineFilenameAuthority::operator_attestation(),
            )
            .expect("offline conversion"),
            OfflineFilenameOutcome::Converted
        );
        let migrated_store = crate::MemoryStore::open(canonical_path.to_str().expect("utf8 path"))
            .expect("open after explicit conversion");

        assert!(
            canonical_path.is_file(),
            "the legacy file must now be the file at the canonical path"
        );
        let legacy_meta = std::fs::symlink_metadata(&legacy_path).expect("legacy metadata");
        assert!(
            legacy_meta.file_type().is_symlink(),
            "the old name must be left behind as a compat symlink"
        );

        let marker = migrated_store
            .get("rename-on-open-marker")
            .expect("read marker through canonical store")
            .expect("marker must survive the rename");
        assert_eq!(marker.summary, "marker");
        assert_eq!(marker.retention_policy.as_deref(), Some("durable"));

        // Reopening does not perform a second conversion.
        drop(migrated_store);
        let reopened = crate::MemoryStore::open(canonical_path.to_str().expect("utf8 path"))
            .expect("reopen canonical path");
        let marker_again = reopened
            .get("rename-on-open-marker")
            .expect("read marker through reopened canonical store")
            .expect("marker must still be there on reopen");
        assert_eq!(marker_again.summary, "marker");
        assert_eq!(marker_again.retention_policy.as_deref(), Some("durable"));
    }
}
