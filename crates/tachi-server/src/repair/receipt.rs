//! Closed filesystem protocol for prepared-to-committed repair receipts.
//!
//! Receipt schemas and database semantics stay in their owning operations.
//! This private module owns only durable, no-overwrite publication and
//! inode-safe finalization of opaque receipt bytes.

use std::fs::{self, File, OpenOptions};
use std::io::Write;
#[cfg(unix)]
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};

#[cfg(test)]
use std::cell::Cell;

#[cfg(test)]
std::thread_local! {
    static FAULT_DURING_PREPARED_STAGE_WRITE: Cell<bool> = const { Cell::new(false) };
    static FAULT_BEFORE_PREPARED_PARENT_SYNC: Cell<bool> = const { Cell::new(false) };
    static FAULT_BEFORE_RECOVERY_LINK: Cell<bool> = const { Cell::new(false) };
}

#[derive(Debug, Clone, Copy)]
pub(super) enum ReceiptKind {
    ExactDedupe,
    MemoryMaintenance,
}

impl ReceiptKind {
    fn stage_label(self) -> &'static str {
        match self {
            Self::ExactDedupe => "exact-dedupe",
            Self::MemoryMaintenance => "memory-maintenance",
        }
    }
}

#[cfg(test)]
pub(super) fn inject_fault_during_prepared_stage_write_for_test(enabled: bool) {
    FAULT_DURING_PREPARED_STAGE_WRITE.with(|fault| fault.set(enabled));
}

#[cfg(test)]
pub(super) fn inject_fault_before_prepared_parent_sync_for_test(enabled: bool) {
    FAULT_BEFORE_PREPARED_PARENT_SYNC.with(|fault| fault.set(enabled));
}

#[cfg(test)]
pub(super) fn inject_fault_before_recovery_link_for_test(enabled: bool) {
    FAULT_BEFORE_RECOVERY_LINK.with(|fault| fault.set(enabled));
}

fn receipt_parent(path: &Path) -> &Path {
    path.parent()
        .filter(|path| !path.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."))
}

fn sync_receipt_parent(receipt_out: &Path) -> Result<(), std::io::Error> {
    File::open(receipt_parent(receipt_out))?.sync_all()
}

fn sync_prepared_receipt_parent(receipt_out: &Path) -> Result<(), std::io::Error> {
    #[cfg(test)]
    if FAULT_BEFORE_PREPARED_PARENT_SYNC.with(|fault| fault.replace(false)) {
        return Err(std::io::Error::other(
            "injected prepared-receipt parent sync failure",
        ));
    }
    sync_receipt_parent(receipt_out)
}

fn write_staged_receipt(
    receipt_out: &Path,
    kind: ReceiptKind,
    phase: &str,
    bytes: &[u8],
) -> Result<(PathBuf, File), std::io::Error> {
    let parent = receipt_parent(receipt_out);
    let name = receipt_out
        .file_name()
        .map(|name| name.to_string_lossy())
        .unwrap_or_else(|| "receipt".into());
    for attempt in 0..32 {
        let candidate = parent.join(format!(
            ".{name}.{}-{phase}-{}-{attempt}",
            kind.stage_label(),
            uuid::Uuid::new_v4()
        ));
        let mut file = match OpenOptions::new()
            .read(true)
            .write(true)
            .create_new(true)
            .open(&candidate)
        {
            Ok(file) => file,
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(error),
        };
        let write_result = (|| {
            #[cfg(test)]
            if phase == "prepared"
                && FAULT_DURING_PREPARED_STAGE_WRITE.with(|fault| fault.replace(false))
            {
                file.write_all(&bytes[..bytes.len() / 2])?;
                return Err(std::io::Error::other(
                    "injected partial prepared-receipt write failure",
                ));
            }
            file.write_all(bytes)?;
            file.write_all(b"\n")?;
            file.sync_all()
        })();
        if let Err(error) = write_result {
            drop(file);
            let _ = fs::remove_file(&candidate);
            return Err(error);
        }
        return Ok((candidate, file));
    }
    Err(std::io::Error::new(
        std::io::ErrorKind::AlreadyExists,
        "could not allocate receipt staging path",
    ))
}

#[derive(Debug)]
pub(super) enum PreparedArtifactError {
    Stage(std::io::Error),
    AlreadyExists,
    Publish(std::io::Error),
    RecoveryAlreadyExists,
    RecoveryPublish(std::io::Error),
    ParentSync(std::io::Error),
}

/// Publishes opaque prepared bytes without overwriting an existing public name.
/// The returned open file proves the prepared inode during finalization.
pub(super) fn persist_prepared_artifact_bytes(
    output: &Path,
    kind: ReceiptKind,
    bytes: &[u8],
) -> Result<File, PreparedArtifactError> {
    let (staged, file) = write_staged_receipt(output, kind, "prepared", bytes)
        .map_err(PreparedArtifactError::Stage)?;
    if let Err(error) = fs::hard_link(&staged, output) {
        let _ = fs::remove_file(&staged);
        if error.kind() == std::io::ErrorKind::AlreadyExists {
            return Err(PreparedArtifactError::AlreadyExists);
        }
        return Err(PreparedArtifactError::Publish(error));
    }
    if let Err(error) = sync_prepared_receipt_parent(output) {
        // Never unlink the public name after publication: a concurrent actor
        // could replace it between the check and unlink. The caller decides
        // whether a surrounding DB transaction must roll back.
        let _ = fs::remove_file(&staged);
        return Err(PreparedArtifactError::ParentSync(error));
    }
    if fs::remove_file(&staged).is_ok() {
        let _ = sync_receipt_parent(output);
    }
    Ok(file)
}

/// Publishes one prepared inode under both a public name and a deterministic
/// private recovery name. The file and both directory entries are durable
/// before this returns, so a later public-name replacement cannot erase the
/// commit-boundary evidence.
pub(super) fn persist_prepared_artifact_with_recovery_bytes(
    output: &Path,
    recovery: &Path,
    kind: ReceiptKind,
    bytes: &[u8],
) -> Result<File, PreparedArtifactError> {
    if receipt_parent(output) != receipt_parent(recovery) {
        return Err(PreparedArtifactError::RecoveryPublish(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "prepared receipt recovery path must share the public parent",
        )));
    }
    let (staged, file) = write_staged_receipt(output, kind, "prepared", bytes)
        .map_err(PreparedArtifactError::Stage)?;
    if let Err(error) = fs::hard_link(&staged, output) {
        let _ = fs::remove_file(&staged);
        if error.kind() == std::io::ErrorKind::AlreadyExists {
            return Err(PreparedArtifactError::AlreadyExists);
        }
        return Err(PreparedArtifactError::Publish(error));
    }
    #[cfg(test)]
    let recovery_link = if FAULT_BEFORE_RECOVERY_LINK.with(|fault| fault.replace(false)) {
        Err(std::io::Error::other(
            "injected recovery-link publication failure",
        ))
    } else {
        fs::hard_link(&staged, recovery)
    };
    #[cfg(not(test))]
    let recovery_link = fs::hard_link(&staged, recovery);
    if let Err(error) = recovery_link {
        // Never unlink the caller-visible public name after publication: a
        // concurrent actor could replace it between a check and unlink. The
        // surrounding DB callback returns an error and rolls back.
        let _ = fs::remove_file(&staged);
        if error.kind() == std::io::ErrorKind::AlreadyExists {
            return Err(PreparedArtifactError::RecoveryAlreadyExists);
        }
        return Err(PreparedArtifactError::RecoveryPublish(error));
    }
    if let Err(error) = sync_prepared_receipt_parent(output) {
        let _ = fs::remove_file(&staged);
        return Err(PreparedArtifactError::ParentSync(error));
    }
    if fs::remove_file(&staged).is_ok() {
        let _ = sync_receipt_parent(output);
    }
    Ok(file)
}

