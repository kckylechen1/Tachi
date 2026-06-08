//! Credential profile planning for Vault materialization.
//!
//! This first slice intentionally plans and validates materialization only. It
//! does not decrypt Vault values or write auth files.

use memory_core::MemoryStore;
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::collections::HashMap;
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
    #[serde(skip_serializing_if = "Option::is_none")]
    pub chmod: Option<String>,
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
        _ => format!("{kind}:{target}"),
    }
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
            "env" | "file_copy" | "config_overlay"
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
            would_write: false,
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

pub(crate) fn credential_materialize_report_json(
    report: &CredentialMaterializeReport,
) -> serde_json::Value {
    json!(report)
}
