//! Descriptor-anchored location and removal of managed credential targets.
//!
//! Run-scoped cleanup follows a tree a worker could write to, so it opens the
//! path Tachi itself recorded from `/` without following a single symlink: a
//! parent swapped for a symlink cannot steer the unlink into a host directory.
//! Manual cleanup follows the operator's own path up to the parent directory,
//! but never a symlink that holds the target name itself.

use super::{
    LiveRunRoot, ManagedCredentialMaterialization, RemovableTarget, Removal, TargetInspection,
    TargetLocation,
};
use memcore::anchored_fs::{AnchoredDirectory, DirectoryIdentity, EntryKind};
use std::ffi::OsStr;
use std::io;
use std::os::unix::fs::PermissionsExt;
use std::path::{Component, Path, PathBuf};

pub(super) fn locate_bound(
    root: &LiveRunRoot,
    identity: DirectoryIdentity,
    relative: &Path,
    capture: Option<&OsStr>,
) -> Result<TargetLocation, String> {
    if root.identity != identity {
        return Err("recorded directory identity differs from run_dir".into());
    }
    Ok(locate_in_run_dir(&root.held, relative, capture))
}

pub(super) fn locate_bound_for_operator(
    metadata: &ManagedCredentialMaterialization,
    identity: DirectoryIdentity,
    relative: &Path,
    mark_only: bool,
    capture: Option<&OsStr>,
) -> Result<TargetLocation, String> {
    let root = metadata
        .run_root_canonical
        .as_deref()
        .ok_or("missing recorded run_dir")?;
    let held = match AnchoredDirectory::open_absolute(Path::new(root)) {
        Ok(held) => held,
        Err(err) if err.kind() == io::ErrorKind::NotFound => {
            return if mark_only {
                Ok(TargetLocation::Missing)
            } else {
                Err("recorded run_dir is absent; cannot confirm directory was not renamed without --mark-only".into())
            };
        }
        Err(err) => return Err(format!("cannot open recorded run_dir: {err}")),
    };
    if held
        .identity()
        .map_err(|err| format!("identify recorded run_dir: {err}"))?
        != identity
    {
        return Err("recorded run_dir now has a different directory identity".into());
    }
    Ok(locate_in_run_dir(&held, relative, capture))
}

pub(super) fn locate_legacy(root: &LiveRunRoot, target: &Path) -> Result<TargetLocation, String> {
    let absolute = legacy_platform_path(&root.absolute);
    if !matches!(root.held.matches_absolute_path(&absolute), Ok(true)) {
        return Err("legacy target requires run_dir without symlink aliases".into());
    }
    let target = legacy_platform_path(target);
    let relative = target
        .strip_prefix(&root.canonical)
        .map_err(|_| "legacy target is outside the canonical run_dir")?;
    Ok(locate_in_run_dir(&root.held, relative, None))
}

fn legacy_platform_path(path: &Path) -> std::borrow::Cow<'_, Path> {
    #[cfg(target_os = "macos")]
    for alias in ["/var", "/tmp", "/etc"] {
        if let Ok(relative) = path.strip_prefix(alias) {
            let platform_target = PathBuf::from(format!("/private{alias}"));
            if std::fs::read_link(alias)
                .is_ok_and(|link| Path::new("/").join(link) == platform_target)
            {
                return std::borrow::Cow::Owned(platform_target.join(relative));
            }
        }
    }
    std::borrow::Cow::Borrowed(path)
}