#[cfg(any(target_os = "macos", target_os = "ios", target_os = "linux"))]
fn atomic_exchange_paths(left: &Path, right: &Path) -> Result<(), std::io::Error> {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;

    let left = CString::new(left.as_os_str().as_bytes()).map_err(|_| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "receipt path contains an interior NUL byte",
        )
    })?;
    let right = CString::new(right.as_os_str().as_bytes()).map_err(|_| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "receipt path contains an interior NUL byte",
        )
    })?;
    // SAFETY: both C strings are live and NUL-terminated for the duration of
    // the syscall. The platform primitive atomically exchanges two directory
    // entries and does not retain either pointer.
    let rc = unsafe {
        #[cfg(any(target_os = "macos", target_os = "ios"))]
        {
            libc::renamex_np(left.as_ptr(), right.as_ptr(), libc::RENAME_SWAP)
        }
        #[cfg(target_os = "linux")]
        {
            libc::renameat2(
                libc::AT_FDCWD,
                left.as_ptr(),
                libc::AT_FDCWD,
                right.as_ptr(),
                libc::RENAME_EXCHANGE,
            )
        }
    };
    if rc == 0 {
        Ok(())
    } else {
        Err(std::io::Error::last_os_error())
    }
}

#[cfg(not(any(target_os = "macos", target_os = "ios", target_os = "linux")))]
fn atomic_exchange_paths(_left: &Path, _right: &Path) -> Result<(), std::io::Error> {
    Err(std::io::Error::new(
        std::io::ErrorKind::Unsupported,
        "atomic receipt exchange is unavailable on this platform",
    ))
}

#[cfg(unix)]
pub(super) fn path_names_open_file(path: &Path, file: &File) -> Result<bool, std::io::Error> {
    let path_metadata = fs::symlink_metadata(path)?;
    let file_metadata = file.metadata()?;
    Ok(path_metadata.file_type().is_file()
        && path_metadata.dev() == file_metadata.dev()
        && path_metadata.ino() == file_metadata.ino())
}

