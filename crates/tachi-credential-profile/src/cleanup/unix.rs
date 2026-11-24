//! Descriptor-anchored location and removal of managed credential targets.
//!
//! Run-scoped cleanup follows a tree a worker could write to, so it opens the
//! path Tachi itself recorded from `/` without following a single symlink: a
//! parent swapped for a symlink cannot steer the unlink into a host directory.
//! Manual cleanup follows the operator's own path up to the parent directory,
//! but never a symlink that holds the target name itself.

use super::{ManagedCredentialMaterialization, RemovableTarget, Removal, TargetLocation};
use memcore::anchored_fs::{AnchoredDirectory, EntryKind};
use std::ffi::OsStr;
use std::io;
use std::path::{Component, Path, PathBuf};

pub(super) fn locate(
    metadata: &ManagedCredentialMaterialization,
    run_dir: Option<&Path>,
) -> TargetLocation {
    let target = PathBuf::from(&metadata.target);
    if run_dir.is_some() {
        let target = if target.is_absolute() {
            target
        } else {
            // A relative target was recorded against the daemon's own cwd.
            match std::env::current_dir() {
                Ok(cwd) => cwd.join(target),
                Err(err) => {
                    return TargetLocation::Unremovable(format!(
                        "resolve the current directory: {err}"
                    ))
                }
            }
        };
        return locate_recorded(&target);
    }
    locate_for_operator(&target)
}

fn locate_recorded(target: &Path) -> TargetLocation {
    let Some(leaf) = target.file_name() else {
        return TargetLocation::Unremovable(format!("'{}' does not name a file", target.display()));
    };
    match open_recorded_parent(target) {
        Ok(parent) => classify(parent, leaf),
        Err(location) => location,
    }
}

/// Open the recorded target's parent one component at a time from `/`.
fn open_recorded_parent(target: &Path) -> Result<AnchoredDirectory, TargetLocation> {
    let mut directory = AnchoredDirectory::open_absolute(Path::new("/"))
        .map_err(|err| TargetLocation::Unremovable(format!("open '/': {err}")))?;
    let mut opened = PathBuf::from("/");
    for component in target.parent().unwrap_or(Path::new("/")).components() {
        match component {
            Component::RootDir | Component::CurDir => {}
            Component::Normal(name) => {
                opened.push(name);
                directory = open_component(&directory, name, &opened)?;
            }
            _ => {
                return Err(TargetLocation::Unremovable(format!(
                    "'{}' is not an ordinary absolute path",
                    target.display()
                )))
            }
        }
    }
    Ok(directory)
}

/// Open one recorded path component, never through a symlink. A component
/// that is gone means the target is gone; anything else is refused.
fn open_component(
    parent: &AnchoredDirectory,
    name: &OsStr,
    opened: &Path,
) -> Result<AnchoredDirectory, TargetLocation> {
    let error = match parent.open_directory(name) {
        Ok(directory) => return Ok(directory),
        Err(error) => error,
    };
    // `O_DIRECTORY | O_NOFOLLOW` reports a symlink as ENOTDIR on macOS and
    // ELOOP on Linux, so the entry itself is asked what it is.
    Err(match parent.entry_kind(name) {
        Ok(EntryKind::Symlink) => {
            TargetLocation::Unremovable(format!("'{}' is a symlink", opened.display()))
        }
        Ok(EntryKind::Missing) => TargetLocation::Missing,
        Ok(_) => TargetLocation::Unremovable(format!("'{}' is not a directory", opened.display())),
        Err(_) if error.kind() == io::ErrorKind::NotFound => TargetLocation::Missing,
        Err(_) => TargetLocation::Unremovable(format!("open '{}': {error}", opened.display())),
    })
}

/// Manual cleanup resolves the operator's own parent path, including its
/// symlinked ancestors, and refuses only what holds the target name itself.
fn locate_for_operator(target: &Path) -> TargetLocation {
    let Some(leaf) = target.file_name() else {
        return TargetLocation::Unremovable(format!("'{}' does not name a file", target.display()));
    };
    let parent = target
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    let canonical = match std::fs::canonicalize(parent) {
        Ok(canonical) => canonical,
        Err(err) if err.kind() == io::ErrorKind::NotFound => return TargetLocation::Missing,
        Err(err) => {
            return TargetLocation::Unremovable(format!(
                "resolve its parent directory '{}': {err}",
                parent.display()
            ))
        }
    };
    match AnchoredDirectory::open_absolute(&canonical) {
        Ok(directory) => classify(directory, leaf),
        Err(err) => TargetLocation::Unremovable(format!(
            "open its parent directory without symlinks: {err}"
        )),
    }
}

/// Classify the leaf by its own directory entry, never through it.
fn classify(directory: AnchoredDirectory, leaf: &OsStr) -> TargetLocation {
    match directory.entry_kind(leaf) {
        Ok(EntryKind::Missing) => TargetLocation::Missing,
        Ok(EntryKind::RegularFile) => TargetLocation::Removable(RemovableTarget {
            parent: directory,
            leaf: leaf.to_os_string(),
        }),
        Ok(EntryKind::Symlink) => TargetLocation::Unremovable("it is a symlink".into()),
        Ok(EntryKind::Directory) => TargetLocation::Unremovable("it is a directory".into()),
        Ok(EntryKind::Other) => TargetLocation::Unremovable("it is not a regular file".into()),
        Err(err) => TargetLocation::Unremovable(format!("inspect it: {err}")),
    }
}

pub(super) fn remove<A: Fn(&[u8]) -> bool>(
    target: &RemovableTarget,
    require_managed_content: Option<&A>,
) -> io::Result<Removal> {
    let Some(accept) = require_managed_content else {
        return target.parent.remove_entry(&target.leaf).map(|removed| {
            if removed {
                Removal::Removed
            } else {
                Removal::Vanished
            }
        });
    };
    Ok(match target.parent.remove_file_if(&target.leaf, accept)? {
        Some(true) => Removal::Removed,
        Some(false) => Removal::ForeignContent,
        None => Removal::Vanished,
    })
}
