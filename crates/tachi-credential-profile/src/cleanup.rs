use super::managed::{
    managed_materialization_content_hash, mark_metadata_cleaned, ManagedCredentialMaterialization,
};
use super::types::{CredentialCleanupOptions, CredentialCleanupReport};
use super::CREDENTIAL_MATERIALIZATION_NAMESPACE;
#[cfg(unix)]
use memcore::anchored_fs::AnchoredDirectory;
use memcore::MemoryStore;
use std::ffi::OsString;
use std::path::{Component, Path, PathBuf};

#[cfg(not(unix))]
mod fallback;
#[cfg(unix)]
mod unix;
#[cfg(not(unix))]
use fallback as target_fs;
#[cfg(unix)]
use unix as target_fs;

/// An opened parent directory on Unix; the parent's path elsewhere.
#[cfg(unix)]
type TargetParent = AnchoredDirectory;
#[cfg(not(unix))]
type TargetParent = PathBuf;

/// A regular file the cleanup may remove, held through its parent so removal
/// never re-resolves the mutable pathname.
struct RemovableTarget {
    parent: TargetParent,
    leaf: OsString,
}

/// What the cleanup found at a managed target.
enum TargetLocation {
    /// Nothing holds the target name.
    Missing,
    /// A regular file reached without following a symlink that holds it.
    Removable(RemovableTarget),
    /// Something the cleanup must not traverse or remove; the reason names it.
    Unremovable(String),
}

enum Removal {
    Removed,
    /// The entry disappeared while the cleanup held it.
    Vanished,
    /// The captured bytes are no longer the managed content; the file was
    /// restored under its own name.
    ForeignContent,
}

/// The run directory of a scoped cleanup, resolved once so that selecting
/// records and removing files share one view of it.
pub(crate) enum RunRoot {
    /// The run directory is gone, and so are its targets.
    Missing,
    /// `as_given` is the path the caller named; `resolved` is the same path
    /// with the platform's own top-level aliases expanded. Either spelling
    /// selects records. Removal walks from the opened handle.
    Open {
        as_given: PathBuf,
        resolved: PathBuf,
        #[cfg(unix)]
        directory: AnchoredDirectory,
    },
}

/// Resolve the run directory for a cleanup or an inspection. `Err` means the
/// directory cannot be opened without following a link its caller cannot
/// vouch for. A cleanup must then leave the run directory alone, records
/// included: a link where a worker may write can be repointed, and where it
/// points now says nothing about where the credentials were recorded.
pub(crate) fn open_run_root(run_dir: &Path) -> Result<RunRoot, String> {
    target_fs::open_run_root(run_dir)
}

/// Safe descriptor-anchored inspection of a credential target for `doctor`.
pub(crate) enum TargetInspection {
    Missing,
    RegularFile {
        #[cfg(unix)]
        mode: u32,
        bytes: Vec<u8>,
    },
    UntrustedEntry {
        reason: String,
    },
    Unreadable {
        reason: String,
    },
}

pub(crate) fn inspect_target_file(
    target: &Path,
    run_root: Option<&RunRoot>,
    max_bytes: usize,
) -> TargetInspection {
    target_fs::inspect(target, run_root, max_bytes)
}

pub fn cleanup_ephemeral_credential_materializations(
    store: &MemoryStore,
    run_dir: &Path,
    dry_run: bool,
) -> Result<CredentialCleanupReport, String> {
    cleanup_managed_credential_materializations(
        store,
        &CredentialCleanupOptions {
            run_dir: Some(run_dir.to_path_buf()),
            profile: None,
            consumer: None,
            dry_run,
            mark_only: false,
        },
    )
}

fn metadata_matches_cleanup_scope(
    metadata: &ManagedCredentialMaterialization,
    options: &CredentialCleanupOptions,
    run_root: Option<&RunRoot>,
) -> bool {
    if let Some(profile) = &options.profile {
        if metadata.profile != *profile {
            return false;
        }
    }
    if let Some(consumer) = &options.consumer {
        if metadata.consumer != *consumer {
            return false;
        }
    }
    if options.run_dir.is_some() {
        let target = PathBuf::from(&metadata.target);
        if target
            .components()
            .any(|component| matches!(component, Component::ParentDir))
        {
            return false;
        }
        // Temporary materializations record their canonical physical path,
        // older ones the path they were given. Both spellings of the run
        // directory select records. They were resolved once for this
        // cleanup, so what is selected here is what is removed below.
        let RunRoot::Open {
            as_given, resolved, ..
        } = run_root.expect("a run-scoped cleanup resolves its run directory first")
        else {
            return true;
        };
        if !target.starts_with(as_given) && !target.starts_with(resolved) {
            return false;
        }
    }
    true
}

