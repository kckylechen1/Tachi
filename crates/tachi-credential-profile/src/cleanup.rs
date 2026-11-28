use super::managed::{
    close_metadata, managed_materialization_content_hash, ManagedCredentialMaterialization,
    PUBLICATION_PREFIX,
};
use super::safety::{absolute_run_dir, refuse_parent_dir};
use super::types::{CredentialCleanupOptions, CredentialCleanupReport};
use super::CREDENTIAL_MATERIALIZATION_NAMESPACE;
#[cfg(unix)]
use memcore::anchored_fs::{AnchoredDirectory, DirectoryIdentity};
use memcore::db::StateRow;
use memcore::store::state::StateTransaction;
use memcore::MemoryStore;
use std::collections::HashSet;
use std::ffi::{OsStr, OsString};
use std::fs;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};

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
    /// The leaf is absent under a held parent that can be synced before closing.
    #[cfg(unix)]
    MissingAt(AnchoredDirectory),
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
    run_dir: Option<&Path>,
    max_bytes: usize,
) -> TargetInspection {
    target_fs::inspect(target, run_dir, max_bytes)
}

pub fn cleanup_ephemeral_credential_materializations(
    store: &MemoryStore,
    run_dir: &Path,
    dry_run: bool,
) -> Result<CredentialCleanupReport, String> {
    cleanup_ephemeral_credential_materializations_for_run(store, run_dir, None, dry_run)
}

pub fn cleanup_ephemeral_credential_materializations_for_run(
    store: &MemoryStore,
    run_dir: &Path,
    run_generation: Option<&str>,
    dry_run: bool,
) -> Result<CredentialCleanupReport, String> {
    cleanup_managed_credential_materializations(
        store,
        &CredentialCleanupOptions {
            run_dir: Some(run_dir.to_path_buf()),
            run_generation: run_generation.map(str::to_owned),
            dry_run,
            ..Default::default()
        },
    )
}

enum RunScope {
    Live(LiveRunRoot),
    Absent(PathBuf),
}

struct LiveRunRoot {
    #[cfg(unix)]
    absolute: PathBuf,
    #[cfg(unix)]
    canonical: PathBuf,
    #[cfg(unix)]
    held: AnchoredDirectory,
    #[cfg(unix)]
    identity: DirectoryIdentity,
}

impl RunScope {
    fn open(run_dir: &Path) -> Result<Self, String> {
        let absolute = absolute_run_dir(run_dir)?;
        let canonical = match fs::canonicalize(&absolute) {
            Ok(path) => path,
            Err(err) if err.kind() == ErrorKind::NotFound => return Ok(Self::Absent(absolute)),
            Err(err) => {
                return Err(format!(
                    "resolve credential run_dir '{}': {err}",
                    absolute.display()
                ))
            }
        };
        #[cfg(unix)]
        {
            let held = AnchoredDirectory::open_absolute(&canonical).map_err(|err| {
                format!("open credential run_dir '{}': {err}", canonical.display())
            })?;
            let identity = held
                .identity()
                .map_err(|err| format!("identify credential run_dir: {err}"))?;
            Ok(Self::Live(LiveRunRoot {
                absolute,
                canonical,
                held,
                identity,
            }))
        }
        #[cfg(not(unix))]
        {
            if !canonical.is_dir() {
                return Err("credential run_dir must be a directory".into());
            }
            Ok(Self::Live(LiveRunRoot {}))
        }
    }