fn locate_path(target: &Path, run_dir: Option<&Path>) -> TargetLocation {
    if let Some(run_dir) = run_dir {
        let resolved_target = if target.is_absolute() {
            target.to_path_buf()
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
        // Canonicalize `run_dir` once and keep that exact path for both the
        // prefix match and the anchored open, so an alias target swap between
        // match and open cannot redirect cleanup into another directory.
        let canonical_run_dir = match std::fs::canonicalize(run_dir) {
            Ok(path) => Some(path),
            Err(err) if err.kind() == io::ErrorKind::NotFound => None,
            Err(err) => {
                return TargetLocation::Unremovable(format!(
                    "resolve run_dir '{}': {err}",
                    run_dir.display()
                ))
            }
        };
        if let Some(canonical_run_dir) = canonical_run_dir {
            if let Some(rel) = relative_under_run_dir(&resolved_target, run_dir, &canonical_run_dir)
            {
                return match AnchoredDirectory::open_absolute(&canonical_run_dir) {
                    Ok(directory) => locate_in_run_dir(&directory, &rel, None),
                    Err(err) if err.kind() == io::ErrorKind::NotFound => TargetLocation::Missing,
                    Err(err) => TargetLocation::Unremovable(format!("open run_dir: {err}")),
                };
            }
        }
        return locate_recorded(&resolved_target);
    }
    locate_for_operator(target)
}

fn relative_under_run_dir(
    target: &Path,
    run_dir: &Path,
    canonical_run_dir: &Path,
) -> Option<PathBuf> {
    // Strip the lexical `run_dir` prefix first so legacy records recorded under
    // an alias located inside `canonical_run_dir` (e.g. `<real>/current -> <real>`)
    // resolve cleanly relative to `canonical_run_dir` instead of leaving the
    // symlink component at the head of the relative path.
    if let Ok(rel) = target.strip_prefix(run_dir) {
        return Some(rel.to_path_buf());
    }
    target
        .strip_prefix(canonical_run_dir)
        .ok()
        .map(Path::to_path_buf)
}

fn locate_in_run_dir(
    held: &AnchoredDirectory,
    rel: &Path,
    capture: Option<&OsStr>,
) -> TargetLocation {
    let mut components = rel
        .components()
        .filter(|component| !matches!(component, Component::CurDir))
        .collect::<Vec<_>>();
    let Some(Component::Normal(leaf)) = components.pop() else {
        return TargetLocation::Unremovable(format!(
            "'{}' does not name a file inside run_dir",
            rel.display()
        ));
    };
    if components
        .iter()
        .any(|component| !matches!(component, Component::Normal(_)))
    {
        return TargetLocation::Unremovable(format!(
            "'{}' is not an ordinary path inside run_dir",
            rel.display()
        ));
    }
    let mut directory = held.clone();
    let mut opened = PathBuf::new();
    for component in components {
        if let Component::Normal(name) = component {
            opened.push(name);
            directory = match open_component(&directory, name, &opened) {
                Ok(dir) => dir,
                Err(location) => return location,
            };
        }
    }
    if let Some(capture) = capture {
        match directory.entry_kind(capture) {
            Ok(EntryKind::Missing) => {}
            Ok(_) => {
                return TargetLocation::Removable(RemovableTarget {
                    parent: directory,
                    leaf: leaf.to_os_string(),
                })
            }
            Err(err) => {
                return TargetLocation::Unremovable(format!("inspect interrupted removal: {err}"))
            }
        }
    }
    classify(directory, leaf)
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
        Ok(EntryKind::Directory) => {
            TargetLocation::Unremovable(format!("open '{}': {error}", opened.display()))
        }
        Ok(_) => TargetLocation::Unremovable(format!("'{}' is not a directory", opened.display())),
        Err(_) if error.kind() == io::ErrorKind::NotFound => TargetLocation::Missing,
        Err(_) => TargetLocation::Unremovable(format!("open '{}': {error}", opened.display())),
    })
}

/// Manual cleanup resolves the operator's own parent path, including its
/// symlinked ancestors, and refuses only what holds the target name itself.
pub(super) fn locate_for_operator(target: &Path) -> TargetLocation {
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
        Ok(EntryKind::Missing) => TargetLocation::MissingAt(directory),
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

pub(super) fn inspect(target: &Path, run_dir: Option<&Path>, max_bytes: usize) -> TargetInspection {
    let removable = match locate_path(target, run_dir) {
        TargetLocation::Missing | TargetLocation::MissingAt(_) => return TargetInspection::Missing,
        TargetLocation::Unremovable(reason) => return TargetInspection::UntrustedEntry { reason },
        TargetLocation::Removable(removable) => removable,
    };
    match removable
        .parent
        .read_regular_file(&removable.leaf, max_bytes)
    {
        Ok(Some((metadata, bytes))) => TargetInspection::RegularFile {
            mode: metadata.permissions().mode() & 0o777,
            bytes,
        },
        Ok(None) => match classify(removable.parent, &removable.leaf) {
            TargetLocation::Missing | TargetLocation::MissingAt(_) => TargetInspection::Missing,
            TargetLocation::Unremovable(reason) => TargetInspection::UntrustedEntry { reason },
            TargetLocation::Removable(_) => TargetInspection::Unreadable {
                reason: "target changed during inspection".into(),
            },
        },
        Err(err) => TargetInspection::Unreadable {
            reason: err.to_string(),
        },
    }
}

pub(super) fn restore(
    target: &RemovableTarget,
    backup: &Path,
    destination: &Path,
    identity: (u64, u64),
    capture: Option<&OsStr>,
) -> io::Result<()> {
    let dest = destination
        .file_name()
        .filter(|dest| *dest != target.leaf && backup.parent() == destination.parent())
        .ok_or_else(|| io::Error::other("credential restore destination must be a sibling"))?;
    target
        .parent
        .restore_file_if_matching(&target.leaf, dest, identity, capture)
}

pub(super) fn confirm_original(
    target: &RemovableTarget,
    inode: (u64, u64),
    capture: Option<&OsStr>,
) -> io::Result<bool> {
    target
        .parent
        .sync_file_if_matching(&target.leaf, inode, capture)
}

pub(super) fn remove<A: Fn(&[u8]) -> bool>(
    target: &RemovableTarget,
    identity: Option<(u64, u64)>,
    capture: Option<&OsStr>,
    require_managed_content: Option<&A>,
) -> io::Result<Removal> {
    if identity.is_none() && require_managed_content.is_none() {
        return target.parent.remove_entry(&target.leaf).map(|removed| {
            if removed {
                Removal::Removed
            } else {
                Removal::Vanished
            }
        });
    }
    Ok(
        match target.parent.remove_file_if_matching(
            &target.leaf,
            identity,
            capture,
            require_managed_content,
        )? {
            Some(true) => Removal::Removed,
            Some(false) => Removal::ForeignContent,
            None => Removal::Vanished,
        },
    )
}