pub fn cleanup_managed_credential_materializations(
    store: &MemoryStore,
    options: &CredentialCleanupOptions,
) -> Result<CredentialCleanupReport, String> {
    if options.run_dir.is_none() && options.profile.is_none() && options.consumer.is_none() {
        return Err(
            "credential cleanup requires at least one scope: --run-dir, --profile, or --consumer"
                .to_string(),
        );
    }
    // Resolved once: a run directory that cannot be opened without following
    // a link refuses the whole cleanup before any record is looked at, so
    // no record is selected, removed or marked behind that link.
    let run_root = match options.run_dir.as_deref() {
        Some(dir) => Some(open_run_root(dir)?),
        None => None,
    };
    let run_dir = options.run_dir.clone().unwrap_or_default();
    let credentials_dir = run_dir.join("credentials");
    let mut report = CredentialCleanupReport {
        run_dir: run_dir.to_string_lossy().to_string(),
        credentials_dir: credentials_dir.to_string_lossy().to_string(),
        dry_run: options.dry_run,
        profile: options.profile.clone(),
        consumer: options.consumer.clone(),
        mark_only: options.mark_only,
        would_mark: Vec::new(),
        marked: Vec::new(),
        would_remove: Vec::new(),
        removed: Vec::new(),
        missing: Vec::new(),
        skipped: Vec::new(),
        errors: Vec::new(),
    };
    let rows = store
        .list_state(CREDENTIAL_MATERIALIZATION_NAMESPACE)
        .map_err(|e| format!("list credential materialization metadata: {e}"))?;
    for row in rows {
        let mut metadata: ManagedCredentialMaterialization =
            match serde_json::from_str(&row.value_json) {
                Ok(metadata) => metadata,
                Err(err) => {
                    report
                        .errors
                        .push(format!("parse metadata '{}': {err}", row.key));
                    continue;
                }
            };
        if metadata.cleanup_status.as_deref() == Some("cleaned") {
            report.skipped.push(metadata.target);
            continue;
        }
        if !metadata_matches_cleanup_scope(&metadata, options, run_root.as_ref()) {
            report.skipped.push(metadata.target);
            continue;
        }
        // Every target is located through its parent directory, never by a
        // pathname a worker could repoint between the check and the removal.
        let removable = match target_fs::locate(&metadata, run_root.as_ref()) {
            TargetLocation::Missing => {
                if !options.dry_run {
                    mark_metadata_cleaned(
                        store,
                        &row.key,
                        &mut metadata,
                        options.run_dir.as_deref(),
                    )?;
                }
                report.missing.push(metadata.target.clone());
                continue;
            }
            // Marking only rewrites metadata, so an entry the cleanup refuses to
            // remove can still be marked after review.
            TargetLocation::Unremovable(_) if options.mark_only => {
                if options.dry_run {
                    report.would_mark.push(metadata.target.clone());
                } else {
                    mark_metadata_cleaned(
                        store,
                        &row.key,
                        &mut metadata,
                        options.run_dir.as_deref(),
                    )?;
                    report.marked.push(metadata.target.clone());
                }
                continue;
            }
            TargetLocation::Unremovable(reason) => {
                report.errors.push(format!(
                    "refusing to remove credential target '{}': {reason}",
                    metadata.target
                ));
                continue;
            }
            TargetLocation::Removable(removable) => removable,
        };
        if options.mark_only {
            if options.dry_run {
                report.would_mark.push(metadata.target.clone());
                continue;
            }
            mark_metadata_cleaned(store, &row.key, &mut metadata, options.run_dir.as_deref())?;
            report.marked.push(metadata.target.clone());
            continue;
        }
        if metadata.materializer_type == "config_patch" && options.run_dir.is_none() {
            report.skipped.push(format!(
                "{} (config_patch requires --mark-only unless scoped to --run-dir)",
                metadata.target
            ));
            continue;
        }
        if options.dry_run {
            report.would_remove.push(metadata.target.clone());
            continue;
        }
        // Run-scoped cleanup owns everything under the run directory; manual
        // cleanup removes only the bytes it materialized.
        let require_managed_content = options.run_dir.is_none().then_some(|captured: &[u8]| {
            std::str::from_utf8(captured).is_ok_and(|content| {
                managed_materialization_content_hash(
                    &metadata.profile,
                    &metadata.consumer,
                    &metadata.target,
                    content,
                ) == metadata.content_hash
            })
        });
        match target_fs::remove(&removable, require_managed_content.as_ref()) {
            Ok(Removal::Removed) => {
                mark_metadata_cleaned(store, &row.key, &mut metadata, options.run_dir.as_deref())?;
                report.removed.push(metadata.target.clone());
            }
            Ok(Removal::Vanished) => {
                mark_metadata_cleaned(store, &row.key, &mut metadata, options.run_dir.as_deref())?;
                report.missing.push(metadata.target.clone());
            }
            Ok(Removal::ForeignContent) => report.skipped.push(format!(
                "{} (hash mismatch; use --mark-only after review)",
                metadata.target
            )),
            Err(err) => report.errors.push(format!(
                "remove credential target '{}': {err}",
                metadata.target
            )),
        }
    }
    Ok(report)
}
