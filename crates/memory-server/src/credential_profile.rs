//! Credential profile planning and application for Vault materialization.
//!
//! Reports remain redacted, while apply can prepare child-process env values and
//! write guarded credential/config files after the caller supplies decrypted
//! Vault values.

use memory_core::MemoryStore;
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::collections::HashMap;
use std::fs;
use std::io::Write;
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct CredentialProfileDocument {
    pub credential_profiles: HashMap<String, CredentialProfile>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct CredentialProfile {
    #[serde(default)]
    pub provider: Option<String>,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub entries: HashMap<String, String>,
    #[serde(default)]
    pub allowed_consumers: AllowedConsumers,
    #[serde(default)]
    pub materializers: Vec<CredentialMaterializer>,
}

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct AllowedConsumers {
    #[serde(default)]
    pub agents: Vec<String>,
    #[serde(default)]
    pub profiles: Vec<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct CredentialMaterializer {
    #[serde(rename = "type")]
    pub kind: String,
    pub source: String,
    pub target: String,
    #[serde(default)]
    pub chmod: Option<String>,
    #[serde(default)]
    pub template: Option<serde_json::Value>,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct CredentialMaterializeReport {
    pub profile: String,
    pub consumer: String,
    pub dry_run: bool,
    pub provider: Option<String>,
    pub allowed: bool,
    pub steps: Vec<CredentialMaterializeStepReport>,
    pub missing_secrets: Vec<String>,
    pub denied_secrets: Vec<String>,
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct CredentialMaterializeStepReport {
    pub materializer_type: String,
    pub source: String,
    pub resolved_secret: String,
    pub target: String,
    pub output: String,
    pub status: String,
    pub redacted: bool,
    pub would_write: bool,
    pub applied: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub chmod: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct CredentialDoctorReport {
    pub profile: String,
    pub consumer: String,
    pub issues: Vec<CredentialDoctorIssue>,
    pub summary: CredentialDoctorSummary,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct CredentialDoctorIssue {
    pub severity: String,
    pub code: String,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub target: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct CredentialDoctorSummary {
    pub issue_count: usize,
    pub high_count: usize,
    pub medium_count: usize,
}

#[derive(Debug, Clone, Default)]
pub(crate) struct CredentialApplyOptions {
    pub allow_existing: bool,
}

#[derive(Debug, Clone)]
pub(crate) struct CredentialApplyResult {
    pub report: CredentialMaterializeReport,
    pub env: HashMap<String, String>,
}

pub(crate) fn default_credentials_dir() -> PathBuf {
    PathBuf::from(".tachi").join("credentials")
}

pub(crate) fn load_credential_profile_from_path(
    path: &Path,
    profile_name: &str,
) -> Result<CredentialProfile, String> {
    let raw = std::fs::read_to_string(path)
        .map_err(|e| format!("read credential profile config '{}': {e}", path.display()))?;
    let doc: CredentialProfileDocument = serde_json::from_str(&raw)
        .map_err(|e| format!("parse credential profile config '{}': {e}", path.display()))?;
    doc.credential_profiles
        .get(profile_name)
        .cloned()
        .ok_or_else(|| {
            format!(
                "Credential profile '{profile_name}' not found in {}",
                path.display()
            )
        })
}

pub(crate) fn find_credential_profile(
    credentials_dir: &Path,
    profile_name: &str,
) -> Result<(PathBuf, CredentialProfile), String> {
    let entries = std::fs::read_dir(credentials_dir).map_err(|e| {
        format!(
            "read credential profile directory '{}': {e}",
            credentials_dir.display()
        )
    })?;
    let mut skipped_invalid_configs = Vec::new();
    for entry in entries {
        let path = entry
            .map_err(|e| format!("read credential profile directory entry: {e}"))?
            .path();
        if path.extension().and_then(|ext| ext.to_str()) != Some("json") {
            continue;
        }
        let raw = match std::fs::read_to_string(&path) {
            Ok(raw) => raw,
            Err(err) => {
                skipped_invalid_configs.push(format!("{} ({err})", path.display()));
                continue;
            }
        };
        let doc: CredentialProfileDocument = match serde_json::from_str(&raw) {
            Ok(doc) => doc,
            Err(err) => {
                skipped_invalid_configs.push(format!("{} ({err})", path.display()));
                continue;
            }
        };
        if let Some(profile) = doc.credential_profiles.get(profile_name).cloned() {
            return Ok((path, profile));
        }
    }
    let mut err = format!(
        "Credential profile '{profile_name}' not found under {}",
        credentials_dir.display()
    );
    if !skipped_invalid_configs.is_empty() {
        err.push_str(&format!(
            "; skipped invalid configs: {}",
            skipped_invalid_configs.join(", ")
        ));
    }
    Err(err)
}

fn consumer_allowed(allowed: &AllowedConsumers, consumer: &str) -> bool {
    if allowed.agents.is_empty() && allowed.profiles.is_empty() {
        return true;
    }
    allowed.agents.iter().any(|agent| agent == consumer)
        || allowed.profiles.iter().any(|profile| profile == consumer)
}

fn entry_allows_consumer(entry_allowed_agents: Option<&[String]>, consumer: &str) -> bool {
    entry_allowed_agents
        .map(|agents| agents.iter().any(|agent| agent == consumer))
        .unwrap_or(true)
}

fn resolve_source(profile: &CredentialProfile, source: &str) -> String {
    profile
        .entries
        .get(source)
        .cloned()
        .unwrap_or_else(|| source.to_string())
}

fn display_target(target: &str) -> String {
    if let Some(rest) = target.strip_prefix("~/") {
        if let Some(home) = dirs::home_dir() {
            return home.join(rest).to_string_lossy().to_string();
        }
    }
    target.to_string()
}

fn materializer_output(kind: &str, target: &str) -> String {
    match kind {
        "env" => format!("env:{target}"),
        "file_copy" => format!("file:{target}"),
        "config_overlay" => format!("config_overlay:{target}"),
        "config_content_env" => format!("config_content_env:{target}"),
        _ => format!("{kind}:{target}"),
    }
}

fn is_env_target(target: &str) -> bool {
    let mut chars = target.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    (first == '_' || first.is_ascii_alphabetic())
        && chars.all(|ch| ch == '_' || ch.is_ascii_alphanumeric())
}

fn materializer_writes_file(kind: &str, target: &str) -> bool {
    matches!(kind, "file_copy") || (kind == "config_overlay" && !is_env_target(target))
}

fn render_template_value(template: &serde_json::Value, secret: &str) -> serde_json::Value {
    match template {
        serde_json::Value::String(text) => serde_json::Value::String(
            text.replace("{{secret}}", secret)
                .replace("{{value}}", secret),
        ),
        serde_json::Value::Array(values) => serde_json::Value::Array(
            values
                .iter()
                .map(|value| render_template_value(value, secret))
                .collect(),
        ),
        serde_json::Value::Object(map) => serde_json::Value::Object(
            map.iter()
                .map(|(key, value)| (key.clone(), render_template_value(value, secret)))
                .collect(),
        ),
        other => other.clone(),
    }
}

fn render_config_overlay_value(
    materializer: &CredentialMaterializer,
    secret: &str,
) -> Result<String, String> {
    let Some(template) = materializer.template.as_ref() else {
        return Ok(secret.to_string());
    };
    let rendered = render_template_value(template, secret);
    serde_json::to_string(&rendered).map_err(|e| {
        format!(
            "serialize config_overlay template for target '{}': {e}",
            materializer.target
        )
    })
}

pub(crate) fn profile_secret_names(profile: &CredentialProfile) -> Vec<String> {
    let mut names = profile
        .materializers
        .iter()
        .map(|materializer| resolve_source(profile, &materializer.source))
        .collect::<Vec<_>>();
    names.sort();
    names.dedup();
    names
}

pub(crate) fn plan_credential_materialization(
    profile_name: &str,
    profile: &CredentialProfile,
    consumer: &str,
    store: &MemoryStore,
) -> Result<CredentialMaterializeReport, String> {
    let entries = store
        .vault_list_entries()
        .map_err(|e| format!("vault_list_entries: {e}"))?
        .into_iter()
        .map(|entry| (entry.name.clone(), entry))
        .collect::<HashMap<_, _>>();

    let allowed = consumer_allowed(&profile.allowed_consumers, consumer);
    let mut missing_secrets = Vec::new();
    let mut denied_secrets = Vec::new();
    let mut warnings = Vec::new();
    let mut steps = Vec::new();

    if !allowed {
        warnings.push(format!(
            "consumer '{consumer}' is not allowed by credential profile '{profile_name}'"
        ));
    }

    for materializer in &profile.materializers {
        let resolved_secret = resolve_source(profile, &materializer.source);
        let target = display_target(&materializer.target);
        let known_kind = matches!(
            materializer.kind.as_str(),
            "env" | "file_copy" | "config_overlay" | "config_content_env"
        );
        if !known_kind {
            warnings.push(format!(
                "unsupported materializer type '{}' for target '{}'",
                materializer.kind, materializer.target
            ));
        }

        let status = match entries.get(&resolved_secret) {
            None => {
                if !missing_secrets.contains(&resolved_secret) {
                    missing_secrets.push(resolved_secret.clone());
                }
                "missing_secret"
            }
            Some(entry) if !entry_allows_consumer(entry.allowed_agents.as_deref(), consumer) => {
                if !denied_secrets.contains(&resolved_secret) {
                    denied_secrets.push(resolved_secret.clone());
                }
                "denied_secret"
            }
            Some(_) if !allowed => "denied_consumer",
            Some(_) if !known_kind => "unsupported",
            Some(_) => "ready",
        };

        steps.push(CredentialMaterializeStepReport {
            materializer_type: materializer.kind.clone(),
            source: materializer.source.clone(),
            resolved_secret,
            target: target.clone(),
            output: materializer_output(&materializer.kind, &target),
            status: status.to_string(),
            redacted: true,
            would_write: materializer_writes_file(&materializer.kind, &target),
            applied: false,
            chmod: materializer.chmod.clone(),
        });
    }

    Ok(CredentialMaterializeReport {
        profile: profile_name.to_string(),
        consumer: consumer.to_string(),
        dry_run: true,
        provider: profile.provider.clone(),
        allowed,
        steps,
        missing_secrets,
        denied_secrets,
        warnings,
    })
}

fn mode_from_chmod(chmod: Option<&str>) -> Result<u32, String> {
    let raw = chmod.unwrap_or("0600");
    u32::from_str_radix(raw, 8).map_err(|e| format!("invalid chmod '{raw}': {e}"))
}

fn is_high_risk_file_copy_target(path: &Path) -> bool {
    let raw = path.to_string_lossy();
    raw.ends_with("/.claude.json")
        || raw.contains("/.claude/")
        || raw.contains("/.claude-code-router/")
}

fn ensure_safe_file_copy_target(path: &Path) -> Result<(), String> {
    if is_high_risk_file_copy_target(path) {
        return Err(format!(
            "refusing high-risk credential target '{}'; use a narrower generated credential path",
            path.display()
        ));
    }
    Ok(())
}

fn write_file_atomic(
    target: &Path,
    value: &str,
    chmod: Option<&str>,
    allow_existing: bool,
) -> Result<(), String> {
    ensure_safe_file_copy_target(target)?;
    if target.exists() && !allow_existing {
        return Err(format!(
            "target '{}' already exists; rerun with allow_existing after reviewing backup policy",
            target.display()
        ));
    }
    if let Some(parent) = target.parent() {
        fs::create_dir_all(parent).map_err(|e| format!("create '{}': {e}", parent.display()))?;
    }

    if target.exists() {
        let backup = target.with_extension(format!(
            "{}.tachi-bak-{}",
            target
                .extension()
                .and_then(|ext| ext.to_str())
                .unwrap_or("bak"),
            chrono::Utc::now().timestamp()
        ));
        fs::copy(target, &backup).map_err(|e| {
            format!(
                "backup existing target '{}' to '{}': {e}",
                target.display(),
                backup.display()
            )
        })?;
    }

    let temp = target.with_extension(format!(
        "{}.tachi-tmp-{}",
        target
            .extension()
            .and_then(|ext| ext.to_str())
            .unwrap_or("tmp"),
        uuid::Uuid::new_v4().as_simple()
    ));
    {
        let mut file = fs::OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&temp)
            .map_err(|e| format!("create temp credential file '{}': {e}", temp.display()))?;
        file.write_all(value.as_bytes())
            .map_err(|e| format!("write temp credential file '{}': {e}", temp.display()))?;
        file.sync_all()
            .map_err(|e| format!("sync temp credential file '{}': {e}", temp.display()))?;
    }

    #[cfg(unix)]
    {
        let mode = mode_from_chmod(chmod)?;
        fs::set_permissions(&temp, fs::Permissions::from_mode(mode))
            .map_err(|e| format!("chmod temp credential file '{}': {e}", temp.display()))?;
    }

    fs::rename(&temp, target).map_err(|e| {
        let _ = fs::remove_file(&temp);
        format!(
            "move temp credential file '{}' to '{}': {e}",
            temp.display(),
            target.display()
        )
    })?;
    Ok(())
}

pub(crate) fn apply_credential_materialization(
    profile_name: &str,
    profile: &CredentialProfile,
    consumer: &str,
    store: &MemoryStore,
    secret_values: &HashMap<String, String>,
    options: &CredentialApplyOptions,
) -> Result<CredentialApplyResult, String> {
    let mut report = plan_credential_materialization(profile_name, profile, consumer, store)?;
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
                    report.steps[idx].status = "written".to_string();
                }
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

pub(crate) fn credential_materialize_report_json(
    report: &CredentialMaterializeReport,
) -> serde_json::Value {
    json!(report)
}

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
            if is_high_risk_file_copy_target(&target) {
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
            if let Ok(meta) = fs::metadata(&target) {
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
                if step.materializer_type == "config_overlay" {
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
