use super::managed::record_managed_materialization;
use super::plan::{is_env_target, plan_credential_materialization_with_run_dir, resolve_source};
use super::render::{render_config_overlay_value, render_config_patch_value};
use super::safety::{ensure_safe_credential_target, write_file_atomic};
use super::types::{CredentialApplyOptions, CredentialApplyResult, CredentialProfile};
use memcore::MemoryStore;
use std::collections::HashMap;
use std::path::PathBuf;

pub(crate) fn apply_credential_materialization(
    profile_name: &str,
    profile: &CredentialProfile,
    consumer: &str,
    store: &MemoryStore,
    secret_values: &HashMap<String, String>,
    options: &CredentialApplyOptions,
) -> Result<CredentialApplyResult, String> {
    let mut report = plan_credential_materialization_with_run_dir(
        profile_name,
        profile,
        consumer,
        store,
        options.run_dir.as_deref(),
    )?;
    report.dry_run = false;
    if !report.allowed
        || !report.missing_secrets.is_empty()
        || !report.denied_secrets.is_empty()
        || report.steps.iter().any(|step| step.status != "ready")
    {
        return Err(format!(
            "credential profile '{}' is not ready to apply; inspect dry-run report first",
            profile_name
        ));
    }

    let mut env = HashMap::new();
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
            "file_copy" => {
                let target = PathBuf::from(&report.steps[idx].target);
                write_file_atomic(
                    &target,
                    value,
                    materializer.chmod.as_deref(),
                    options.allow_existing,
                )?;
                record_managed_materialization(
                    store,
                    profile_name,
                    consumer,
                    &report.steps[idx],
                    value,
                )?;
                report.steps[idx].status = "written".to_string();
                report.steps[idx].would_write = false;
                report.steps[idx].applied = true;
            }
            "config_overlay" => {
                let rendered = render_config_overlay_value(materializer, value)?;
                if is_env_target(&report.steps[idx].target) {
                    env.insert(report.steps[idx].target.clone(), rendered);
                    report.steps[idx].status = "prepared_config_env".to_string();
                } else {
                    let target = PathBuf::from(&report.steps[idx].target);
                    write_file_atomic(
                        &target,
                        &rendered,
                        materializer.chmod.as_deref(),
                        options.allow_existing,
                    )?;
                    record_managed_materialization(
                        store,
                        profile_name,
                        consumer,
                        &report.steps[idx],
                        &rendered,
                    )?;
                    report.steps[idx].status = "written".to_string();
                }
                report.steps[idx].would_write = false;
                report.steps[idx].applied = true;
            }
            "config_patch" => {
                let target = PathBuf::from(&report.steps[idx].target);
                ensure_safe_credential_target(&target)?;
                let rendered = render_config_patch_value(materializer, value, &target)?;
                write_file_atomic(
                    &target,
                    &rendered,
                    materializer.chmod.as_deref(),
                    options.allow_existing,
                )?;
                record_managed_materialization(
                    store,
                    profile_name,
                    consumer,
                    &report.steps[idx],
                    &rendered,
                )?;
                report.steps[idx].status = "written".to_string();
                report.steps[idx].would_write = false;
                report.steps[idx].applied = true;
            }
            "config_content_env" => {
                if !is_env_target(&report.steps[idx].target) {
                    return Err(format!(
                        "config_content_env target '{}' must be a shell env name",
                        report.steps[idx].target
                    ));
                }
                let rendered = render_config_overlay_value(materializer, value)?;
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
        &chrono::Utc::now().to_rfc3339(),
        "credential_materialize",
        Some(profile_name),
        true,
        Some(&audit_detail),
    ) {
        eprintln!("WARNING: failed to record credential materialize audit: {err}");
    }

    Ok(CredentialApplyResult { report, env })
}
