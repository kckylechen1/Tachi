//! Path-based location and removal for platforms without descriptor anchoring.
//! The entry holding a target name is never followed, and only a regular file
//! is removed.

use super::{
    LiveRunRoot, ManagedCredentialMaterialization, RemovableTarget, Removal, TargetInspection,
    TargetLocation,
};
use memcore::anchored_fs::DirectoryIdentity;
use std::ffi::OsStr;
use std::fs::{self, File};
use std::io::{self, Read};
use std::path::{Path, PathBuf};

pub(super) fn locate_bound(
    _root: &LiveRunRoot,
    _identity: DirectoryIdentity,
    _relative: &Path,
    _capture: Option<&OsStr>,
) -> Result<TargetLocation, String> {
    Err("bound target requires Unix directory identity verification".into())
}

pub(super) fn locate_bound_for_operator(
    _metadata: &ManagedCredentialMaterialization,
    _identity: DirectoryIdentity,
    _relative: &Path,
    _mark_only: bool,
    _capture: Option<&OsStr>,
) -> Result<TargetLocation, String> {
    Err("bound target requires Unix directory identity verification".into())
}

pub(super) fn locate_legacy(_root: &LiveRunRoot, _target: &Path) -> Result<TargetLocation, String> {
    Err("legacy target requires a run_dir opened without symlinks".into())
}

pub(super) fn locate_for_operator(target: &Path) -> TargetLocation {
    let Some(leaf) = target.file_name() else {
        return TargetLocation::Unremovable(format!("'{}' does not name a file", target.display()));
    };
    let parent = target
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or(Path::new("."))
        .to_path_buf();
    match fs::symlink_metadata(target) {
        Err(err) if err.kind() == io::ErrorKind::NotFound => TargetLocation::Missing,
        Err(err) => TargetLocation::Unremovable(format!("inspect it: {err}")),
        Ok(entry) if entry.file_type().is_symlink() => {
            TargetLocation::Unremovable("it is a symlink".into())
        }
        Ok(entry) if entry.is_dir() => TargetLocation::Unremovable("it is a directory".into()),
        Ok(entry) if !entry.is_file() => {
            TargetLocation::Unremovable("it is not a regular file".into())
        }
        Ok(_) => TargetLocation::Removable(RemovableTarget {
            parent,
            leaf: leaf.to_os_string(),
        }),
    }
}

pub(super) fn inspect(
    target: &Path,
    _run_dir: Option<&Path>,
    max_bytes: usize,
) -> TargetInspection {
    match fs::symlink_metadata(target) {
        Err(err) if err.kind() == io::ErrorKind::NotFound => return TargetInspection::Missing,
        Err(err) => {
            return TargetInspection::Unreadable {
                reason: err.to_string(),
            }
        }
        Ok(entry) if entry.file_type().is_symlink() => {
            return TargetInspection::UntrustedEntry {
                reason: "it is a symlink".into(),
            }
        }
        Ok(entry) if entry.is_dir() => {
            return TargetInspection::UntrustedEntry {
                reason: "it is a directory".into(),
            }
        }
        Ok(entry) if !entry.is_file() => {
            return TargetInspection::UntrustedEntry {
                reason: "it is not a regular file".into(),
            }
        }
        Ok(_) => {}
    }
    let mut file = match File::open(target) {
        Ok(file) => file,
        Err(err) if err.kind() == io::ErrorKind::NotFound => return TargetInspection::Missing,
        Err(err) => {
            return TargetInspection::Unreadable {
                reason: err.to_string(),
            }
        }
    };
    match file.metadata() {
        Ok(meta) if !meta.is_file() => {
            return TargetInspection::UntrustedEntry {
                reason: "it is not a regular file".into(),
            }
        }
        Err(err) => {
            return TargetInspection::Unreadable {
                reason: err.to_string(),
            }
        }
        Ok(_) => {}
    }
    let limit = u64::try_from(max_bytes)
        .unwrap_or(u64::MAX)
        .saturating_add(1);
    let mut bytes = Vec::new();
    if let Err(err) = file.by_ref().take(limit).read_to_end(&mut bytes) {
        return TargetInspection::Unreadable {
            reason: err.to_string(),
        };
    }
    if bytes.len() > max_bytes {
        return TargetInspection::Unreadable {
            reason: format!("file exceeds maximum size of {max_bytes} bytes"),
        };
    }
    TargetInspection::RegularFile { bytes }
}

pub(super) fn restore(
    _target: &RemovableTarget,
    _backup: &Path,
    _destination: &Path,
    _identity: (u64, u64),
    _capture: Option<&OsStr>,
) -> io::Result<()> {
    Err(io::Error::other("credential backup recovery requires Unix"))
}

pub(super) fn confirm_original(
    _target: &RemovableTarget,
    _inode: (u64, u64),
    _capture: Option<&OsStr>,
) -> io::Result<bool> {
    Ok(false)
}

pub(super) fn remove<A: Fn(&[u8]) -> bool>(
    target: &RemovableTarget,
    identity: Option<(u64, u64)>,
    _capture: Option<&OsStr>,
    require_managed_content: Option<&A>,
) -> io::Result<Removal> {
    if identity.is_some() {
        return Err(io::Error::other(
            "credential inode verification requires Unix",
        ));
    }
    let path = target.parent.join(&target.leaf);
    let Some(accept) = require_managed_content else {
        return match fs::remove_file(&path) {
            Ok(()) => Ok(Removal::Removed),
            Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(Removal::Vanished),
            Err(err) => Err(err),
        };
    };
    let mut file = File::open(&path)?;
    if !file.metadata()?.is_file() {
        return Ok(Removal::ForeignContent);
    }
    let mut captured = Vec::new();
    file.read_to_end(&mut captured)?;
    if !accept(&captured) {
        return Ok(Removal::ForeignContent);
    }
    fs::remove_file(&path).map(|()| Removal::Removed)
}
