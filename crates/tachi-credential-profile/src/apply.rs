use super::plan::{plan_with_resolved_run_dir, resolve_source};
use super::render::render_config_content;
use super::safety::refuse_parent_dir;
use super::types::{CredentialApplyOptions, CredentialApplyResult, CredentialProfile};
use memcore::MemoryStore;
use std::collections::HashMap;
use std::path::Path;

pub fn apply_credential_materialization(
    profile_name: &str,
    profile: &CredentialProfile,
    consumer: &str,
    store: &MemoryStore,
    secret_values: &HashMap<String, String>,
    options: &CredentialApplyOptions,
) -> Result<CredentialApplyResult, String> {
    if let Some(run_dir) = options.run_dir.as_deref() {
        refuse_parent_dir(run_dir, "credential run_dir")?;
    }
    for materializer in &profile.materializers {
        refuse_parent_dir(Path::new(&materializer.target), "credential target")?;
    }
    let (mut report, run_root) = plan_with_resolved_run_dir(
        profile_name,
        profile,
        consumer,
        store,
        options.run_dir.as_deref(),
    )?;
    report.dry_run = false;
    report.ensure_ready()?;

    let mut env = HashMap::new();
    let mut overlays = Vec::new();
    for (idx, materializer) in profile.materializers.iter().enumerate() {
        let resolved_secret = resolve_source(profile, &materializer.source);
        let value = secret_values
            .get(&resolved_secret)
            .ok_or_else(|| format!("missing decrypted value for secret '{resolved_secret}'"))?;
        match materializer.kind.as_str() {
            "env" => {
                env.insert(materializer.target.clone(), value.clone());
                report.steps[idx].status = "prepared_env".to_string();
                report.steps[idx].would_write = false;
                report.steps[idx].applied = true;
            }
            "config_overlay" => {
                let rendered = render_config_content(materializer, value)?;
                overlays.push((idx, rendered));
            }
            "config_content_env" => {
                let rendered = render_config_content(materializer, value)?;
                env.insert(report.steps[idx].target.clone(), rendered);
                report.steps[idx].status = "prepared_config_env".to_string();
                report.steps[idx].would_write = false;
                report.steps[idx].applied = true;
            }
            other => {
                return Err(format!(
                    "unsupported credential materializer type '{other}'"
                ))
            }
        }
    }

    if !overlays.is_empty() {
        #[cfg(unix)]
        publish_overlays(
            store,
            options,
            &run_root.expect("a ready overlay has a run_dir").canonical,
            &mut report,
            &overlays,
        )?;
        #[cfg(not(unix))]
        {
            let _ = run_root;
            return Err("config_overlay file output is unavailable on this platform".into());
        }
    }

    let audit_detail = format!(
        "consumer={consumer}; outputs={}",
        report
            .steps
            .iter()
            .map(|step| format!("{}:{}", step.output, step.status))
            .collect::<Vec<_>>()
            .join(",")
    );
    if let Err(err) = store.vault_insert_audit(
        &jiff::Timestamp::now().to_string(),
        "credential_materialize",
        Some(profile_name),
        true,
        Some(&audit_detail),
    ) {
        eprintln!("WARNING: failed to record credential materialize audit: {err}");
    }

    Ok(CredentialApplyResult { report, env })
}

