#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;

use super::managed::{managed_materialization_content_hash, read_managed_materialization};
use super::plan::{materializer_writes_file, plan_credential_materialization};
use super::safety::is_high_risk_credential_target;
use super::types::{
    CredentialDoctorIssue, CredentialDoctorReport, CredentialDoctorSummary, CredentialProfile,
};
use memory_core::MemoryStore;
use std::fs;
use std::path::{Path, PathBuf};

fn is_secretish_config_key(key: &str) -> bool {
    let normalized = key
        .chars()
        .filter(|ch| ch.is_ascii_alphanumeric())
        .flat_map(|ch| ch.to_lowercase())
        .collect::<String>();
    let lower = key.to_ascii_lowercase();
    let parts = lower
        .split(|ch: char| !(ch == '_' || ch.is_ascii_alphanumeric()))
        .flat_map(|part| part.split('_'))
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>();
    matches!(
        normalized.as_str(),
        "apikey" | "apitoken" | "token" | "accesstoken" | "refreshtoken" | "secret" | "secretkey"
    ) || normalized.ends_with("apikey")
        || normalized.ends_with("token")
        || normalized.ends_with("secret")
        || parts.iter().any(|part| matches!(*part, "token" | "secret"))
        || (parts.contains(&"api") && parts.contains(&"key"))
}

fn is_safe_secret_reference(value: &str) -> bool {
    let trimmed = value.trim();
    trimmed.is_empty()
        || trimmed.starts_with('$')
        || trimmed.starts_with("env:")
        || trimmed.starts_with("vault:")
        || trimmed.starts_with("{{")
}

fn config_contains_plaintext_secret(value: &serde_json::Value) -> bool {
    match value {
        serde_json::Value::Object(map) => map.iter().any(|(key, value)| {
            if is_secretish_config_key(key) {
                if let Some(raw) = value.as_str() {
                    return !is_safe_secret_reference(raw) && raw.trim().len() >= 8;
                }
            }
            config_contains_plaintext_secret(value)
        }),
        serde_json::Value::Array(values) => values.iter().any(config_contains_plaintext_secret),
        _ => false,
    }
}

fn target_json_contains_plaintext_secret(target: &Path) -> Result<bool, String> {
    let raw = fs::read_to_string(target)
        .map_err(|e| format!("read credential config target '{}': {e}", target.display()))?;
    let parsed: serde_json::Value = serde_json::from_str(&raw)
        .map_err(|e| format!("parse credential config target '{}': {e}", target.display()))?;
    Ok(config_contains_plaintext_secret(&parsed))
}

