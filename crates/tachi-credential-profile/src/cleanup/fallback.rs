//! Path-based location and removal for platforms without descriptor anchoring.
//! The entry holding a target name is never followed, and only a regular file
//! is removed.

use super::{
    ManagedCredentialMaterialization, RemovableTarget, Removal, TargetInspection, TargetLocation,
};
use std::fs::{self, File};
use std::io::{self, Read};
use std::path::{Path, PathBuf};

pub(super) fn locate(
    metadata: &ManagedCredentialMaterialization,
    _run_dir: Option<&Path>,
) -> TargetLocation {
    let target = PathBuf::from(&metadata.target);
    let Some(leaf) = target.file_name() else {
        return TargetLocation::Unremovable(format!("'{}' does not name a file", target.display()));
    };
    let parent = target
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or(Path::new("."))
        .to_path_buf();
    match fs::symlink_metadata(&target) {
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

pub(super) fn remove<A: Fn(&[u8]) -> bool>(
    target: &RemovableTarget,
    require_managed_content: Option<&A>,
) -> io::Result<Removal> {
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
