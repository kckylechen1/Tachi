use super::managed::{
    managed_target_current_hash, mark_metadata_cleaned, ManagedCredentialMaterialization,
};
use super::types::{CredentialCleanupOptions, CredentialCleanupReport};
use super::CREDENTIAL_MATERIALIZATION_NAMESPACE;
use memory_core::MemoryStore;
use std::fs;
use std::path::{Component, Path, PathBuf};

pub(crate) fn cleanup_ephemeral_credential_materializations(
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
    if let Some(run_dir) = &options.run_dir {
        let target = PathBuf::from(&metadata.target);
        if target
            .components()
            .any(|component| matches!(component, Component::ParentDir))
        {
            return false;
        }
        if !target.starts_with(run_dir) {
            return false;
        }
    }
    true
}

pub(crate) fn cleanup_managed_credential_materializations(
    store: &MemoryStore,
    options: &CredentialCleanupOptions,
) -> Result<CredentialCleanupReport, String> {
    if options.run_dir.is_none() && options.profile.is_none() && options.consumer.is_none() {
        return Err(
            "credential cleanup requires at least one scope: --run-dir, --profile, or --consumer"
                .to_string(),
        );
    }
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
        if !metadata_matches_cleanup_scope(&metadata, options) {
            report.skipped.push(metadata.target);
            continue;
        }
        let target = PathBuf::from(&metadata.target);
        if !target.exists() {
            if !options.dry_run {
                mark_metadata_cleaned(store, &row.key, &mut metadata, options.run_dir.as_deref())?;
            }
            report.missing.push(target.to_string_lossy().to_string());
            continue;
        }
        if options.mark_only {
            if options.dry_run {
                report.would_mark.push(target.to_string_lossy().to_string());
                continue;
            }
            mark_metadata_cleaned(store, &row.key, &mut metadata, options.run_dir.as_deref())?;
            report.marked.push(target.to_string_lossy().to_string());
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
            report
                .would_remove
                .push(target.to_string_lossy().to_string());
            continue;
        }
        if target.is_dir() {
            report.errors.push(format!(
                "refusing to remove credential target directory '{}'",
                target.display()
            ));
            continue;
        }
        if options.run_dir.is_none() {
            match managed_target_current_hash(&metadata, &target) {
                Ok(current_hash) if current_hash == metadata.content_hash => {}
                Ok(_) => {
                    report.skipped.push(format!(
                        "{} (hash mismatch; use --mark-only after review)",
                        metadata.target
                    ));
                    continue;
                }
                Err(err) => {
                    report.errors.push(err);
                    continue;
                }
            }
        }
        match fs::remove_file(&target) {
            Ok(()) => {
                mark_metadata_cleaned(store, &row.key, &mut metadata, options.run_dir.as_deref())?;
                report.removed.push(target.to_string_lossy().to_string());
            }
            Err(err) => report.errors.push(format!(
                "remove credential target '{}': {err}",
                target.display()
            )),
        }
    }
    Ok(report)
}