#[cfg(not(unix))]
pub(super) fn path_names_open_file(_path: &Path, _file: &File) -> Result<bool, std::io::Error> {
    Ok(false)
}

pub(super) fn sync_and_validate_prepared_links(
    receipt_out: &Path,
    recovery: &Path,
    prepared_file: &File,
) -> Result<bool, std::io::Error> {
    if receipt_parent(receipt_out) != receipt_parent(recovery)
        || !path_names_open_file(receipt_out, prepared_file)?
        || !path_names_open_file(recovery, prepared_file)?
    {
        return Ok(false);
    }
    prepared_file.sync_all()?;
    sync_receipt_parent(receipt_out)?;
    Ok(path_names_open_file(receipt_out, prepared_file)?
        && path_names_open_file(recovery, prepared_file)?)
}

/// Atomically replaces a public prepared artifact with committed bytes while
/// preserving and proving every displaced inode. No object is unlinked after
/// exchange, so concurrent pathname replacement cannot destroy foreign data.
pub(super) fn finalize_prepared_receipt(
    receipt_out: &Path,
    kind: ReceiptKind,
    prepared_file: &File,
    committed_bytes: &[u8],
) -> Result<PathBuf, Box<dyn std::error::Error>> {
    let (staged, committed_file) =
        write_staged_receipt(receipt_out, kind, "prepared-backup", committed_bytes)?;
    if let Err(error) = atomic_exchange_paths(&staged, receipt_out) {
        let _ = fs::remove_file(&staged);
        return Err(format!("atomic committed-receipt exchange failed: {error}").into());
    }
    if !path_names_open_file(&staged, prepared_file)? {
        // The output was replaced before the exchange. Exchange back so the
        // unrelated object returns to its original path. Both sides are kept
        // if further interference prevents proving that restoration.
        let displaced = OpenOptions::new().read(true).open(&staged)?;
        atomic_exchange_paths(&staged, receipt_out)?;
        let restored = path_names_open_file(receipt_out, &displaced)?
            && path_names_open_file(&staged, &committed_file)?;
        if restored {
            return Err(format!(
                "public receipt path was replaced before finalization; replacement was restored without overwrite and the committed staging artifact was retained at {}",
                staged.display()
            )
            .into());
        }
        return Err(format!(
            "public receipt path changed during atomic finalization; no object was deleted and both exchange paths were retained: {} and {}",
            receipt_out.display(),
            staged.display()
        )
        .into());
    }
    // `staged` now names our own prepared inode, proven by the open handle.
    // Keep it as commit-boundary audit evidence: deleting by pathname would
    // reintroduce the same compare-then-unlink race this exchange avoids.
    if let Err(error) = sync_receipt_parent(receipt_out) {
        return Err(format!(
            "committed receipt exchange completed but parent fsync failed: {error}; the prepared inode remains retained at {} for reconciliation",
            staged.display()
        )
        .into());
    }
    Ok(staged)
}

/// Finalizes committed bytes from a durable recovery inode. A displaced
/// public object is retained byte-for-byte at the unique staging path. The
/// deterministic recovery link is removed only after the committed public
/// entry and any displaced entry have been fsynced.
pub(super) fn finalize_prepared_receipt_from_recovery(
    receipt_out: &Path,
    recovery: &Path,
    kind: ReceiptKind,
    prepared_file: &File,
    committed_bytes: &[u8],
) -> Result<Option<PathBuf>, Box<dyn std::error::Error>> {
    if !path_names_open_file(recovery, prepared_file)? {
        return Err(format!(
            "prepared recovery path {} no longer names the opened prepared inode",
            recovery.display()
        )
        .into());
    }
    let (staged, committed_file) =
        write_staged_receipt(receipt_out, kind, "committed-publication", committed_bytes)?;
    let displaced = if fs::symlink_metadata(receipt_out).is_ok() {
        atomic_exchange_paths(&staged, receipt_out)?;
        Some(staged.clone())
    } else {
        match fs::hard_link(&staged, receipt_out) {
            Ok(()) => {
                fs::remove_file(&staged)?;
                None
            }
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                atomic_exchange_paths(&staged, receipt_out)?;
                Some(staged.clone())
            }
            Err(error) => return Err(error.into()),
        }
    };
    if !path_names_open_file(receipt_out, &committed_file)? {
        return Err(format!(
            "committed receipt public path {} changed before durability; recovery evidence retained at {}",
            receipt_out.display(),
            recovery.display()
        )
        .into());
    }
    sync_receipt_parent(receipt_out)?;
    if !path_names_open_file(receipt_out, &committed_file)? {
        return Err(format!(
            "committed receipt public path {} changed after durability; recovery evidence retained at {}",
            receipt_out.display(),
            recovery.display()
        )
        .into());
    }
    if !path_names_open_file(recovery, prepared_file)? {
        return Err(format!(
            "prepared recovery path {} changed after committed publication; no recovery object was removed",
            recovery.display()
        )
        .into());
    }
    fs::remove_file(recovery)?;
    sync_receipt_parent(receipt_out)?;
    Ok(displaced)
}