pub(crate) fn doctor_credential_profile(
    profile_name: &str,
    profile: &CredentialProfile,
    consumer: &str,
    store: &MemoryStore,
) -> Result<CredentialDoctorReport, String> {
    let plan = plan_credential_materialization(profile_name, profile, consumer, store)?;
    let mut issues = Vec::new();

    if !plan.allowed {
        issues.push(CredentialDoctorIssue {
            severity: "high".to_string(),
            code: "consumer_denied".to_string(),
            message: format!(
                "consumer '{}' is not allowed by credential profile '{}'",
                consumer, profile_name
            ),
            source: None,
            target: None,
        });
    }

    for step in &plan.steps {
        match step.status.as_str() {
            "missing_secret" => issues.push(CredentialDoctorIssue {
                severity: "high".to_string(),
                code: "missing_secret".to_string(),
                message: format!("Vault secret '{}' is missing", step.resolved_secret),
                source: Some(step.resolved_secret.clone()),
                target: Some(step.target.clone()),
            }),
            "denied_secret" => issues.push(CredentialDoctorIssue {
                severity: "high".to_string(),
                code: "secret_denied_for_consumer".to_string(),
                message: format!(
                    "Vault secret '{}' does not allow consumer '{}'",
                    step.resolved_secret, consumer
                ),
                source: Some(step.resolved_secret.clone()),
                target: Some(step.target.clone()),
            }),
            "unsupported" => issues.push(CredentialDoctorIssue {
                severity: "medium".to_string(),
                code: "unsupported_materializer".to_string(),
                message: format!(
                    "materializer '{}' is not supported by this credential slice",
                    step.materializer_type
                ),
                source: Some(step.resolved_secret.clone()),
                target: Some(step.target.clone()),
            }),
            _ => {}
        }

        if materializer_writes_file(&step.materializer_type, &step.target) {
            let target = PathBuf::from(&step.target);
            let managed = match read_managed_materialization(store, profile_name, consumer, step) {
                Ok(managed) => managed,
                Err(err) => {
                    issues.push(CredentialDoctorIssue {
                        severity: "medium".to_string(),
                        code: "managed_metadata_unreadable".to_string(),
                        message: err,
                        source: Some(step.resolved_secret.clone()),
                        target: Some(step.target.clone()),
                    });
                    None
                }
            };
            if is_high_risk_credential_target(&target) {
                issues.push(CredentialDoctorIssue {
                    severity: "high".to_string(),
                    code: "high_risk_target".to_string(),
                    message: format!(
                        "target '{}' is a broad session/auth path and is rejected by apply",
                        target.display()
                    ),
                    source: Some(step.resolved_secret.clone()),
                    target: Some(step.target.clone()),
                });
            }
            let target_metadata = fs::metadata(&target).ok();
            if target_metadata.is_none() {
                if managed
                    .as_ref()
                    .is_some_and(|managed| managed.cleanup_status.as_deref() != Some("cleaned"))
                {
                    issues.push(CredentialDoctorIssue {
                        severity: "high".to_string(),
                        code: "managed_target_missing".to_string(),
                        message: format!(
                            "target '{}' was previously materialized by Tachi but no longer exists",
                            target.display()
                        ),
                        source: Some(step.resolved_secret.clone()),
                        target: Some(step.target.clone()),
                    });
                }
                continue;
            }
            if let Some(meta) = target_metadata {
                let mut managed_target_readable = true;
                if let Some(managed) = managed
                    .as_ref()
                    .filter(|managed| managed.cleanup_status.as_deref() != Some("cleaned"))
                {
                    match fs::read_to_string(&target) {
                        Ok(current) => {
                            let current_hash = managed_materialization_content_hash(
                                profile_name,
                                consumer,
                                &step.target,
                                &current,
                            );
                            if current_hash != managed.content_hash {
                                issues.push(CredentialDoctorIssue {
                                    severity: "high".to_string(),
                                    code: "managed_target_hash_mismatch".to_string(),
                                    message: format!(
                                        "target '{}' differs from the last Tachi-managed materialization",
                                        target.display()
                                    ),
                                    source: Some(step.resolved_secret.clone()),
                                    target: Some(step.target.clone()),
                                });
                            }
                        }
                        Err(err) => {
                            managed_target_readable = false;
                            issues.push(CredentialDoctorIssue {
                                severity: "medium".to_string(),
                                code: "managed_target_unreadable".to_string(),
                                message: format!(
                                    "read managed credential target '{}': {err}",
                                    target.display()
                                ),
                                source: Some(step.resolved_secret.clone()),
                                target: Some(step.target.clone()),
                            });
                        }
                    }
                } else {
                    issues.push(CredentialDoctorIssue {
                        severity: "medium".to_string(),
                        code: "existing_target".to_string(),
                        message: format!(
                            "target '{}' already exists; apply will require allow_existing and create a backup",
                            target.display()
                        ),
                        source: Some(step.resolved_secret.clone()),
                        target: Some(step.target.clone()),
                    });
                }
                #[cfg(unix)]
                {
                    let mode = meta.permissions().mode() & 0o777;
                    if mode & 0o077 != 0 {
                        issues.push(CredentialDoctorIssue {
                            severity: "high".to_string(),
                            code: "target_permissions_too_broad".to_string(),
                            message: format!(
                                "target '{}' permissions are {:o}; credential files should be 0600",
                                target.display(),
                                mode
                            ),
                            source: Some(step.resolved_secret.clone()),
                            target: Some(step.target.clone()),
                        });
                    }
                }
                if matches!(
                    step.materializer_type.as_str(),
                    "config_overlay" | "config_patch"
                ) && managed_target_readable
                {
                    match target_json_contains_plaintext_secret(&target) {
                        Ok(true) => issues.push(CredentialDoctorIssue {
                            severity: "high".to_string(),
                            code: "plaintext_config_secret".to_string(),
                            message: format!(
                                "target '{}' appears to contain a plaintext secret; prefer env or vault references in generated configs",
                                target.display()
                            ),
                            source: Some(step.resolved_secret.clone()),
                            target: Some(step.target.clone()),
                        }),
                        Ok(false) => {}
                        Err(err) => issues.push(CredentialDoctorIssue {
                            severity: "medium".to_string(),
                            code: "target_config_unreadable".to_string(),
                            message: err,
                            source: Some(step.resolved_secret.clone()),
                            target: Some(step.target.clone()),
                        }),
                    }
                }
            }
        }
    }

    let high_count = issues
        .iter()
        .filter(|issue| issue.severity == "high")
        .count();
    let medium_count = issues
        .iter()
        .filter(|issue| issue.severity == "medium")
        .count();
    Ok(CredentialDoctorReport {
        profile: profile_name.to_string(),
        consumer: consumer.to_string(),
        summary: CredentialDoctorSummary {
            issue_count: issues.len(),
            high_count,
            medium_count,
        },
        issues,
    })
}