    fn verify(&self) -> Result<(), String> {
        match self {
            #[cfg(unix)]
            Self::Live(root)
                if !matches!(root.held.matches_absolute_path(&root.canonical), Ok(true)) =>
            {
                Err("credential run_dir changed during cleanup".into())
            }
            Self::Absent(absolute)
                if !matches!(
                    fs::symlink_metadata(absolute),
                    Err(err) if err.kind() == ErrorKind::NotFound
                ) =>
            {
                Err("credential run_dir reappeared during cleanup".into())
            }
            _ => Ok(()),
        }
    }
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
    let granted_run_dir = options.run_dir.as_deref();
    if let Some(run_dir) = granted_run_dir {
        refuse_parent_dir(run_dir, "credential run_dir")?;
    }
    let run_dir = granted_run_dir.unwrap_or_else(|| Path::new(""));
    let mut report = CredentialCleanupReport {
        run_dir: run_dir.to_string_lossy().into_owned(),
        credentials_dir: run_dir.join("credentials").to_string_lossy().into_owned(),
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
    if options.dry_run {
        // The CLI opens the store read-only for a preview. It takes a single
        // row snapshot and makes no filesystem or hard-state changes.
        let scope = granted_run_dir.map(RunScope::open).transpose()?;
        let rows = store
            .list_state(CREDENTIAL_MATERIALIZATION_NAMESPACE)
            .map_err(|err| format!("list credential materialization metadata: {err}"))?;
        cleanup_rows(None, rows, scope.as_ref(), options, &mut report)?;
        return Ok(report);
    }
    store
        .with_state_transaction(|state| {
            let scope = granted_run_dir.map(RunScope::open).transpose();
            let scope = match scope {
                Ok(scope) => scope,
                Err(err) => return Ok(Err(err)),
            };
            let rows = state.list_state(CREDENTIAL_MATERIALIZATION_NAMESPACE)?;
            // If the scope changes after a successful unlink, keep that unlink's
            // bookkeeping committed while refusing to process any further rows.
            Ok(cleanup_rows(
                Some(state),
                rows,
                scope.as_ref(),
                options,
                &mut report,
            ))
        })
        .map_err(|err| format!("credential cleanup transaction: {err}"))??;
    Ok(report)
}

fn locate_for_cleanup(
    metadata: &ManagedCredentialMaterialization,
    scope: Option<&RunScope>,
    mark_only: bool,
    capture: Option<&OsStr>,
) -> Result<TargetLocation, String> {
    if let Some((identity, relative)) = metadata.bound_target() {
        return match scope {
            Some(RunScope::Live(root)) => {
                target_fs::locate_bound(root, identity, relative, capture)
            }
            Some(RunScope::Absent(absolute))
                if metadata.run_root_canonical.as_deref().map(Path::new)
                    == Some(absolute.as_path()) =>
            {
                if mark_only {
                    Ok(TargetLocation::Missing)
                } else {
                    Ok(TargetLocation::Unremovable(
                        "recorded run_dir is absent and may have been renamed".into(),
                    ))
                }
            }
            Some(RunScope::Absent(_)) => {
                Err("recorded run_dir differs from the absent scope".into())
            }
            None => target_fs::locate_bound_for_operator(
                metadata, identity, relative, mark_only, capture,
            ),
        };
    }
    if !metadata.is_legacy() {
        return Err("incomplete credential run binding".into());
    }
    match scope {
        None => Ok(target_fs::locate_for_operator(Path::new(&metadata.target))),
        Some(scope) => {
            let target = Path::new(&metadata.target);
            refuse_parent_dir(target, "legacy credential target")?;
            if !target.is_absolute() {
                return Err("legacy target is relative and has no provable run_dir".into());
            }
            if let RunScope::Live(root) = scope {
                target_fs::locate_legacy(root, target)
            } else {
                Err("legacy target has no verifiable run_dir".into())
            }
        }
    }
}

fn matches_selection(
    metadata: &ManagedCredentialMaterialization,
    options: &CredentialCleanupOptions,
) -> bool {
    metadata.cleanup_status.is_none()
        && options
            .profile
            .as_ref()
            .is_none_or(|name| *name == metadata.profile)
        && options
            .consumer
            .as_ref()
            .is_none_or(|name| *name == metadata.consumer)
        && (options.run_generation.is_none()
            || metadata.is_legacy()
            || options.run_generation == metadata.run_generation)
}

fn is_backup(row: &StateRow) -> bool {
    row.key.starts_with(PUBLICATION_PREFIX) && row.key.ends_with(":backup")
}

fn cleanup_rows(
    state: Option<&StateTransaction<'_>>,
    mut rows: Vec<StateRow>,
    scope: Option<&RunScope>,
    options: &CredentialCleanupOptions,
    report: &mut CredentialCleanupReport,
) -> Result<(), String> {
    // A committed publication deletes its intent. Cleanup closes intents in place
    // so interrupted recovery can still identify the corresponding backup.
    let publication_intents: HashSet<_> = rows
        .iter()
        .filter(|row| {
            row.key.starts_with(PUBLICATION_PREFIX)
                && !row.key.ends_with(":stage")
                && !is_backup(row)
        })
        .map(|row| row.key.clone())
        .collect();
    // Clear unpublished files before restoring backups, and restore backups
    // before inspecting normal owners so both v1 legacy and v2 owners see their
    // restored original files (and are preserved when unselected or cleaned when selected).
    rows.sort_by_key(|row| (!row.key.starts_with(PUBLICATION_PREFIX), is_backup(row)));
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
        let identity = metadata.target_identity();
        if !matches_selection(&metadata, options) {
            report.skipped.push(metadata.target);
            continue;
        }
        let same_generation =
            options.run_generation.is_some() && options.run_generation == metadata.run_generation;
        let capture = identity.map(|_| cleanup_capture_name(&row.key));
        let capture = capture.as_deref().map(OsStr::new);
        let location = match locate_for_cleanup(&metadata, scope, options.mark_only, capture) {
            Ok(location) => location,
            Err(reason) => {
                let messages = if same_generation || (!options.mark_only && scope.is_none()) {
                    &mut report.errors
                } else {
                    &mut report.skipped
                };
                messages.push(format!("{reason}: {}", metadata.target));
                continue;
            }
        };
        if let Some(scope) = scope {
            scope.verify()?;
        }
        let required = options.run_generation.is_some();
        if metadata.is_legacy() && !required && scope.is_some() {
            if let TargetLocation::Unremovable(reason) = &location {
                report
                    .skipped
                    .push(format!("{reason}: {}", metadata.target));
                continue;
            }
        }

        let removed = match location {
            TargetLocation::Missing => {
                if options.dry_run {
                    report.missing.push(metadata.target);
                    continue;
                }
                false
            }
            #[cfg(unix)]
            TargetLocation::MissingAt(parent) => {
                if options.dry_run {
                    report.missing.push(metadata.target);
                    continue;
                }
                if !options.mark_only {
                    if let Err(err) = parent.sync() {
                        report.errors.push(format!(
                            "sync parent of credential target '{}': {err}",
                            metadata.target
                        ));
                        continue;
                    }
                }
                false
            }
            _ if options.mark_only => {
                if options.dry_run {
                    report.would_mark.push(metadata.target);
                } else if close_row(state, &row, &mut metadata, options, false, report) {
                    report.marked.push(metadata.target);
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
            TargetLocation::Removable(removable) => {
                if metadata.materializer_type == "config_patch" && scope.is_none() {
                    let messages = if required {
                        &mut report.errors
                    } else {
                        &mut report.skipped
                    };
                    messages.push(format!(
                        "{} (config_patch requires --mark-only unless scoped to --run-dir)",
                        metadata.target
                    ));
                    continue;
                }
                if options.dry_run {
                    report.would_remove.push(metadata.target);
                    continue;
                }

                let restore = metadata
                    .bound_target()
                    .zip(metadata.restore_rel.as_deref())
                    .zip(identity)
                    .filter(|_| {
                        row.key
                            .strip_suffix(":backup")
                            .is_some_and(|intent| publication_intents.contains(intent))
                    });
                if let Some((((_, backup), destination), inode)) = restore {
                    match target_fs::restore(
                        &removable,
                        backup,
                        Path::new(destination),
                        inode,
                        capture,
                    ) {
                        Ok(()) => {
                            if close_row(state, &row, &mut metadata, options, true, report) {
                                report.removed.push(metadata.target);
                            }
                        }
                        Err(err) => report.errors.push(format!(
                            "restore credential backup '{}': {err}",
                            metadata.target
                        )),
                    }
                    continue;
                }

                let journal = row.key.starts_with(PUBLICATION_PREFIX) && identity.is_some();
                let require_managed_content =
                    (scope.is_none() && !journal).then_some(|captured: &[u8]| {
                        std::str::from_utf8(captured).is_ok_and(|content| {
                            managed_materialization_content_hash(
                                &metadata.profile,
                                &metadata.consumer,
                                &metadata.target,
                                content,
                            ) == metadata.content_hash
                        })
                    });
                match target_fs::remove(
                    &removable,
                    identity,
                    capture,
                    require_managed_content.as_ref(),
                ) {
                    Ok(Removal::Removed) => true,
                    Ok(Removal::Vanished) => false,
                    Ok(Removal::ForeignContent) => {
                        if journal && !row.key.ends_with(":stage") && !is_backup(&row) {
                            match confirm_original(
                                state.expect("only applied cleanup modifies files"),
                                &row,
                                &metadata,
                                &removable,
                                capture,
                            ) {
                                Ok(true) => {
                                    if close_row(state, &row, &mut metadata, options, false, report)
                                    {
                                        report.missing.push(metadata.target);
                                    }
                                    continue;
                                }
                                Ok(false) => {}
                                Err(err) => {
                                    report.errors.push(format!(
                                        "confirm original credential target '{}': {err}",
                                        metadata.target
                                    ));
                                    continue;
                                }
                            }
                        }
                        let messages = if required || (scope.is_some() && identity.is_some()) {
                            &mut report.errors
                        } else {
                            &mut report.skipped
                        };
                        messages.push(format!(
                            "{} (file identity or hash mismatch; use --mark-only after review)",
                            metadata.target
                        ));
                        continue;
                    }
                    Err(err) => {
                        report.errors.push(format!(
                            "remove credential target '{}': {err}",
                            metadata.target
                        ));
                        continue;
                    }
                }
            }
        };
        close_row(state, &row, &mut metadata, options, removed, report);
        if removed {
            report.removed.push(metadata.target);
        } else {
            report.missing.push(metadata.target);
        }
    }
    Ok(())
}

fn confirm_original(
    state: &StateTransaction<'_>,
    row: &StateRow,
    metadata: &ManagedCredentialMaterialization,
    target: &RemovableTarget,
    capture: Option<&OsStr>,
) -> Result<bool, String> {
    // Recovery can stop after restoring the original name but before SQLite
    // commits. The paired backup still records the inode that belongs there.
    let Some((json, _)) = state
        .get_state(
            CREDENTIAL_MATERIALIZATION_NAMESPACE,
            &format!("{}:backup", row.key),
        )
        .map_err(|err| err.to_string())?
    else {
        return Ok(false);
    };
    let backup: ManagedCredentialMaterialization =
        serde_json::from_str(&json).map_err(|err| err.to_string())?;
    let Some((root, relative)) = metadata.bound_target() else {
        return Ok(false);
    };
    if backup.restore_rel.as_deref().map(Path::new) != Some(relative)
        || !backup
            .bound_target()
            .is_some_and(|(backup_root, _)| backup_root == root)
    {
        return Ok(false);
    }
    let Some(inode) = backup.target_identity() else {
        return Ok(false);
    };
    target_fs::confirm_original(target, inode, capture).map_err(|err| err.to_string())
}

pub(crate) fn cleanup_capture_name(key: &str) -> String {
    format!(".tachi-cleanup-{}", tachi_params::util::stable_hash(key))
}

fn close_row(
    state: Option<&StateTransaction<'_>>,
    row: &StateRow,
    metadata: &mut ManagedCredentialMaterialization,
    options: &CredentialCleanupOptions,
    removed: bool,
    report: &mut CredentialCleanupReport,
) -> bool {
    let state = state.expect("only applied cleanup closes metadata");
    let failure = match close_metadata(state, row, metadata, "cleaned", options.run_dir.as_deref())
    {
        Ok(true) => return true,
        Ok(false) => "metadata version changed".to_owned(),
        Err(err) => err,
    };
    let status = if removed {
        "file removed; metadata not closed"
    } else {
        "metadata not closed"
    };
    report.errors.push(format!(
        "credential target '{}': {status}: {failure}",
        metadata.target
    ));
    false
}
