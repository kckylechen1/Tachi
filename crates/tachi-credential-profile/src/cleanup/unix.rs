//! Descriptor-anchored location and removal of managed credential targets.
//!
//! Run-scoped cleanup follows a tree a worker could write to, so it opens the
//! path Tachi itself recorded from `/` without following a single symlink: a
//! parent swapped for a symlink cannot steer the unlink into a host directory.
//! Manual cleanup follows the operator's own path up to the parent directory,
//! but never a symlink that holds the target name itself.

use super::{
    ManagedCredentialMaterialization, RemovableTarget, Removal, RunRoot, TargetInspection,
    TargetLocation,
};
use memcore::anchored_fs::{AnchoredDirectory, EntryKind};
use std::ffi::OsStr;
use std::io;
use std::os::unix::fs::PermissionsExt;
use std::path::{Component, Path, PathBuf};

pub(super) fn locate(
    metadata: &ManagedCredentialMaterialization,
    run_root: Option<&RunRoot>,
) -> TargetLocation {
    locate_path(Path::new(&metadata.target), run_root)
}

/// Resolve the run directory once per cleanup. The platform's own top-level
/// aliases are expanded by name; every other component is opened without
/// following a link. Any other link on the way refuses the whole run
/// directory: it sits where a worker may write, and where it points now says
/// nothing about where it pointed when the credentials were recorded.
pub(super) fn open_run_root(run_dir: &Path) -> Result<RunRoot, String> {
    let cwd = std::env::current_dir().map_err(|err| {
        format!(
            "resolve the current directory for run_dir '{}': {err}",
            run_dir.display()
        )
    })?;
    let as_given = if run_dir.is_absolute() {
        run_dir.to_path_buf()
    } else {
        cwd.join(run_dir)
    };
    // `..` is not resolved by name here: that would skip whatever sits
    // before it, a link included, and a run directory is exactly where a
    // link must not go unchecked.
    if as_given
        .components()
        .any(|component| matches!(component, Component::ParentDir))
    {
        return Err(format!(
            "run directory '{}' cannot be opened as written: name it without '..'",
            run_dir.display()
        ));
    }
    let resolved = expand_platform_aliases(&as_given);
    let mut directory = AnchoredDirectory::open_absolute(Path::new("/"))
        .map_err(|err| format!("open '/': {err}"))?;
    let mut opened = PathBuf::from("/");
    let mut missing = false;
    for component in resolved.components() {
        let Component::Normal(name) = component else {
            continue;
        };
        opened.push(name);
        match open_component(&directory, name, &opened) {
            Ok(next) => directory = next,
            Err(TargetLocation::Missing) => {
                missing = true;
                break;
            }
            Err(TargetLocation::Unremovable(reason)) => {
                return Err(format!(
                    "run directory '{}' cannot be opened without following a link: {reason}",
                    run_dir.display()
                ));
            }
            // `open_component` only opens directories. A later change must not
            // turn this into a silent pass.
            Err(TargetLocation::Removable(_)) => {
                return Err(format!(
                    "run directory '{}' cannot be opened as a directory",
                    run_dir.display()
                ));
            }
        }
    }
    Ok(RunRoot {
        as_given,
        resolved,
        cwd,
        directory: if missing { None } else { Some(directory) },
    })
}

/// macOS keeps `/var`, `/tmp` and `/etc` as links into `/private`. They belong
/// to the system and sit where no worker writes, so they are expanded by
/// name, after checking where they point. Everything else is left for the
/// no-follow open.
#[cfg(target_os = "macos")]
fn expand_platform_aliases(path: &Path) -> PathBuf {
    const ALIASES: [(&str, &str); 3] = [
        ("var", "private/var"),
        ("tmp", "private/tmp"),
        ("etc", "private/etc"),
    ];
    let mut components = path.components();
    if let (Some(Component::RootDir), Some(Component::Normal(first))) =
        (components.next(), components.next())
    {
        for (alias, real) in ALIASES {
            if first == OsStr::new(alias)
                && std::fs::read_link(Path::new("/").join(alias))
                    .is_ok_and(|target| target == Path::new(real))
            {
                return Path::new("/")
                    .join(real)
                    .components()
                    .chain(components)
                    .collect();
            }
        }
    }
    path.to_path_buf()
}

#[cfg(not(target_os = "macos"))]
fn expand_platform_aliases(path: &Path) -> PathBuf {
    path.to_path_buf()
}

fn locate_path(target: &Path, run_root: Option<&RunRoot>) -> TargetLocation {
    let Some(root) = run_root else {
        return locate_for_operator(target);
    };
    let resolved_target = if target.is_absolute() {
        target.to_path_buf()
    } else {
        // The working directory of this cleanup, the same one selection read
        // the target against.
        root.cwd.join(target)
    };
    let Some(directory) = root.directory.as_ref() else {
        return TargetLocation::Missing;
    };
    let Some(rel) = relative_under(&resolved_target, &root.as_given, &root.resolved) else {
        return TargetLocation::Unremovable(
            "it is not under the run directory this cleanup resolved".into(),
        );
    };
    locate_in_run_dir(directory.clone(), root.resolved.clone(), &rel)
}

/// Where `target` sits under the run directory, by either spelling of it.
fn relative_under(target: &Path, as_given: &Path, resolved: &Path) -> Option<PathBuf> {
    target
        .strip_prefix(as_given)
        .or_else(|_| target.strip_prefix(resolved))
        .ok()
        .map(Path::to_path_buf)
}

/// Locate `rel` under the run directory this cleanup opened. `root` is only
/// for messages; the handle is what is walked.
fn locate_in_run_dir(
    mut directory: AnchoredDirectory,
    root: PathBuf,
    rel: &Path,
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
    let mut opened = root;
    for component in components {
        if let Component::Normal(name) = component {
            opened.push(name);
            directory = match open_component(&directory, name, &opened) {
                Ok(dir) => dir,
                Err(location) => return location,
            };
        }
    }
    classify(directory, leaf)
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

pub(super) fn inspect(
    target: &Path,
    run_root: Option<&RunRoot>,
    max_bytes: usize,
) -> TargetInspection {
    let removable = match locate_path(target, run_root) {
        TargetLocation::Missing => return TargetInspection::Missing,
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
            TargetLocation::Missing => TargetInspection::Missing,
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
