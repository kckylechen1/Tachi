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

use crate::utils::stable_hash;

pub(crate) const CREDENTIAL_MATERIALIZATION_NAMESPACE: &str = "credential_materialization";

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
    pub run_dir: Option<PathBuf>,
}

#[derive(Debug, Clone)]
pub(crate) struct CredentialApplyResult {
    pub report: CredentialMaterializeReport,
    pub env: HashMap<String, String>,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct CredentialCleanupReport {
    pub run_dir: String,
    pub credentials_dir: String,
    pub dry_run: bool,
    pub profile: Option<String>,
    pub consumer: Option<String>,
    pub mark_only: bool,
    pub would_mark: Vec<String>,
    pub marked: Vec<String>,
    pub would_remove: Vec<String>,
    pub removed: Vec<String>,
    pub missing: Vec<String>,
    pub skipped: Vec<String>,
    pub errors: Vec<String>,
}

#[derive(Debug, Clone, Default)]
pub(crate) struct CredentialCleanupOptions {
    pub run_dir: Option<PathBuf>,
    pub profile: Option<String>,
    pub consumer: Option<String>,
    pub dry_run: bool,
    pub mark_only: bool,
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
        "config_patch" => format!("config_patch:{target}"),
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
    matches!(kind, "file_copy" | "config_patch")
        || (kind == "config_overlay" && !is_env_target(target))
}

#[derive(Debug, Clone, Deserialize, Serialize)]
struct ManagedCredentialMaterialization {
    version: u32,
    profile: String,
    consumer: String,
    materializer_type: String,
    source: String,
    resolved_secret: String,
    target: String,
    content_hash: String,
    chmod: Option<String>,
    managed_at: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    cleanup_status: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    cleaned_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    cleanup_run_dir: Option<String>,
}

fn managed_materialization_key(
    profile_name: &str,
    consumer: &str,
    materializer_type: &str,
    resolved_secret: &str,
    target: &str,
) -> String {
    format!(
        "managed:{}",
        stable_hash(&format!(
            "{profile_name}\u{1f}{consumer}\u{1f}{materializer_type}\u{1f}{resolved_secret}\u{1f}{target}"
        ))
    )
}

fn managed_materialization_content_hash(
    profile_name: &str,
    consumer: &str,
    target: &str,
    content: &str,
) -> String {
    format!(
        "stable-fnv1a:{}",
        stable_hash(&format!(
            "{profile_name}\u{1f}{consumer}\u{1f}{target}\u{1f}{content}"
        ))
    )
}

fn record_managed_materialization(
    store: &MemoryStore,
    profile_name: &str,
    consumer: &str,
    step: &CredentialMaterializeStepReport,
    content: &str,
) -> Result<(), String> {
    let key = managed_materialization_key(
        profile_name,
        consumer,
        &step.materializer_type,
        &step.resolved_secret,
        &step.target,
    );
    let metadata = ManagedCredentialMaterialization {
        version: 1,
        profile: profile_name.to_string(),
        consumer: consumer.to_string(),
        materializer_type: step.materializer_type.clone(),
        source: step.source.clone(),
        resolved_secret: step.resolved_secret.clone(),
        target: step.target.clone(),
        content_hash: managed_materialization_content_hash(
            profile_name,
            consumer,
            &step.target,
            content,
        ),
        chmod: step.chmod.clone(),
        managed_at: chrono::Utc::now().to_rfc3339(),
        cleanup_status: None,
        cleaned_at: None,
        cleanup_run_dir: None,
    };
    let value = serde_json::to_string(&metadata)
        .map_err(|e| format!("serialize credential materialization metadata: {e}"))?;
    store
        .set_state(CREDENTIAL_MATERIALIZATION_NAMESPACE, &key, &value)
        .map_err(|e| format!("record credential materialization metadata: {e}"))?;
    Ok(())
}

fn read_managed_materialization(
    store: &MemoryStore,
    profile_name: &str,
    consumer: &str,
    step: &CredentialMaterializeStepReport,
) -> Result<Option<ManagedCredentialMaterialization>, String> {
    let key = managed_materialization_key(
        profile_name,
        consumer,
        &step.materializer_type,
        &step.resolved_secret,
        &step.target,
    );
    let Some((value_json, _version)) = store
        .get_state_kv(CREDENTIAL_MATERIALIZATION_NAMESPACE, &key)
        .map_err(|e| format!("read credential materialization metadata: {e}"))?
    else {
        return Ok(None);
    };
    let metadata = serde_json::from_str(&value_json)
        .map_err(|e| format!("parse credential materialization metadata: {e}"))?;
    Ok(Some(metadata))
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

fn render_template_json(materializer: &CredentialMaterializer, secret: &str) -> serde_json::Value {
    materializer
        .template
        .as_ref()
        .map(|template| render_template_value(template, secret))
        .unwrap_or_else(|| serde_json::Value::String(secret.to_string()))
}

fn merge_json_patch(base: &mut serde_json::Value, patch: serde_json::Value) {
    match (base, patch) {
        (serde_json::Value::Object(base), serde_json::Value::Object(patch)) => {
            for (key, value) in patch {
                if value.is_null() {
                    base.remove(&key);
                } else if let Some(existing) = base.get_mut(&key) {
                    merge_json_patch(existing, value);
                } else {
                    base.insert(key, value);
                }
            }
        }
        (base, patch) => {
            *base = patch;
        }
    }
}

fn render_config_patch_value(
    materializer: &CredentialMaterializer,
    secret: &str,
    target: &Path,
) -> Result<String, String> {
    let patch = render_template_json(materializer, secret);
    if !patch.is_object() {
        return Err(format!(
            "config_patch template for target '{}' must render to a JSON object",
            materializer.target
        ));
    }
    let mut base = if target.exists() {
        let raw = fs::read_to_string(target)
            .map_err(|e| format!("read config_patch target '{}': {e}", target.display()))?;
        serde_json::from_str(&raw)
            .map_err(|e| format!("parse config_patch target '{}': {e}", target.display()))?
    } else {
        serde_json::json!({})
    };
    if !base.is_object() {
        return Err(format!(
            "config_patch target '{}' must contain a JSON object",
            target.display()
        ));
    }
    merge_json_patch(&mut base, patch);
    serde_json::to_string_pretty(&base)
        .map_err(|e| format!("serialize config_patch target '{}': {e}", target.display()))
}

fn expand_target(target: &str, run_dir: Option<&Path>) -> String {
    let mut expanded = display_target(target);
    if let Some(run_dir) = run_dir {
        let run_dir = run_dir.to_string_lossy();
        let credentials_dir = Path::new(run_dir.as_ref())
            .join("credentials")
            .to_string_lossy()
            .to_string();
        expanded = expanded
            .replace("{credentials_dir}", &credentials_dir)
            .replace("{run_dir}", run_dir.as_ref());
    }
    expanded
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
    plan_credential_materialization_with_run_dir(profile_name, profile, consumer, store, None)
}

pub(crate) fn plan_credential_materialization_with_run_dir(
    profile_name: &str,
    profile: &CredentialProfile,
    consumer: &str,
    store: &MemoryStore,
    run_dir: Option<&Path>,
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
        let target = expand_target(&materializer.target, run_dir);
        let known_kind = matches!(
            materializer.kind.as_str(),
            "env" | "file_copy" | "config_overlay" | "config_patch" | "config_content_env"
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

fn is_high_risk_credential_target(path: &Path) -> bool {
    let raw = path.to_string_lossy();
    raw.ends_with("/.claude.json")
        || raw.contains("/.claude/")
        || raw.contains("/.claude-code-router/")
}

fn ensure_safe_credential_target(path: &Path) -> Result<(), String> {
    if is_high_risk_credential_target(path) {
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
    ensure_safe_credential_target(target)?;
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
        if !target.starts_with(run_dir) {
            return false;
        }
    }
    true
}

fn mark_metadata_cleaned(
    store: &MemoryStore,
    row_key: &str,
    metadata: &mut ManagedCredentialMaterialization,
    cleanup_run_dir: Option<&Path>,
) -> Result<(), String> {
    metadata.cleanup_status = Some("cleaned".to_string());
    metadata.cleaned_at = Some(chrono::Utc::now().to_rfc3339());
    metadata.cleanup_run_dir = cleanup_run_dir.map(|run_dir| run_dir.to_string_lossy().to_string());
    let value = serde_json::to_string(metadata)
        .map_err(|e| format!("serialize cleaned credential metadata: {e}"))?;
    store
        .set_state(CREDENTIAL_MATERIALIZATION_NAMESPACE, row_key, &value)
        .map_err(|e| format!("mark credential target cleaned: {e}"))?;
    Ok(())
}

fn managed_target_current_hash(
    metadata: &ManagedCredentialMaterialization,
    target: &Path,
) -> Result<String, String> {
    let current = fs::read_to_string(target)
        .map_err(|e| format!("read managed credential target '{}': {e}", target.display()))?;
    Ok(managed_materialization_content_hash(
        &metadata.profile,
        &metadata.consumer,
        &metadata.target,
        &current,
    ))
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
