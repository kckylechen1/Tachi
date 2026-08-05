use super::managed::record_managed_materialization;
use super::plan::{is_env_target, plan_credential_materialization_with_run_dir, resolve_source};
use super::render::{render_config_overlay_value, render_config_patch_value};
use super::safety::{ensure_safe_credential_target, write_file_atomic};
use super::types::{CredentialApplyOptions, CredentialApplyResult, CredentialProfile};
use memcore::MemoryStore;
use std::collections::HashMap;
use std::path::{Component, Path, PathBuf};

const OPENCODE_ENV_REF_PREFIX: &str = "{env:";

/// OpenCode consumes `apiKey` from its config as a provider credential.  A
/// persistent config materializer must therefore keep that field as an env
/// reference; writing the decrypted value makes the config a second plaintext
/// custody location.
fn ensure_opencode_config_materializer_keeps_api_key_indirect(
    profile_name: &str,
    profile: &CredentialProfile,
    materializer: &super::types::CredentialMaterializer,
    resolved_target: &str,
    writes_file: bool,
) -> Result<(), String> {
    if !writes_file || !is_opencode_persistent_target(profile_name, profile, resolved_target) {
        return Ok(());
    }

    match materializer.kind.as_str() {
        "file_copy" => Err(
            "OpenCode config materializer refuses file_copy because raw secret copy cannot prove env-ref-only apiKey output"
                .to_string(),
        ),
        "config_patch" | "config_overlay" => {
            let safe = materializer
                .template
                .as_ref()
                .is_some_and(opencode_api_keys_are_indirect);
            if safe {
                Ok(())
            } else {
                Err("OpenCode config materializer refuses plaintext apiKey; use an {env:NAME} apiKey reference and a separate env materializer".to_string())
            }
        }
        _ => Ok(()),
    }
}

fn is_opencode_persistent_target(
    profile_name: &str,
    profile: &CredentialProfile,
    resolved_target: &str,
) -> bool {
    profile_name.to_ascii_lowercase().contains("opencode")
        || profile
            .provider
            .as_deref()
            .is_some_and(|provider| provider.eq_ignore_ascii_case("opencode"))
        || normalized_path_ends_with_opencode_config(resolved_target)
}

fn normalized_path_ends_with_opencode_config(raw: &str) -> bool {
    let mut normalized = PathBuf::new();
    for component in Path::new(raw).components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                normalized.pop();
            }
            other => normalized.push(other.as_os_str()),
        }
    }
    normalized.ends_with(Path::new(".config/opencode/opencode.json"))
}

fn opencode_api_keys_are_indirect(value: &serde_json::Value) -> bool {
    match value {
        serde_json::Value::Array(values) => values.iter().all(opencode_api_keys_are_indirect),
        serde_json::Value::Object(values) => values.iter().all(|(key, value)| {
            if key == "apiKey" {
                value.as_str().is_some_and(is_opencode_env_reference)
            } else {
                opencode_api_keys_are_indirect(value)
            }
        }),
        _ => true,
    }
}

fn ensure_opencode_final_persistent_json_keeps_api_keys_indirect(
    profile_name: &str,
    profile: &CredentialProfile,
    materializer_kind: &str,
    resolved_target: &str,
    rendered: &str,
) -> Result<(), String> {
    if !is_opencode_persistent_target(profile_name, profile, resolved_target) {
        return Ok(());
    }
    let final_json: serde_json::Value = serde_json::from_str(rendered).map_err(|e| {
        format!("OpenCode persistent {materializer_kind} final output is not valid JSON: {e}")
    })?;
    if opencode_api_keys_are_indirect(&final_json) {
        Ok(())
    } else {
        Err(format!(
            "OpenCode persistent {materializer_kind} final JSON contains an apiKey that is not a valid {{env:NAME}} reference"
        ))
    }
}

fn is_opencode_env_reference(value: &str) -> bool {
    value
        .strip_prefix(OPENCODE_ENV_REF_PREFIX)
        .and_then(|rest| rest.strip_suffix('}'))
        .is_some_and(tachi_params::util::is_shell_env_name)
}

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
                ensure_opencode_config_materializer_keeps_api_key_indirect(
                    profile_name,
                    profile,
                    materializer,
                    &report.steps[idx].target,
                    true,
                )?;
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
                ensure_opencode_config_materializer_keeps_api_key_indirect(
                    profile_name,
                    profile,
                    materializer,
                    &report.steps[idx].target,
                    !is_env_target(&report.steps[idx].target),
                )?;
                let rendered = render_config_overlay_value(materializer, value)?;
                if is_env_target(&report.steps[idx].target) {
                    env.insert(report.steps[idx].target.clone(), rendered);
                    report.steps[idx].status = "prepared_config_env".to_string();
                } else {
                    ensure_opencode_final_persistent_json_keeps_api_keys_indirect(
                        profile_name,
                        profile,
                        "config_overlay",
                        &report.steps[idx].target,
                        &rendered,
                    )?;
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
                ensure_opencode_config_materializer_keeps_api_key_indirect(
                    profile_name,
                    profile,
                    materializer,
                    &report.steps[idx].target,
                    true,
                )?;
                let target = PathBuf::from(&report.steps[idx].target);
                ensure_safe_credential_target(&target)?;
                let rendered = render_config_patch_value(materializer, value, &target)?;
                ensure_opencode_final_persistent_json_keeps_api_keys_indirect(
                    profile_name,
                    profile,
                    "config_patch",
                    &report.steps[idx].target,
                    &rendered,
                )?;
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
