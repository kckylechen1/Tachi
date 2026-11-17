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

/// Durable admission fence for a pre-layout copy of a database and its WAL.
pub fn legacy_copy_marker_path(db_path: &Path) -> PathBuf {
    sqlite_sidecar_path(db_path, ".legacy-copy-incomplete")
}

/// Maximum alias hops inspected before refusing an unproven target.
const MAX_LEGACY_COPY_ALIAS_HOPS: usize = 32;

/// A leftover main file does not prove that its legacy WAL was copied. Check
/// the requested path and every alias hop it names so a Plan-C alias cannot
/// admit an incomplete copy as a new or already initialized store.
///
/// The walk reads links by name (`read_link`) and never `canonicalize`s them:
/// symlink-leaf classification belongs to the caller's symlink authority (the
/// server refuses a symlinked canonical leaf via
/// `canonical_db_leaf_exists_without_symlink`, memcore via store-open path
/// admission), and a leaf or ancestor link loop must not surface here as a
/// legacy-copy stat failure that pre-empts that authority. A path that
/// resolves through no link can be neither opened nor created by SQLite, so
/// there is nothing for this fence to admit there; a genuine I/O failure
/// (permissions, transient I/O) is still never treated as "no marker".
pub fn refuse_incomplete_legacy_copy(db_path: &Path) -> Result<(), MemoryError> {
    let mut path = db_path.to_path_buf();
    let mut visited = std::collections::HashSet::new();
    for _ in 0..MAX_LEGACY_COPY_ALIAS_HOPS {
        if !visited.insert(path.clone()) {
            // A confirmed cycle cannot resolve to a SQLite file. Leave its
            // diagnostic to the caller's existing symlink authority.
            return Ok(());
        }
        let marker = legacy_copy_marker_path(&path);
        match std::fs::symlink_metadata(&marker) {
            Ok(_) => {
                return Err(MemoryError::InvalidArg(format!(
                    "incomplete legacy database copy at {}; admission marker {} remains. Stop all writers and inspect the target main/WAL/SHM offline; preserve the legacy source, quarantine the incomplete target set, then remove the marker and retry startup",
                    path.display(), marker.display()
                )));
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) if unresolvable_through_links(&error) => return Ok(()),
            Err(error) => return Err(stat_error("legacy-copy marker", &marker, error)),
        }
        // A failed cleanup may have removed the main file while leaving its
        // marker, and a dangling Plan-C link must still inspect its target.
        match std::fs::symlink_metadata(&path) {
            Ok(metadata) if !metadata.file_type().is_symlink() => return Ok(()),
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(error) if unresolvable_through_links(&error) => return Ok(()),
            Err(error) => return Err(stat_error("legacy-copy target", &path, error)),
        }
        // Do not call read_link on a regular Windows file: its non-reparse
        // error is not classified as InvalidInput by every Rust version.
        let target = match std::fs::read_link(&path) {
            Ok(target) => target,
            Err(error)
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::NotFound | std::io::ErrorKind::InvalidInput
                ) || unresolvable_through_links(&error) =>
            {
                // Not a link (regular file, directory), absent, or
                // unresolvable: no further alias exists to inspect.
                return Ok(());
            }
            Err(error) => return Err(stat_error("legacy-copy target", &path, error)),
        };
        let target = if target.is_absolute() {
            target
        } else {
            path.parent().unwrap_or_else(|| Path::new(".")).join(target)
        };
        if target == path {
            // Self-referential alias: the hop above already saw this name.
            return Ok(());
        }
        path = target;
    }
    Err(MemoryError::InvalidArg(format!(
        "legacy-copy alias chain exceeds {MAX_LEGACY_COPY_ALIAS_HOPS} hops at {}; refusing an unverified copy target",
        path.display()
    )))
}

/// `true` when `error` means the path could not be resolved *through* links at
/// all: a link loop (`ELOOP`) or a non-directory component on the way
/// (`ENOTDIR`). Neither state can hold a store, so symlink classification
/// stays with the caller's symlink authority instead of being answered here.
fn unresolvable_through_links(error: &std::io::Error) -> bool {
    #[cfg(unix)]
    {
        matches!(
            error.raw_os_error(),
            Some(libc::ELOOP) | Some(libc::ENOTDIR)
        )
    }
    #[cfg(not(unix))]
    {
        let _ = error;
        false
    }
}

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
    refuse_incomplete_legacy_copy(canonical)?;
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
        .truncate(false)
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
    refuse_incomplete_legacy_copy(db_path)?;
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
mod tests;