#[cfg(unix)]
fn publish_overlays(
    store: &MemoryStore,
    options: &CredentialApplyOptions,
    root: &Path,
    report: &mut super::types::CredentialMaterializeReport,
    overlays: &[(usize, String)],
) -> Result<(), String> {
    use super::managed::{
        prepare_managed_materialization, record_managed_materialization,
        supersede_colliding_materializations, ManagedCredentialMaterialization, PUBLICATION_PREFIX,
    };
    use super::safety::{
        credential_file_mode,
        unix::{PublishedRunFile, RunWriter},
    };
    use super::CREDENTIAL_MATERIALIZATION_NAMESPACE as NS;
    use memcore::MemoryError;

    struct PreparedOverlay {
        metadata: ManagedCredentialMaterialization,
        mode: u32,
        file: PublishedRunFile,
        intents: [String; 2],
        backup: Option<String>,
    }

    fn undo_overlays(
        prepared: &mut Vec<PreparedOverlay>,
        cleared: &mut Vec<String>,
        errors: &mut Vec<String>,
    ) {
        for overlay in prepared.drain(..).rev() {
            match overlay.file.undo() {
                Ok(()) => cleared.extend(overlay.intents.into_iter().chain(overlay.backup)),
                Err(err) => errors.push(err.to_string()),
            }
        }
    }

    let prepared = store.with_state_transaction(|state| {
        let stage = || -> Result<Vec<_>, String> {
            let writer = RunWriter::open(root)?;
            let mut prepared = Vec::new();
            let mut previous_overlays = HashMap::new();
            for (idx, rendered) in overlays {
                let step = &report.steps[*idx];
                let mode = credential_file_mode(step.chmod.as_deref())?;
                let mut metadata = prepare_managed_materialization(
                    &report.profile,
                    &report.consumer,
                    step,
                    rendered,
                    root,
                    writer.identity,
                    options.run_generation.as_deref(),
                );
                let (_, relative) = metadata.bound_target().expect("new overlay is bound");
                let from_same_batch = previous_overlays.contains_key(relative);
                let file = writer.stage_file(
                    relative,
                    options.allow_existing,
                    previous_overlays.get(relative).copied(),
                )?;
                let restore_rel =
                    (!from_same_batch).then(|| relative.to_string_lossy().into_owned());
                let temporary = relative.with_file_name(file.temporary_name());
                let backup = file.backup_name().map(|name| relative.with_file_name(name));
                let (dev, ino) = file.identity().map_err(|err| err.to_string())?;
                if options.allow_existing {
                    previous_overlays.insert(relative.to_path_buf(), (dev, ino));
                }
                metadata.target_dev = Some(dev);
                metadata.target_ino = Some(ino);

                // Commit ownership for every name that may hold plaintext before
                // writing even the staging file. The old active row remains intact
                // until the whole profile is published.
                let intent = format!("{PUBLICATION_PREFIX}{}", uuid::Uuid::new_v4());
                let temp_key = format!("{intent}:stage");
                record_managed_materialization(state, &intent, &metadata)?;
                let mut auxiliary = metadata.clone();
                auxiliary.target = root.join(&temporary).to_string_lossy().into_owned();
                auxiliary.target_rel = Some(temporary.to_string_lossy().into_owned());
                record_managed_materialization(state, &temp_key, &auxiliary)?;
                let backup_key = if let Some((backup, (dev, ino))) = backup.zip(file.original) {
                    let key = format!("{intent}:backup");
                    auxiliary.target = root.join(&backup).to_string_lossy().into_owned();
                    auxiliary.target_rel = Some(backup.to_string_lossy().into_owned());
                    auxiliary.restore_rel = restore_rel;
                    auxiliary.target_dev = Some(dev);
                    auxiliary.target_ino = Some(ino);
                    record_managed_materialization(state, &key, &auxiliary)?;
                    Some(key)
                } else {
                    None
                };
                prepared.push(PreparedOverlay {
                    metadata,
                    mode,
                    file,
                    intents: [intent, temp_key],
                    backup: backup_key,
                });
            }
            Ok(prepared)
        };
        stage().map_err(MemoryError::Vault)
    });
    let mut prepared = prepared.map_err(|err| err.to_string())?;

    let mut cleared = Vec::new();
    let mut file_errors = Vec::new();
    let result = store.with_state_transaction(|state| {
        let publish = (|| {
            for (overlay, (_, content)) in prepared.iter_mut().zip(overlays) {
                for key in overlay.intents.iter().chain(overlay.backup.iter()) {
                    if !matches!(state.get_state(NS, key)?, Some((_, 1))) {
                        return Err(MemoryError::Vault(
                            "credential publication was interrupted by cleanup".into(),
                        ));
                    }
                }
                let metadata = &overlay.metadata;
                #[cfg(test)]
                BEFORE_OVERLAY_WRITE.with(|hook| {
                    if let Some(before_write) = hook.take() {
                        before_write(root);
                    }
                });
                let (root_identity, relative) =
                    metadata.bound_target().expect("new overlay is bound");
                overlay
                    .file
                    .write_file(root, root_identity, relative, content, overlay.mode)
                    .map_err(MemoryError::Vault)?;
                #[cfg(test)]
                AFTER_OVERLAY_WRITE.with(|hook| {
                    if let Some(after_write) = hook.take() {
                        after_write(root);
                    }
                });
                supersede_colliding_materializations(state, metadata, options.allow_existing)
                    .map_err(MemoryError::Vault)?;
                record_managed_materialization(state, &metadata.key(), metadata)
                    .map_err(MemoryError::Vault)?;
                for key in &overlay.intents {
                    state.delete_state(NS, key)?;
                }
            }
            Ok(())
        })();
        if publish.is_err() {
            undo_overlays(&mut prepared, &mut cleared, &mut file_errors);
        }
        publish
    });

    // Leave the durable intent in place wherever rollback or backup removal
    // cannot be confirmed.
    if !prepared.is_empty() || !cleared.is_empty() {
        let finish = store.with_state_transaction(|state| {
            if result.is_ok() {
                for overlay in prepared.drain(..) {
                    match overlay.file.commit() {
                        Ok(()) => cleared.extend(overlay.backup),
                        Err(err) => file_errors.push(err.to_string()),
                    }
                }
            } else {
                // COMMIT may itself have failed after the closure succeeded.
                undo_overlays(&mut prepared, &mut cleared, &mut file_errors);
            }
            for key in &cleared {
                state.delete_state(NS, key)?;
            }
            Ok(())
        });
        if let Err(err) = finish {
            file_errors.push(err.to_string());
        }
    }

    let finish_error = (!file_errors.is_empty()).then(|| file_errors.join("; "));
    if let Err(err) = result {
        return Err(match finish_error {
            Some(cleanup) => {
                format!("{err}; credential publication rollback incomplete: {cleanup}")
            }
            None => err.to_string(),
        });
    }
    if let Some(err) = finish_error {
        eprintln!("WARNING: credential backup remains tracked for cleanup: {err}");
    }
    for (idx, _) in overlays {
        let step = &mut report.steps[*idx];
        step.status = "written".to_string();
        step.would_write = false;
        step.applied = true;
    }
    Ok(())
}

#[cfg(all(test, unix))]
thread_local! {
    pub(crate) static BEFORE_OVERLAY_WRITE: std::cell::Cell<Option<fn(&Path)>> =
        const { std::cell::Cell::new(None) };
    pub(crate) static AFTER_OVERLAY_WRITE: std::cell::Cell<Option<fn(&Path)>> =
        const { std::cell::Cell::new(None) };
}
