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
    static REPLACE_STAGED_BEFORE_CLEANUP: Cell<Option<CleanupEdge>> = const { Cell::new(None) };
}

#[cfg(test)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CleanupEdge {
    StageWriteFailure,
    PublishFailure,
    ParentSyncFailure,
    Success,
    FinalizeExchangeFailure,
}

#[cfg(test)]
fn inject_replace_staged_before_cleanup_for_test(edge: CleanupEdge) {
    REPLACE_STAGED_BEFORE_CLEANUP.with(|slot| slot.set(Some(edge)));
}

#[cfg(test)]
fn replace_staged_before_cleanup(path: &Path, edge: CleanupEdge) -> Result<(), std::io::Error> {
    if REPLACE_STAGED_BEFORE_CLEANUP.with(|slot| {
        if slot.get() == Some(edge) {
            slot.set(None);
            true
        } else {
            false
        }
    }) {
        let retained = path.with_extension(format!("retained-{edge:?}"));
        fs::rename(path, retained)?;
        fs::write(path, format!("FOREIGN-STAGED-{edge:?}"))?;
    }
    Ok(())
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
            #[cfg(test)]
            replace_staged_before_cleanup(&candidate, CleanupEdge::StageWriteFailure)?;
            return Err(std::io::Error::new(
                error.kind(),
                format!(
                    "{error}; partial staging artifact retained at {}",
                    candidate.display()
                ),
            ));
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
    AlreadyExists(PathBuf),
    Publish(std::io::Error),
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
        #[cfg(test)]
        replace_staged_before_cleanup(&staged, CleanupEdge::PublishFailure)
            .map_err(PreparedArtifactError::Publish)?;
        if error.kind() == std::io::ErrorKind::AlreadyExists {
            return Err(PreparedArtifactError::AlreadyExists(staged));
        }
        return Err(PreparedArtifactError::Publish(std::io::Error::new(
            error.kind(),
            format!("{error}; staging artifact retained at {}", staged.display()),
        )));
    }
    if let Err(error) = sync_prepared_receipt_parent(output) {
        // Never unlink the public name after publication: a concurrent actor
        // could replace it between the check and unlink. The caller decides
        // whether a surrounding DB transaction must roll back.
        #[cfg(test)]
        replace_staged_before_cleanup(&staged, CleanupEdge::ParentSyncFailure)
            .map_err(PreparedArtifactError::ParentSync)?;
        return Err(PreparedArtifactError::ParentSync(std::io::Error::new(
            error.kind(),
            format!("{error}; staging artifact retained at {}", staged.display()),
        )));
    }
    #[cfg(test)]
    replace_staged_before_cleanup(&staged, CleanupEdge::Success)
        .map_err(PreparedArtifactError::Publish)?;
    if !path_names_open_file(&staged, &file).map_err(PreparedArtifactError::Publish)? {
        return Err(PreparedArtifactError::Publish(std::io::Error::other(
            format!(
                "prepared staging path changed before return; no pathname was deleted: {}",
                staged.display()
            ),
        )));
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

pub(super) fn sync_and_validate_prepared_artifact(
    receipt_out: &Path,
    prepared_file: &File,
) -> Result<bool, std::io::Error> {
    if !path_names_open_file(receipt_out, prepared_file)? {
        return Ok(false);
    }
    prepared_file.sync_all()?;
    sync_receipt_parent(receipt_out)?;
    path_names_open_file(receipt_out, prepared_file)
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
        #[cfg(test)]
        replace_staged_before_cleanup(&staged, CleanupEdge::FinalizeExchangeFailure)?;
        return Err(format!(
            "atomic committed-receipt exchange failed: {error}; committed staging artifact retained at {}",
            staged.display()
        )
        .into());
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

#[derive(Debug)]
pub(super) enum CommittedProjection {
    ReplacedExpected,
    ReplacedForeign { retained: PathBuf },
    PublishedMissing,
}

/// Projects committed receipt bytes from database authority. Every displaced
/// pathname object is retained at the randomized staging name; this function
/// never removes a path after comparing its inode.
pub(super) fn publish_committed_projection(
    receipt_out: &Path,
    kind: ReceiptKind,
    expected_prepared_file: Option<&File>,
    committed_bytes: &[u8],
) -> Result<CommittedProjection, Box<dyn std::error::Error>> {
    let (staged, committed_file) =
        write_staged_receipt(receipt_out, kind, "committed-publication", committed_bytes)?;
    let projection = match fs::hard_link(&staged, receipt_out) {
        Ok(()) => CommittedProjection::PublishedMissing,
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
            atomic_exchange_paths(&staged, receipt_out)?;
            if expected_prepared_file
                .map(|file| path_names_open_file(&staged, file))
                .transpose()?
                .unwrap_or(false)
            {
                CommittedProjection::ReplacedExpected
            } else {
                CommittedProjection::ReplacedForeign {
                    retained: staged.clone(),
                }
            }
        }
        Err(error) => return Err(error.into()),
    };
    if !path_names_open_file(receipt_out, &committed_file)? {
        return Err(format!(
            "committed receipt public path {} changed before durability; projection evidence retained at {}",
            receipt_out.display(),
            staged.display()
        )
        .into());
    }
    sync_receipt_parent(receipt_out)?;
    if !path_names_open_file(receipt_out, &committed_file)? {
        return Err(format!(
            "committed receipt public path {} changed after durability; projection evidence retained at {}",
            receipt_out.display(),
            staged.display()
        )
        .into());
    }
    Ok(projection)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn foreign_survives(dir: &Path, edge: CleanupEdge) -> bool {
        let expected = format!("FOREIGN-STAGED-{edge:?}").into_bytes();
        dir.read_dir()
            .unwrap()
            .any(|entry| std::fs::read(entry.unwrap().path()).is_ok_and(|bytes| bytes == expected))
    }

    #[test]
    fn cleanup_edges_never_delete_a_foreign_staged_replacement() {
        let stage_failure = tempfile::tempdir().unwrap();
        let output = stage_failure.path().join("receipt.json");
        inject_fault_during_prepared_stage_write_for_test(true);
        inject_replace_staged_before_cleanup_for_test(CleanupEdge::StageWriteFailure);
        persist_prepared_artifact_bytes(&output, ReceiptKind::MemoryMaintenance, b"prepared")
            .expect_err("partial stage write must fail");
        assert!(foreign_survives(
            stage_failure.path(),
            CleanupEdge::StageWriteFailure
        ));

        let publish_failure = tempfile::tempdir().unwrap();
        let output = publish_failure.path().join("receipt.json");
        fs::write(&output, b"existing").unwrap();
        inject_replace_staged_before_cleanup_for_test(CleanupEdge::PublishFailure);
        persist_prepared_artifact_bytes(&output, ReceiptKind::MemoryMaintenance, b"prepared")
            .expect_err("no-overwrite publication must fail");
        assert!(foreign_survives(
            publish_failure.path(),
            CleanupEdge::PublishFailure
        ));

        let parent_failure = tempfile::tempdir().unwrap();
        let output = parent_failure.path().join("receipt.json");
        inject_fault_before_prepared_parent_sync_for_test(true);
        inject_replace_staged_before_cleanup_for_test(CleanupEdge::ParentSyncFailure);
        persist_prepared_artifact_bytes(&output, ReceiptKind::MemoryMaintenance, b"prepared")
            .expect_err("parent sync failure must be non-success");
        assert!(foreign_survives(
            parent_failure.path(),
            CleanupEdge::ParentSyncFailure
        ));

        let success_cleanup = tempfile::tempdir().unwrap();
        let output = success_cleanup.path().join("receipt.json");
        inject_replace_staged_before_cleanup_for_test(CleanupEdge::Success);
        persist_prepared_artifact_bytes(&output, ReceiptKind::MemoryMaintenance, b"prepared")
            .expect_err("staged replacement before success cleanup must be non-success");
        assert!(foreign_survives(
            success_cleanup.path(),
            CleanupEdge::Success
        ));

        let finalize_failure = tempfile::tempdir().unwrap();
        let output = finalize_failure.path().join("missing-public.json");
        let prepared_path = finalize_failure.path().join("prepared");
        fs::write(&prepared_path, b"prepared").unwrap();
        let prepared = File::open(prepared_path).unwrap();
        inject_replace_staged_before_cleanup_for_test(CleanupEdge::FinalizeExchangeFailure);
        finalize_prepared_receipt(&output, ReceiptKind::ExactDedupe, &prepared, b"committed")
            .expect_err("exchange against missing public path must fail");
        assert!(foreign_survives(
            finalize_failure.path(),
            CleanupEdge::FinalizeExchangeFailure
        ));
    }
}
