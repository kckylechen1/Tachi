//! Report-only `tachi vault doctor --providers` view.
//!
//! Reads OpenCode config, classifies each provider's apiKey shape without
//! printing secret values, and joins against Vault metadata. With an explicit
//! password source, the caller may supply a verified key for report-only
//! decrypt + in-memory comparison; no Vault or provider state is mutated.

use blake2::{Blake2s256, Digest};
use memcore::vault::{VaultKeyHealth, SECRET_TYPE_API_KEY};
use memcore::MemoryStore;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};

const ENV_REF_PREFIX: &str = "{env:";
const ENV_REF_SUFFIX: &str = "}";

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum ApiKeyShape {
    /// `{env:NAME}` — name is not a secret; print literally.
    EnvRef { name: String },
    /// Non-empty literal string — print only a constant marker.
    Literal,
    /// Missing, null, empty, or non-string.
    Absent,
}

impl ApiKeyShape {
    pub(super) fn display(&self) -> String {
        match self {
            Self::EnvRef { name } => format!("{ENV_REF_PREFIX}{name}{ENV_REF_SUFFIX}"),
            Self::Literal => "LITERAL(redacted)".to_string(),
            Self::Absent => "absent".to_string(),
        }
    }

    pub(super) fn env_name(&self) -> Option<&str> {
        match self {
            Self::EnvRef { name } => Some(name.as_str()),
            Self::Literal | Self::Absent => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct ProviderDoctorRow {
    pub provider: String,
    pub shape: ApiKeyShape,
    pub env_ref: Option<String>,
    pub admitted: Option<bool>,
    pub vault: Option<bool>,
    pub vault_rotation: bool,
    pub age_days: Option<u64>,
    pub age_basis: Option<String>,
    pub value_match: String,
    pub daemon_effective_source: String,
    /// This CLI cannot observe a daemon's live provider cache/LKG state or an
    /// external OAuth store, so completeness is never promoted to CLEAN.
    pub source_completeness: String,
}

impl ProviderDoctorRow {
    pub(super) fn format_line(&self) -> String {
        let admitted = match self.admitted {
            Some(true) => "yes",
            Some(false) => "no",
            None => "n/a",
        };
        let vault = match (self.vault, self.vault_rotation) {
            (Some(true), true) => "yes (rotation)",
            (Some(true), false) => "yes",
            (Some(false), _) => "no",
            (None, _) => "n/a",
        };
        let age = match self.age_days {
            Some(days) => days.to_string(),
            None => "n/a".to_string(),
        };
        let env_ref = self.env_ref.as_deref().unwrap_or("n/a");
        let age_basis = self
            .age_basis
            .as_deref()
            .map(|basis| format!(" · age_basis={basis}"))
            .unwrap_or_default();
        format!(
            "{} · {} · env_ref={} · admitted={} · vault={} · age_days={}{} · value_match={} · daemon_effective_source={} · source_completeness={}",
            self.provider,
            self.shape.display(),
            env_ref,
            admitted,
            vault,
            age,
            age_basis,
            self.value_match,
            self.daemon_effective_source,
            self.source_completeness,
        )
    }
}

#[derive(Default)]
struct RotationReportEvidence {
    configured_members: usize,
    present_configured_members: usize,
    active_members: usize,
    unreadable_members: usize,
    values: Vec<String>,
    oldest_reportable_updated_at: Option<String>,
    age_basis: Option<String>,
}

impl Drop for RotationReportEvidence {
    fn drop(&mut self) {
        for value in &mut self.values {
            crate::vault_crypto::zero_string(value);
        }
    }
}

#[derive(Default)]
struct ProviderVaultReportEvidence {
    exact_updated_at: HashMap<String, String>,
    exact_values: HashMap<String, String>,
    rotations: HashMap<String, RotationReportEvidence>,
}

impl ProviderVaultReportEvidence {
    #[cfg(test)]
    fn exact_only(updated_at: &HashMap<String, String>, values: &HashMap<String, String>) -> Self {
        Self {
            exact_updated_at: updated_at.clone(),
            exact_values: values.clone(),
            rotations: HashMap::new(),
        }
    }
}

impl Drop for ProviderVaultReportEvidence {
    fn drop(&mut self) {
        for value in self.exact_values.values_mut() {
            crate::vault_crypto::zero_string(value);
        }
    }
}

pub(super) fn default_opencode_config_path() -> PathBuf {
    dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".config/opencode/opencode.json")
}

/// Resolve config path: CLI `--opencode-config` > env `TACHI_OPENCODE_CONFIG` >
/// `~/.config/opencode/opencode.json`.
pub(super) fn resolve_opencode_config_path(cli: Option<PathBuf>) -> PathBuf {
    if let Some(path) = cli {
        return path;
    }
    if let Ok(path) = std::env::var("TACHI_OPENCODE_CONFIG") {
        let trimmed = path.trim();
        if !trimmed.is_empty() {
            return PathBuf::from(trimmed);
        }
    }
    default_opencode_config_path()
}

/// Classify an apiKey JSON value. Never returns the raw secret for literals.
pub(super) fn classify_api_key_value(value: Option<&serde_json::Value>) -> ApiKeyShape {
    let Some(value) = value else {
        return ApiKeyShape::Absent;
    };
    match value {
        serde_json::Value::Null => ApiKeyShape::Absent,
        serde_json::Value::String(raw) => {
            let trimmed = raw.trim();
            if trimmed.is_empty() {
                return ApiKeyShape::Absent;
            }
            if let Some(name) = parse_env_ref(trimmed) {
                return ApiKeyShape::EnvRef { name };
            }
            ApiKeyShape::Literal
        }
        _ => ApiKeyShape::Absent,
    }
}

fn parse_env_ref(value: &str) -> Option<String> {
    if !value.starts_with(ENV_REF_PREFIX) || !value.ends_with(ENV_REF_SUFFIX) {
        return None;
    }
    let inner = &value[ENV_REF_PREFIX.len()..value.len() - ENV_REF_SUFFIX.len()];
    if inner.is_empty()
        || inner.contains('{')
        || inner.contains('}')
        || !crate::utils::is_shell_env_name(inner)
    {
        return None;
    }
    Some(inner.to_string())
}

fn validate_report_safe_provider(name: &str, block: &serde_json::Value) -> Result<(), String> {
    if !is_report_safe_provider_id(name) {
        return Err(
            "OpenCode provider name is not a report-safe ASCII provider identifier".to_string(),
        );
    }
    let Some(raw) = extract_api_key_value(block).and_then(serde_json::Value::as_str) else {
        return Ok(());
    };
    let trimmed = raw.trim();
    if trimmed.starts_with(ENV_REF_PREFIX) && parse_env_ref(trimmed).is_none() {
        return Err(
            "OpenCode provider apiKey contains an invalid environment variable name".to_string(),
        );
    }
    Ok(())
}

fn is_report_safe_provider_id(name: &str) -> bool {
    let mut bytes = name.bytes();
    let Some(first) = bytes.next() else {
        return false;
    };
    first.is_ascii_alphanumeric()
        && bytes.all(|byte| {
            byte.is_ascii_alphanumeric()
                || matches!(byte, b'.' | b'_' | b'-' | b'/' | b':' | b'@' | b'+')
        })
}

/// Prefer `options.apiKey`, else top-level `apiKey`.
pub(super) fn extract_api_key_value(block: &serde_json::Value) -> Option<&serde_json::Value> {
    let obj = block.as_object()?;
    if let Some(options) = obj.get("options").and_then(|v| v.as_object()) {
        if options.contains_key("apiKey") {
            return options.get("apiKey");
        }
    }
    obj.get("apiKey")
}

pub(super) fn age_days_from_updated_at(
    updated_at: &str,
    now: chrono::DateTime<chrono::Utc>,
) -> Option<u64> {
    let parsed = chrono::DateTime::parse_from_rfc3339(updated_at).ok()?;
    let then = parsed.with_timezone(&chrono::Utc);
    let delta = now.signed_duration_since(then);
    if delta.num_seconds() < 0 {
        return Some(0);
    }
    Some(delta.num_days().max(0) as u64)
}

/// Load and validate OpenCode config; return provider name → block map sorted.
pub(super) fn load_provider_blocks(
    config_path: &Path,
) -> Result<BTreeMap<String, serde_json::Value>, String> {
    if !config_path.exists() {
        return Err(format!(
            "opencode config not found: {}",
            config_path.display()
        ));
    }
    let raw = fs::read_to_string(config_path).map_err(|e| {
        format!(
            "failed to read opencode config '{}': {e}",
            config_path.display()
        )
    })?;
    let parsed: serde_json::Value = serde_json::from_str(&raw).map_err(|e| {
        format!(
            "invalid JSON in opencode config '{}': {e}",
            config_path.display()
        )
    })?;
    let provider = parsed.get("provider").ok_or_else(|| {
        format!(
            "opencode config '{}' missing top-level 'provider' object",
            config_path.display()
        )
    })?;
    let provider_obj = provider.as_object().ok_or_else(|| {
        format!(
            "opencode config '{}' field 'provider' must be a JSON object",
            config_path.display()
        )
    })?;
    let mut out = BTreeMap::new();
    for (name, block) in provider_obj {
        validate_report_safe_provider(name, block)?;
        out.insert(name.clone(), block.clone());
    }
    Ok(out)
}

#[cfg(test)]
pub(super) fn build_provider_rows(
    providers: &BTreeMap<String, serde_json::Value>,
    admitted: &HashSet<String>,
    vault_updated_at: &HashMap<String, String>,
    now: chrono::DateTime<chrono::Utc>,
) -> Vec<ProviderDoctorRow> {
    build_provider_rows_with_sources(
        providers,
        admitted,
        vault_updated_at,
        &HashMap::new(),
        &HashMap::new(),
        ProviderVaultAccess::LockedOrUnavailable,
        now,
    )
    .expect("metadata-only provider rows")
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ProviderVaultAccess {
    LockedOrUnavailable,
    Unlocked,
}

#[cfg(test)]
pub(super) fn build_provider_rows_with_sources(
    providers: &BTreeMap<String, serde_json::Value>,
    admitted: &HashSet<String>,
    vault_updated_at: &HashMap<String, String>,
    env_values: &HashMap<String, String>,
    vault_values: &HashMap<String, String>,
    vault_access: ProviderVaultAccess,
    now: chrono::DateTime<chrono::Utc>,
) -> Result<Vec<ProviderDoctorRow>, String> {
    let evidence = ProviderVaultReportEvidence::exact_only(vault_updated_at, vault_values);
    build_provider_rows_with_evidence(
        providers,
        admitted,
        env_values,
        &evidence,
        vault_access,
        now,
    )
}

fn build_provider_rows_with_evidence(
    providers: &BTreeMap<String, serde_json::Value>,
    admitted: &HashSet<String>,
    env_values: &HashMap<String, String>,
    evidence: &ProviderVaultReportEvidence,
    vault_access: ProviderVaultAccess,
    now: chrono::DateTime<chrono::Utc>,
) -> Result<Vec<ProviderDoctorRow>, String> {
    providers
        .iter()
        .map(|(provider, block)| {
            let shape = classify_api_key_value(extract_api_key_value(block));
            let Some(env_name) = shape.env_name().map(str::to_string) else {
                let (value_match, daemon_effective_source) = match shape {
                    ApiKeyShape::Literal => (
                        "UNKNOWN(literal value is redacted and not compared)",
                        "UNKNOWN(config literal is outside the daemon env-ref contract)",
                    ),
                    ApiKeyShape::Absent => (
                        "UNKNOWN(no apiKey value to compare)",
                        "UNKNOWN(no config apiKey; live cache not observable)",
                    ),
                    ApiKeyShape::EnvRef { .. } => unreachable!("env ref handled above"),
                };
                return Ok(ProviderDoctorRow {
                    provider: provider.clone(),
                    shape,
                    env_ref: None,
                    admitted: None,
                    vault: None,
                    vault_rotation: false,
                    age_days: None,
                    age_basis: None,
                    value_match: value_match.to_string(),
                    daemon_effective_source: daemon_effective_source.to_string(),
                    source_completeness:
                        "INCOMPLETE(external OAuth and live daemon cache not observable)"
                            .to_string(),
                });
            };

            let env_value = env_values.get(&env_name).map(|value| value.trim());
            let vault_lookup_name = vault_lookup_name_for_env(&env_name, env_values)?;
            let alias_target = env_value
                .is_some_and(|value| value.starts_with(tachi_llm::VAULT_ALIAS_PREFIX))
                .then_some(vault_lookup_name.as_str());
            let updated_at = evidence.exact_updated_at.get(&vault_lookup_name);
            let vault_present = updated_at.is_some();
            let vault_value = evidence.exact_values.get(&vault_lookup_name);
            let rotation = evidence.rotations.get(&vault_lookup_name);

            if let Some(rotation) = rotation {
                let has_active_pool = rotation.active_members > 0
                    && rotation.unreadable_members == 0
                    && !rotation.values.is_empty();
                let all_configured_present = rotation.configured_members > 0
                    && rotation.present_configured_members == rotation.configured_members;
                let member_set = if all_configured_present {
                    "complete"
                } else {
                    "partial"
                };
                let rotation_completeness = format!(
                    "INCOMPLETE(rotation member set {member_set}; configured_members={} present_configured_members={} active_members={} unreadable_members={}; external OAuth, live health, and daemon LKG cache not observable)",
                    rotation.configured_members,
                    rotation.present_configured_members,
                    rotation.active_members,
                    rotation.unreadable_members,
                );
                // Match daemon construction order exactly: a materialized
                // rotation pool owns the logical prefix; unreadable members
                // abort the whole refresh; only a non-fatal empty/filtered
                // rotation may fall through to an exact logical entry.
                let rotation_is_authoritative = vault_access
                    == ProviderVaultAccess::LockedOrUnavailable
                    || rotation.unreadable_members > 0
                    || has_active_pool
                    || !vault_present;
                if rotation_is_authoritative {
                    let value_match = if vault_access
                        == ProviderVaultAccess::LockedOrUnavailable
                    {
                        "UNKNOWN(vault locked or unavailable)"
                    } else if alias_target.is_some() {
                        "UNKNOWN(env contains a Vault alias, not plaintext)"
                    } else if rotation.unreadable_members > 0 {
                        "UNKNOWN(rotation member could not be decrypted)"
                    } else if let Some(env_value) = env_value {
                        if rotation
                            .values
                            .iter()
                            .any(|value| values_match_in_memory(env_value, value))
                        {
                            "MATCH_ANY"
                        } else if rotation.values.is_empty() {
                            "UNKNOWN(rotation has no active readable member)"
                        } else {
                            "MISMATCH_ALL"
                        }
                    } else {
                        "UNKNOWN(env value absent)"
                    };
                    let daemon_effective_source = if vault_access
                        == ProviderVaultAccess::LockedOrUnavailable
                    {
                        "UNKNOWN(rotation locked or unavailable; live LKG cache not observable)"
                    } else if rotation.unreadable_members > 0 {
                        "UNKNOWN(rotation refresh unreadable; live LKG cache not observable)"
                    } else if has_active_pool {
                        if alias_target.is_some() {
                            "VAULT_ROTATION_ALIAS"
                        } else {
                            "VAULT_ROTATION"
                        }
                    } else if env_value.is_some_and(|value| {
                        !value.is_empty() && !value.starts_with(tachi_llm::VAULT_ALIAS_PREFIX)
                    }) {
                        "ENV"
                    } else {
                        "UNKNOWN(rotation has no active member; live LKG cache not observable)"
                    };
                    let age_days = rotation
                        .oldest_reportable_updated_at
                        .as_deref()
                        .and_then(|value| age_days_from_updated_at(value, now));
                    let age_basis = age_days.and(rotation.age_basis.clone());

                    return Ok(ProviderDoctorRow {
                        provider: provider.clone(),
                        shape,
                        env_ref: Some(env_name.clone()),
                        admitted: Some(admitted.contains(&env_name)),
                        vault: Some(true),
                        vault_rotation: true,
                        age_days,
                        age_basis,
                        value_match: value_match.to_string(),
                        daemon_effective_source: daemon_effective_source.to_string(),
                        source_completeness: rotation_completeness,
                    });
                }
            }

            let rotation_fallback = rotation.filter(|rotation| {
                vault_access == ProviderVaultAccess::Unlocked
                    && rotation.unreadable_members == 0
                    && rotation.values.is_empty()
            });

            let value_match = if !vault_present {
                "UNKNOWN(vault value absent)"
            } else if vault_access == ProviderVaultAccess::LockedOrUnavailable {
                "UNKNOWN(vault locked or unavailable)"
            } else if alias_target.is_some() {
                "UNKNOWN(env contains a Vault alias, not plaintext)"
            } else if let (Some(env_value), Some(vault_value)) = (env_value, vault_value) {
                if values_match_in_memory(env_value, vault_value) {
                    "MATCH"
                } else {
                    "MISMATCH"
                }
            } else if env_value.is_none() {
                "UNKNOWN(env value absent)"
            } else {
                "UNKNOWN(vault value could not be decrypted)"
            };

            let daemon_effective_source = if alias_target.is_some() {
                if vault_present && vault_value.is_some() {
                    if rotation_fallback.is_some() {
                        "VAULT_ALIAS_EXACT_FALLBACK"
                    } else {
                        "VAULT_ALIAS"
                    }
                } else if vault_present {
                    "UNKNOWN(alias target unreadable; live LKG cache not observable)"
                } else {
                    "UNKNOWN(alias target absent; live LKG cache not observable)"
                }
            } else if vault_present && vault_value.is_some() {
                if rotation_fallback.is_some() {
                    "VAULT_EXACT_FALLBACK"
                } else {
                    "VAULT"
                }
            } else if vault_present {
                "UNKNOWN(vault present but unreadable; live daemon cache not observable)"
            } else if env_value.is_some_and(|value| !value.is_empty()) {
                "ENV"
            } else {
                "UNKNOWN(no Vault or env value; external OAuth not observable)"
            };

            let source_completeness = rotation_fallback.map_or_else(
                || "INCOMPLETE(external OAuth and live daemon cache not observable)".to_string(),
                |rotation| {
                    format!(
                        "INCOMPLETE(rotation produced no usable pool; configured_members={} present_configured_members={} active_members={} unreadable_members={}; exact fallback evaluated; external OAuth, live health, and daemon LKG cache not observable)",
                        rotation.configured_members,
                        rotation.present_configured_members,
                        rotation.active_members,
                        rotation.unreadable_members,
                    )
                },
            );

            Ok(ProviderDoctorRow {
                provider: provider.clone(),
                shape,
                env_ref: Some(env_name.clone()),
                admitted: Some(admitted.contains(&env_name)),
                vault: Some(vault_present),
                vault_rotation: false,
                age_days: updated_at.and_then(|value| age_days_from_updated_at(value, now)),
                age_basis: None,
                value_match: value_match.to_string(),
                daemon_effective_source: daemon_effective_source.to_string(),
                source_completeness,
            })
        })
        .collect()
}

/// Compare only fixed-size digests in memory. Neither digest is formatted,
/// returned, stored, or logged; the report receives only MATCH/MISMATCH.
fn values_match_in_memory(left: &str, right: &str) -> bool {
    Blake2s256::digest(left.as_bytes()) == Blake2s256::digest(right.as_bytes())
}

fn vault_lookup_name_for_env(
    env_name: &str,
    env_values: &HashMap<String, String>,
) -> Result<String, String> {
    let Some(value) = env_values.get(env_name).map(|value| value.trim()) else {
        return Ok(env_name.to_string());
    };
    if !value.starts_with(tachi_llm::VAULT_ALIAS_PREFIX) {
        return Ok(env_name.to_string());
    }
    let target = tachi_llm::parse_vault_alias(value).ok_or_else(|| {
        format!(
            "provider env key '{env_name}' contains a malformed Vault alias; alias target redacted"
        )
    })?;
    crate::vault_crypto::validate_secret_name(target).map_err(|_| {
        format!(
            "provider env key '{env_name}' contains an invalid Vault alias; alias target redacted"
        )
    })?;
    Ok(target.to_string())
}

struct ReportSecretValues(HashMap<String, String>);

impl Drop for ReportSecretValues {
    fn drop(&mut self) {
        for (mut name, mut value) in self.0.drain() {
            crate::vault_crypto::zero_string(&mut name);
            crate::vault_crypto::zero_string(&mut value);
        }
    }
}

fn collect_referenced_env_values(
    providers: &BTreeMap<String, serde_json::Value>,
) -> ReportSecretValues {
    ReportSecretValues(
        providers
            .values()
            .filter_map(|block| {
                classify_api_key_value(extract_api_key_value(block))
                    .env_name()
                    .and_then(|name| {
                        std::env::var(name)
                            .ok()
                            .map(|value| (name.to_string(), value))
                    })
            })
            .collect(),
    )
}

fn collect_vault_lookup_names(
    providers: &BTreeMap<String, serde_json::Value>,
    env_values: &HashMap<String, String>,
) -> Result<HashSet<String>, String> {
    providers
        .values()
        .filter_map(|block| {
            classify_api_key_value(extract_api_key_value(block))
                .env_name()
                .map(str::to_string)
        })
        .map(|env_name| vault_lookup_name_for_env(&env_name, env_values))
        .collect()
}

/// Report-only Vault read. It decrypts only provider-referenced entries and
/// deliberately never calls a Vault touch, audit writer, cache refresh, or
/// any mutation API. An unreadable entry is omitted so its row becomes loud
/// UNKNOWN without exposing crypto details.
#[cfg(test)]
fn decrypt_vault_values_for_report(
    store: &MemoryStore,
    key: &[u8; 32],
    names: &HashSet<String>,
) -> Result<ReportSecretValues, String> {
    let mut values = HashMap::new();
    for name in names {
        let entry = store
            .vault_get_entry(name)
            .map_err(|_| "provider doctor could not read Vault entry metadata".to_string())?;
        let Some(entry) = entry else {
            continue;
        };
        if entry.secret_type != memcore::vault::SECRET_TYPE_API_KEY
            || !entry.name.ends_with("_API_KEY")
            || entry
                .allowed_agents
                .as_ref()
                .is_some_and(|agents| !agents.is_empty())
        {
            continue;
        }
        let Ok(decrypted) = crate::vault_crypto::decrypt(key, &entry.encrypted_value, &entry.nonce)
        else {
            continue;
        };
        let mut value = match String::from_utf8(decrypted) {
            Ok(value) => value,
            Err(err) => {
                let mut bytes = err.into_bytes();
                zero_plaintext_bytes(&mut bytes);
                continue;
            }
        };
        if value.trim().is_empty() {
            crate::vault_crypto::zero_string(&mut value);
            continue;
        }
        values.insert(name.clone(), value);
    }
    Ok(ReportSecretValues(values))
}

fn provider_health_is_unusable(
    health: Option<&VaultKeyHealth>,
    now: chrono::DateTime<chrono::Utc>,
) -> bool {
    let Some(health) = health else {
        return false;
    };
    if health.disabled || health.auth_failed {
        return true;
    }
    match health.status.as_str() {
        "exhausted" => true,
        "rate_limited" | "cooldown" => health
            .cooldown_until
            .as_deref()
            .and_then(|value| chrono::DateTime::parse_from_rfc3339(value).ok())
            .is_some_and(|until| until.with_timezone(&chrono::Utc) > now),
        _ => false,
    }
}

fn older_timestamp(current: &Option<String>, candidate: &str) -> Option<String> {
    let candidate_time = chrono::DateTime::parse_from_rfc3339(candidate).ok()?;
    let Some(current) = current else {
        return Some(candidate.to_string());
    };
    let current_time = chrono::DateTime::parse_from_rfc3339(current).ok()?;
    if candidate_time < current_time {
        Some(candidate.to_string())
    } else {
        Some(current.clone())
    }
}

/// Build the doctor-only Vault evidence in one read-only pass. Rotation
/// grouping delegates to the same helper used by daemon materialization; this
/// function never touches access accounting, audit, cache, or provider state.
fn collect_provider_vault_evidence(
    store: &MemoryStore,
    key: Option<&[u8; 32]>,
    names: &HashSet<String>,
    now: chrono::DateTime<chrono::Utc>,
) -> Result<ProviderVaultReportEvidence, String> {
    let entries = store
        .vault_list_entries()
        .map_err(|_| "provider doctor could not read Vault entry metadata".to_string())?;
    let rotations = store
        .vault_list_rotations()
        .map_err(|_| "provider doctor could not read Vault rotation metadata".to_string())?;
    let health_rows = store
        .vault_list_key_health(None)
        .map_err(|_| "provider doctor could not read Vault key health metadata".to_string())?;
    let health_by_identity: HashMap<(String, String), VaultKeyHealth> = health_rows
        .into_iter()
        .map(|health| ((health.logical_name.clone(), health.key_id.clone()), health))
        .collect();

    let mut evidence = ProviderVaultReportEvidence::default();
    for entry in entries.iter().filter(|entry| {
        names.contains(&entry.name)
            && entry.secret_type == SECRET_TYPE_API_KEY
            && entry.name.ends_with("_API_KEY")
            && entry
                .allowed_agents
                .as_ref()
                .is_none_or(|agents| agents.is_empty())
            && !provider_health_is_unusable(
                health_by_identity.get(&(entry.name.clone(), entry.name.clone())),
                now,
            )
    }) {
        let Some(key) = key else {
            evidence
                .exact_updated_at
                .insert(entry.name.clone(), entry.updated_at.clone());
            continue;
        };
        let decrypted =
            match crate::vault_crypto::decrypt(key, &entry.encrypted_value, &entry.nonce) {
                Ok(decrypted) => decrypted,
                Err(_) => {
                    evidence
                        .exact_updated_at
                        .insert(entry.name.clone(), entry.updated_at.clone());
                    continue;
                }
            };
        let mut value = match String::from_utf8(decrypted) {
            Ok(value) => value,
            Err(err) => {
                let mut bytes = err.into_bytes();
                zero_plaintext_bytes(&mut bytes);
                evidence
                    .exact_updated_at
                    .insert(entry.name.clone(), entry.updated_at.clone());
                continue;
            }
        };
        if value.trim().is_empty() {
            crate::vault_crypto::zero_string(&mut value);
            continue;
        }
        evidence
            .exact_updated_at
            .insert(entry.name.clone(), entry.updated_at.clone());
        evidence.exact_values.insert(entry.name.clone(), value);
    }

    for rotation in rotations
        .into_iter()
        .filter(|rotation| names.contains(&rotation.prefix))
    {
        let configured_members = usize::try_from(rotation.total_keys.max(0)).unwrap_or(usize::MAX);
        let matching =
            crate::vault_ops::collect_rotation_entries(entries.clone(), &rotation.prefix);
        let present_configured_members = matching
            .iter()
            .filter(|(index, _)| {
                let index = *index as usize;
                index > 0 && index <= configured_members
            })
            .count();
        let mut rotation_evidence = RotationReportEvidence {
            configured_members,
            present_configured_members,
            active_members: 0,
            unreadable_members: 0,
            values: Vec::new(),
            oldest_reportable_updated_at: None,
            age_basis: None,
        };
        let mut oldest_eligible = None;
        for (_, entry) in matching {
            if entry.secret_type != SECRET_TYPE_API_KEY
                || entry
                    .allowed_agents
                    .as_ref()
                    .is_some_and(|agents| !agents.is_empty())
                || provider_health_is_unusable(
                    health_by_identity.get(&(rotation.prefix.clone(), entry.name.clone())),
                    now,
                )
            {
                continue;
            }
            oldest_eligible = older_timestamp(&oldest_eligible, &entry.updated_at);
            let Some(key) = key else {
                continue;
            };
            let Ok(decrypted) =
                crate::vault_crypto::decrypt(key, &entry.encrypted_value, &entry.nonce)
            else {
                rotation_evidence.unreadable_members += 1;
                continue;
            };
            let mut value = match String::from_utf8(decrypted) {
                Ok(value) => value,
                Err(err) => {
                    let mut bytes = err.into_bytes();
                    zero_plaintext_bytes(&mut bytes);
                    rotation_evidence.unreadable_members += 1;
                    continue;
                }
            };
            if value.trim().is_empty() {
                crate::vault_crypto::zero_string(&mut value);
                continue;
            }
            rotation_evidence.active_members += 1;
            rotation_evidence.oldest_reportable_updated_at = older_timestamp(
                &rotation_evidence.oldest_reportable_updated_at,
                &entry.updated_at,
            );
            rotation_evidence.values.push(value);
        }
        if key.is_some() {
            rotation_evidence.age_basis = rotation_evidence
                .oldest_reportable_updated_at
                .as_ref()
                .map(|_| "oldest_active_member".to_string());
        } else {
            rotation_evidence.oldest_reportable_updated_at = oldest_eligible;
            rotation_evidence.age_basis = rotation_evidence
                .oldest_reportable_updated_at
                .as_ref()
                .map(|_| "oldest_eligible_member".to_string());
        }
        evidence
            .rotations
            .insert(rotation.prefix, rotation_evidence);
    }
    Ok(evidence)
}

fn zero_plaintext_bytes(bytes: &mut [u8]) {
    for byte in bytes {
        // SAFETY: byte is a valid, exclusively borrowed element of the slice.
        unsafe { std::ptr::write_volatile(byte, 0) };
    }
    std::sync::atomic::compiler_fence(std::sync::atomic::Ordering::SeqCst);
}

pub(super) fn render_provider_report(rows: &[ProviderDoctorRow]) -> String {
    let mut lines = vec![
        "INCOMPLETE: config, process env, and report-only Vault reads are observable; external OAuth stores and the daemon's live provider/LKG cache are not. UNKNOWN never means CLEAN."
            .to_string(),
        "PROVIDER · APIKEY_SHAPE · ENV_REF · ADMITTED · VAULT · AGE_DAYS · VALUE_MATCH · DAEMON_EFFECTIVE_SOURCE · SOURCE_COMPLETENESS"
            .to_string(),
    ];
    lines.extend(rows.iter().map(ProviderDoctorRow::format_line));
    lines.join("\n")
}

#[cfg(test)]
pub(super) fn vault_entry_updated_at_by_name(
    store: &MemoryStore,
) -> Result<HashMap<String, String>, String> {
    let entries = store
        .vault_list_entry_timestamps()
        .map_err(|e| format!("vault_list_entry_timestamps: {e}"))?;
    let mut map = HashMap::with_capacity(entries.len());
    for (name, updated_at) in entries {
        map.insert(name, updated_at);
    }
    Ok(map)
}

/// Run the providers doctor and print text rows to stdout. Loud errors to stderr
/// via the returned `Err` (caller prints and exits nonzero).
pub(super) fn run_providers_doctor(
    store: &MemoryStore,
    opencode_config: Option<PathBuf>,
    vault_key: Option<&[u8; 32]>,
) -> Result<(), Box<dyn std::error::Error>> {
    let config_path = resolve_opencode_config_path(opencode_config);
    let providers = load_provider_blocks(&config_path)?;
    let admitted = crate::status_ops::status_health::provider_api_key_env_names();
    let env_values = collect_referenced_env_values(&providers);
    let lookup_names = collect_vault_lookup_names(&providers, &env_values.0)?;
    let now = chrono::Utc::now();
    let evidence = collect_provider_vault_evidence(store, vault_key, &lookup_names, now)?;
    let rows = build_provider_rows_with_evidence(
        &providers,
        &admitted,
        &env_values.0,
        &evidence,
        if vault_key.is_some() {
            ProviderVaultAccess::Unlocked
        } else {
            ProviderVaultAccess::LockedOrUnavailable
        },
        now,
    )?;
    let rendered = render_provider_report(&rows);
    println!("{rendered}");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::NamedTempFile;

    fn write_fixture(json: &str) -> NamedTempFile {
        let mut file = NamedTempFile::new().expect("tempfile");
        file.write_all(json.as_bytes()).expect("write fixture");
        file.flush().expect("flush");
        file
    }

    #[test]
    fn classify_env_ref_literal_and_absent() {
        assert_eq!(
            classify_api_key_value(Some(&serde_json::json!("{env:OPENAI_API_KEY}"))),
            ApiKeyShape::EnvRef {
                name: "OPENAI_API_KEY".to_string()
            }
        );
        assert_eq!(
            classify_api_key_value(Some(&serde_json::json!("sk-secret-value-xyz"))),
            ApiKeyShape::Literal
        );
        assert_eq!(
            classify_api_key_value(Some(&serde_json::json!(""))),
            ApiKeyShape::Absent
        );
        assert_eq!(
            classify_api_key_value(Some(&serde_json::Value::Null)),
            ApiKeyShape::Absent
        );
        assert_eq!(classify_api_key_value(None), ApiKeyShape::Absent);
    }

    #[test]
    fn extract_prefers_options_apikey_over_toplevel() {
        let block = serde_json::json!({
            "apiKey": "{env:TOP_LEVEL}",
            "options": { "apiKey": "{env:FROM_OPTIONS}" }
        });
        let shape = classify_api_key_value(extract_api_key_value(&block));
        assert_eq!(
            shape,
            ApiKeyShape::EnvRef {
                name: "FROM_OPTIONS".to_string()
            }
        );
    }

    #[test]
    fn extract_falls_back_to_toplevel_apikey() {
        let block = serde_json::json!({ "apiKey": "{env:TOP_LEVEL}" });
        let shape = classify_api_key_value(extract_api_key_value(&block));
        assert_eq!(
            shape,
            ApiKeyShape::EnvRef {
                name: "TOP_LEVEL".to_string()
            }
        );
    }

    #[test]
    fn literal_report_is_constant_redacted_without_value_length_prefix_or_hash() {
        let secret = "super-secret-fixture-key-DO-NOT-LEAK";
        let fixture = format!(
            r#"{{
              "provider": {{
                "alpha": {{ "options": {{ "apiKey": "{secret}" }} }},
                "beta": {{ "options": {{ "apiKey": "{{env:OPENAI_API_KEY}}" }} }},
                "gamma": {{ "options": {{ "apiKey": "" }} }},
                "delta": {{ }}
              }}
            }}"#
        );
        let file = write_fixture(&fixture);
        let providers = load_provider_blocks(file.path()).expect("load fixture");
        let admitted = HashSet::from(["OPENAI_API_KEY".to_string()]);
        let mut vault = HashMap::new();
        vault.insert(
            "OPENAI_API_KEY".to_string(),
            "2026-07-11T00:00:00Z".to_string(),
        );
        let now = chrono::DateTime::parse_from_rfc3339("2026-07-23T00:00:00Z")
            .unwrap()
            .with_timezone(&chrono::Utc);
        let rows = build_provider_rows(&providers, &admitted, &vault, now);
        let lines: Vec<String> = rows.iter().map(|r| r.format_line()).collect();
        let joined = lines.join("\n");

        assert!(joined.contains(
            "alpha · LITERAL(redacted) · env_ref=n/a · admitted=n/a · vault=n/a · age_days=n/a · value_match=UNKNOWN(literal value is redacted and not compared) · daemon_effective_source=UNKNOWN(config literal is outside the daemon env-ref contract) · source_completeness=INCOMPLETE(external OAuth and live daemon cache not observable)"
        ));
        assert!(
            joined.contains("beta · {env:OPENAI_API_KEY} · env_ref=OPENAI_API_KEY · admitted=yes · vault=yes · age_days=12 · value_match=UNKNOWN(vault locked or unavailable) · daemon_effective_source=UNKNOWN(vault present but unreadable; live daemon cache not observable) · source_completeness=INCOMPLETE(external OAuth and live daemon cache not observable)")
        );
        assert!(joined.contains("gamma · absent · env_ref=n/a · admitted=n/a · vault=n/a · age_days=n/a · value_match=UNKNOWN(no apiKey value to compare) · daemon_effective_source=UNKNOWN(no config apiKey; live cache not observable) · source_completeness=INCOMPLETE(external OAuth and live daemon cache not observable)"));
        assert!(joined.contains("delta · absent · env_ref=n/a · admitted=n/a · vault=n/a · age_days=n/a · value_match=UNKNOWN(no apiKey value to compare) · daemon_effective_source=UNKNOWN(no config apiKey; live cache not observable) · source_completeness=INCOMPLETE(external OAuth and live daemon cache not observable)"));
        assert!(
            !joined.contains(secret),
            "literal secret must never appear in report output"
        );
        assert!(
            !joined.contains(&secret.chars().count().to_string()),
            "literal length must never appear in report output"
        );
        assert!(
            !joined.contains(&secret[..8]),
            "literal prefix must never appear in report output"
        );
        assert!(
            !joined.contains(&tachi_params::sha256_hex(secret.as_bytes())),
            "literal hash must never appear in report output"
        );
        assert!(
            joined.contains("daemon_effective_source=UNKNOWN"),
            "metadata-only doctor output must say that the effective credential source is unknown"
        );

        // Deterministic sort by provider name.
        let names: Vec<&str> = rows.iter().map(|r| r.provider.as_str()).collect();
        assert_eq!(names, vec!["alpha", "beta", "delta", "gamma"]);
    }

    #[test]
    fn load_provider_blocks_errors_loudly() {
        let missing = PathBuf::from("/tmp/tachi-opencode-config-does-not-exist-1395.json");
        let err = load_provider_blocks(&missing).expect_err("missing file");
        assert!(err.contains("not found"), "{err}");

        let bad = write_fixture("{ not json");
        let err = load_provider_blocks(bad.path()).expect_err("invalid json");
        assert!(err.contains("invalid JSON"), "{err}");

        let no_provider = write_fixture(r#"{"model":"x"}"#);
        let err = load_provider_blocks(no_provider.path()).expect_err("missing provider");
        assert!(err.contains("missing top-level 'provider'"), "{err}");

        let wrong_type = write_fixture(r#"{"provider":[]}"#);
        let err = load_provider_blocks(wrong_type.path()).expect_err("provider array");
        assert!(err.contains("must be a JSON object"), "{err}");
    }

    #[test]
    fn load_provider_blocks_rejects_report_line_injection_and_invalid_env_names() {
        let injected_provider = write_fixture(
            r#"{"provider":{"safe\nforged · CLEAN":{"options":{"apiKey":"{env:SAFE_KEY}"}}}}"#,
        );
        let err = load_provider_blocks(injected_provider.path())
            .expect_err("control characters in provider names must be rejected");
        assert!(err.contains("ASCII provider identifier"));
        assert!(
            !err.contains("forged"),
            "error must not echo unsafe provider name"
        );

        for invalid in [
            "{env:BAD-NAME}",
            "{env:BAD\nNAME}",
            "{env:9STARTS_WITH_DIGIT}",
        ] {
            let fixture = write_fixture(
                &serde_json::json!({
                    "provider": {"safe": {"options": {"apiKey": invalid}}}
                })
                .to_string(),
            );
            let err = load_provider_blocks(fixture.path())
                .expect_err("invalid env reference must be rejected");
            assert!(err.contains("invalid environment variable name"));
            assert!(
                !err.contains(invalid),
                "error must not echo invalid reference"
            );
        }
    }

    #[test]
    fn provider_ids_use_unambiguous_ascii_report_grammar() {
        for unsafe_name in [
            "unsafe\u{202e}name",
            "unsafe\u{2066}name",
            "safe · admitted=yes",
            "safe·forged",
            "safe|forged",
            "has space",
            "_starts_with_punctuation",
        ] {
            let fixture = write_fixture(
                &serde_json::json!({
                    "provider": {unsafe_name: {"options": {"apiKey": "{env:SAFE_KEY}"}}}
                })
                .to_string(),
            );
            let err = load_provider_blocks(fixture.path())
                .expect_err("ambiguous provider id must be rejected");
            assert!(err.contains("ASCII provider identifier"));
            assert!(
                !err.contains(unsafe_name),
                "error must not echo unsafe provider id"
            );
        }

        let valid = write_fixture(
            &serde_json::json!({
                "provider": {
                    "zhipuai-coding-plan": {},
                    "kimi-for-coding": {},
                    "xai/grok": {},
                    "vendor:model@v1+fast": {}
                }
            })
            .to_string(),
        );
        let providers = load_provider_blocks(valid.path()).expect("valid provider ids");
        assert_eq!(
            providers.keys().map(String::as_str).collect::<Vec<_>>(),
            vec![
                "kimi-for-coding",
                "vendor:model@v1+fast",
                "xai/grok",
                "zhipuai-coding-plan"
            ]
        );
    }

    #[test]
    fn admitted_no_and_vault_missing_for_env_ref() {
        let providers = BTreeMap::from([(
            "custom".to_string(),
            serde_json::json!({"options":{"apiKey":"{env:NOT_ADMITTED_KEY}"}}),
        )]);
        let admitted = HashSet::new();
        let vault = HashMap::new();
        let rows = build_provider_rows(&providers, &admitted, &vault, chrono::Utc::now());
        assert_eq!(rows.len(), 1);
        assert_eq!(
            rows[0].format_line(),
            "custom · {env:NOT_ADMITTED_KEY} · env_ref=NOT_ADMITTED_KEY · admitted=no · vault=no · age_days=n/a · value_match=UNKNOWN(vault value absent) · daemon_effective_source=UNKNOWN(no Vault or env value; external OAuth not observable) · source_completeness=INCOMPLETE(external OAuth and live daemon cache not observable)"
        );
    }

    #[test]
    fn provider_doctor_compares_values_and_reports_daemon_sources_without_leaking() {
        let match_secret = "r11-MATCH-high-entropy-7d5e4c2af38b";
        let env_mismatch_secret = "r11-ENV-mismatch-high-entropy-98bdf3471a";
        let vault_mismatch_secret = "r11-VAULT-mismatch-high-entropy-c38a9d017f";
        let providers = BTreeMap::from([
            (
                "anthropic".to_string(),
                serde_json::json!({"options":{"apiKey":"{env:ANTHROPIC_API_KEY}"}}),
            ),
            (
                "openai".to_string(),
                serde_json::json!({"options":{"apiKey":"{env:OPENAI_API_KEY}"}}),
            ),
        ]);
        let admitted = HashSet::from([
            "ANTHROPIC_API_KEY".to_string(),
            "OPENAI_API_KEY".to_string(),
        ]);
        let updated_at = HashMap::from([
            (
                "ANTHROPIC_API_KEY".to_string(),
                "2026-07-20T00:00:00Z".to_string(),
            ),
            (
                "OPENAI_API_KEY".to_string(),
                "2026-07-21T00:00:00Z".to_string(),
            ),
        ]);
        let env_values = HashMap::from([
            (
                "ANTHROPIC_API_KEY".to_string(),
                env_mismatch_secret.to_string(),
            ),
            ("OPENAI_API_KEY".to_string(), match_secret.to_string()),
        ]);
        let vault_values = HashMap::from([
            (
                "ANTHROPIC_API_KEY".to_string(),
                vault_mismatch_secret.to_string(),
            ),
            ("OPENAI_API_KEY".to_string(), match_secret.to_string()),
        ]);
        let now = chrono::DateTime::parse_from_rfc3339("2026-07-23T00:00:00Z")
            .unwrap()
            .with_timezone(&chrono::Utc);

        let rows = build_provider_rows_with_sources(
            &providers,
            &admitted,
            &updated_at,
            &env_values,
            &vault_values,
            ProviderVaultAccess::Unlocked,
            now,
        )
        .expect("source-aware report");
        let rendered = rows
            .iter()
            .map(ProviderDoctorRow::format_line)
            .collect::<Vec<_>>()
            .join("\n");

        assert!(rendered.contains("openai · {env:OPENAI_API_KEY} · env_ref=OPENAI_API_KEY"));
        assert!(rendered.contains("value_match=MATCH"));
        assert!(rendered.contains("value_match=MISMATCH"));
        assert!(rendered.contains("daemon_effective_source=VAULT"));
        assert!(rendered.contains("source_completeness=INCOMPLETE"));
        for secret in [match_secret, env_mismatch_secret, vault_mismatch_secret] {
            assert!(!rendered.contains(secret), "secret leaked in report");
            assert!(
                !rendered.contains(&secret[..12]),
                "secret prefix leaked in report"
            );
            assert!(
                !rendered.contains(&secret.len().to_string()),
                "secret length leaked in report"
            );
            assert!(
                !rendered.contains(&tachi_params::sha256_hex(secret.as_bytes())),
                "digest leaked in report"
            );
            let comparison_digest = format!("{:x}", Blake2s256::digest(secret.as_bytes()));
            assert!(
                !rendered.contains(&comparison_digest),
                "comparison digest leaked in report"
            );
        }
    }

    #[test]
    fn provider_doctor_reports_rotation_source_and_match_any_without_leaking() {
        let matching_member = "r12-rotation-match-member-5be8f9c1-MUST-NOT-LEAK";
        let other_member = "r12-rotation-other-member-1c72a3d4-MUST-NOT-LEAK";
        let providers = BTreeMap::from([(
            "voyage".to_string(),
            serde_json::json!({"options":{"apiKey":"{env:VOYAGE_API_KEY}"}}),
        )]);
        let env_values =
            HashMap::from([("VOYAGE_API_KEY".to_string(), matching_member.to_string())]);
        let evidence = ProviderVaultReportEvidence {
            exact_updated_at: HashMap::new(),
            exact_values: HashMap::new(),
            rotations: HashMap::from([(
                "VOYAGE_API_KEY".to_string(),
                RotationReportEvidence {
                    configured_members: 2,
                    present_configured_members: 2,
                    active_members: 2,
                    unreadable_members: 0,
                    values: vec![other_member.to_string(), matching_member.to_string()],
                    oldest_reportable_updated_at: Some("2026-07-20T00:00:00Z".to_string()),
                    age_basis: Some("oldest_active_member".to_string()),
                },
            )]),
        };
        let now = chrono::DateTime::parse_from_rfc3339("2026-07-23T00:00:00Z")
            .unwrap()
            .with_timezone(&chrono::Utc);

        let rows = build_provider_rows_with_evidence(
            &providers,
            &HashSet::from(["VOYAGE_API_KEY".to_string()]),
            &env_values,
            &evidence,
            ProviderVaultAccess::Unlocked,
            now,
        )
        .expect("rotation-aware report");
        let rendered = render_provider_report(&rows);

        assert!(rendered.contains("vault=yes (rotation)"), "{rendered}");
        assert!(rendered.contains("value_match=MATCH_ANY"), "{rendered}");
        assert!(
            rendered.contains("daemon_effective_source=VAULT_ROTATION"),
            "{rendered}"
        );
        assert!(
            rendered.contains("age_basis=oldest_active_member"),
            "{rendered}"
        );
        for secret in [matching_member, other_member] {
            assert!(!rendered.contains(secret), "rotation secret leaked");
            assert!(!rendered.contains(&secret[..12]), "secret prefix leaked");
            assert!(!rendered.contains(&tachi_params::sha256_hex(secret.as_bytes())));
        }
        assert!(!rendered.contains("VOYAGE_API_KEY_1"));
        assert!(!rendered.contains("VOYAGE_API_KEY_2"));
    }

    #[test]
    fn provider_doctor_active_rotation_outranks_same_name_exact_entry() {
        let rotation_secret = "r13-rotation-wins-70f164c2-MUST-NOT-LEAK";
        let exact_secret = "r13-exact-loses-b29d640e-MUST-NOT-LEAK";
        let providers = BTreeMap::from([(
            "voyage".to_string(),
            serde_json::json!({"options":{"apiKey":"{env:VOYAGE_API_KEY}"}}),
        )]);
        let evidence = ProviderVaultReportEvidence {
            exact_updated_at: HashMap::from([(
                "VOYAGE_API_KEY".to_string(),
                "2026-07-22T00:00:00Z".to_string(),
            )]),
            exact_values: HashMap::from([("VOYAGE_API_KEY".to_string(), exact_secret.to_string())]),
            rotations: HashMap::from([(
                "VOYAGE_API_KEY".to_string(),
                RotationReportEvidence {
                    configured_members: 1,
                    present_configured_members: 1,
                    active_members: 1,
                    unreadable_members: 0,
                    values: vec![rotation_secret.to_string()],
                    oldest_reportable_updated_at: Some("2026-07-21T00:00:00Z".to_string()),
                    age_basis: Some("oldest_active_member".to_string()),
                },
            )]),
        };
        let rows = build_provider_rows_with_evidence(
            &providers,
            &HashSet::new(),
            &HashMap::from([("VOYAGE_API_KEY".to_string(), rotation_secret.to_string())]),
            &evidence,
            ProviderVaultAccess::Unlocked,
            chrono::Utc::now(),
        )
        .expect("exact plus active rotation report");
        let rendered = render_provider_report(&rows);

        assert_eq!(rows[0].daemon_effective_source, "VAULT_ROTATION");
        assert_eq!(rows[0].value_match, "MATCH_ANY");
        assert!(rows[0].vault_rotation);
        for forbidden in [rotation_secret, exact_secret, "VOYAGE_API_KEY_1"] {
            assert!(!rendered.contains(forbidden), "secret/member leaked");
        }
    }

    #[test]
    fn provider_doctor_alias_to_prefix_uses_active_rotation_before_exact() {
        let rotation_secret = "r13-alias-rotation-208f61bc-MUST-NOT-LEAK";
        let exact_secret = "r13-alias-exact-9ae34db7-MUST-NOT-LEAK";
        let alias_target = "R13_ALIAS_TARGET_MUST_NOT_LEAK";
        let providers = BTreeMap::from([(
            "voyage".to_string(),
            serde_json::json!({"options":{"apiKey":"{env:VOYAGE_API_KEY}"}}),
        )]);
        let evidence = ProviderVaultReportEvidence {
            exact_updated_at: HashMap::from([(
                alias_target.to_string(),
                "2026-07-22T00:00:00Z".to_string(),
            )]),
            exact_values: HashMap::from([(alias_target.to_string(), exact_secret.to_string())]),
            rotations: HashMap::from([(
                alias_target.to_string(),
                RotationReportEvidence {
                    configured_members: 1,
                    present_configured_members: 1,
                    active_members: 1,
                    unreadable_members: 0,
                    values: vec![rotation_secret.to_string()],
                    oldest_reportable_updated_at: Some("2026-07-21T00:00:00Z".to_string()),
                    age_basis: Some("oldest_active_member".to_string()),
                },
            )]),
        };
        let env_values = HashMap::from([(
            "VOYAGE_API_KEY".to_string(),
            format!("vault:{alias_target}"),
        )]);
        let rows = build_provider_rows_with_evidence(
            &providers,
            &HashSet::new(),
            &env_values,
            &evidence,
            ProviderVaultAccess::Unlocked,
            chrono::Utc::now(),
        )
        .expect("alias-to-prefix report");
        let rendered = render_provider_report(&rows);

        assert_eq!(rows[0].daemon_effective_source, "VAULT_ROTATION_ALIAS");
        assert!(rows[0].vault_rotation);
        for forbidden in [rotation_secret, exact_secret, alias_target] {
            assert!(!rendered.contains(forbidden), "secret/alias target leaked");
        }
    }

    #[test]
    fn provider_doctor_empty_or_filtered_rotation_falls_back_to_exact() {
        let exact_secret = "r13-exact-fallback-f81463b9-MUST-NOT-LEAK";
        let providers = BTreeMap::from([(
            "voyage".to_string(),
            serde_json::json!({"options":{"apiKey":"{env:VOYAGE_API_KEY}"}}),
        )]);
        for (configured_members, present_configured_members) in [(2, 0), (2, 2)] {
            let evidence = ProviderVaultReportEvidence {
                exact_updated_at: HashMap::from([(
                    "VOYAGE_API_KEY".to_string(),
                    "2026-07-22T00:00:00Z".to_string(),
                )]),
                exact_values: HashMap::from([(
                    "VOYAGE_API_KEY".to_string(),
                    exact_secret.to_string(),
                )]),
                rotations: HashMap::from([(
                    "VOYAGE_API_KEY".to_string(),
                    RotationReportEvidence {
                        configured_members,
                        present_configured_members,
                        active_members: 0,
                        unreadable_members: 0,
                        values: Vec::new(),
                        oldest_reportable_updated_at: None,
                        age_basis: None,
                    },
                )]),
            };
            let rows = build_provider_rows_with_evidence(
                &providers,
                &HashSet::new(),
                &HashMap::from([("VOYAGE_API_KEY".to_string(), exact_secret.to_string())]),
                &evidence,
                ProviderVaultAccess::Unlocked,
                chrono::Utc::now(),
            )
            .expect("rotation fallback report");
            let rendered = render_provider_report(&rows);

            assert_eq!(rows[0].daemon_effective_source, "VAULT_EXACT_FALLBACK");
            assert_eq!(rows[0].value_match, "MATCH");
            assert!(!rows[0].vault_rotation);
            assert!(rows[0]
                .source_completeness
                .contains("rotation produced no usable pool"));
            assert!(!rendered.contains(exact_secret), "exact secret leaked");
        }
    }

    #[test]
    fn provider_doctor_rotation_decrypt_failure_refuses_exact_fallback() {
        let exact_secret = "r13-exact-must-not-mask-error-d741bc05-MUST-NOT-LEAK";
        let providers = BTreeMap::from([(
            "voyage".to_string(),
            serde_json::json!({"options":{"apiKey":"{env:VOYAGE_API_KEY}"}}),
        )]);
        let evidence = ProviderVaultReportEvidence {
            exact_updated_at: HashMap::from([(
                "VOYAGE_API_KEY".to_string(),
                "2026-07-22T00:00:00Z".to_string(),
            )]),
            exact_values: HashMap::from([("VOYAGE_API_KEY".to_string(), exact_secret.to_string())]),
            rotations: HashMap::from([(
                "VOYAGE_API_KEY".to_string(),
                RotationReportEvidence {
                    configured_members: 1,
                    present_configured_members: 1,
                    active_members: 0,
                    unreadable_members: 1,
                    values: Vec::new(),
                    oldest_reportable_updated_at: None,
                    age_basis: None,
                },
            )]),
        };
        let rows = build_provider_rows_with_evidence(
            &providers,
            &HashSet::new(),
            &HashMap::from([("VOYAGE_API_KEY".to_string(), exact_secret.to_string())]),
            &evidence,
            ProviderVaultAccess::Unlocked,
            chrono::Utc::now(),
        )
        .expect("rotation refusal report");
        let rendered = render_provider_report(&rows);

        assert_eq!(
            rows[0].daemon_effective_source,
            "UNKNOWN(rotation refresh unreadable; live LKG cache not observable)"
        );
        assert_eq!(
            rows[0].value_match,
            "UNKNOWN(rotation member could not be decrypted)"
        );
        assert!(rows[0].vault_rotation);
        assert!(!rendered.contains(exact_secret), "exact secret leaked");
        assert!(!rendered.contains("VOYAGE_API_KEY_1"), "member leaked");
    }

    #[test]
    fn provider_doctor_distinguishes_env_locked_and_alias_sources_without_target_leakage() {
        let alias_target = "R11_ALIAS_TARGET_MUST_NOT_LEAK";
        let alias_value = "r11-alias-value-MUST-NOT-LEAK-6d3f4a";
        let providers = BTreeMap::from([
            (
                "alias".to_string(),
                serde_json::json!({"options":{"apiKey":"{env:ANTHROPIC_API_KEY}"}}),
            ),
            (
                "env-only".to_string(),
                serde_json::json!({"options":{"apiKey":"{env:EXA_API_KEY}"}}),
            ),
            (
                "locked".to_string(),
                serde_json::json!({"options":{"apiKey":"{env:OPENAI_API_KEY}"}}),
            ),
        ]);
        let admitted = HashSet::from([
            "ANTHROPIC_API_KEY".to_string(),
            "EXA_API_KEY".to_string(),
            "OPENAI_API_KEY".to_string(),
        ]);
        let env_values = HashMap::from([
            (
                "ANTHROPIC_API_KEY".to_string(),
                format!("vault:{alias_target}"),
            ),
            (
                "EXA_API_KEY".to_string(),
                "r11-env-only-value-MUST-NOT-LEAK".to_string(),
            ),
            (
                "OPENAI_API_KEY".to_string(),
                "r11-locked-env-value-MUST-NOT-LEAK".to_string(),
            ),
        ]);
        let updated_at = HashMap::from([
            (alias_target.to_string(), "2026-07-22T00:00:00Z".to_string()),
            (
                "OPENAI_API_KEY".to_string(),
                "2026-07-22T00:00:00Z".to_string(),
            ),
        ]);
        let vault_values = HashMap::from([(alias_target.to_string(), alias_value.to_string())]);
        let now = chrono::DateTime::parse_from_rfc3339("2026-07-23T00:00:00Z")
            .unwrap()
            .with_timezone(&chrono::Utc);

        let unlocked = build_provider_rows_with_sources(
            &providers,
            &admitted,
            &updated_at,
            &env_values,
            &vault_values,
            ProviderVaultAccess::Unlocked,
            now,
        )
        .expect("unlocked source report");
        let unlocked = render_provider_report(&unlocked);
        assert!(unlocked.contains("alias · {env:ANTHROPIC_API_KEY} · env_ref=ANTHROPIC_API_KEY"));
        assert!(unlocked.contains("daemon_effective_source=VAULT_ALIAS"));
        assert!(unlocked.contains("env-only · {env:EXA_API_KEY}"));
        assert!(unlocked.contains("daemon_effective_source=ENV"));
        assert!(!unlocked.contains(alias_target), "alias target leaked");
        assert!(!unlocked.contains(alias_value), "alias value leaked");

        let locked = build_provider_rows_with_sources(
            &providers,
            &admitted,
            &updated_at,
            &env_values,
            &HashMap::new(),
            ProviderVaultAccess::LockedOrUnavailable,
            now,
        )
        .expect("locked source report");
        let locked = render_provider_report(&locked);
        assert!(locked.contains("value_match=UNKNOWN(vault locked or unavailable)"));
        assert!(locked.contains(
            "daemon_effective_source=UNKNOWN(alias target unreadable; live LKG cache not observable)"
        ));
        assert!(locked.contains(
            "daemon_effective_source=UNKNOWN(vault present but unreadable; live daemon cache not observable)"
        ));
        assert!(!locked.contains(alias_target), "locked alias target leaked");
        assert!(!locked.contains(alias_value), "locked alias value leaked");

        let alias_missing = build_provider_rows_with_sources(
            &providers,
            &admitted,
            &HashMap::from([(
                "OPENAI_API_KEY".to_string(),
                "2026-07-22T00:00:00Z".to_string(),
            )]),
            &env_values,
            &HashMap::new(),
            ProviderVaultAccess::Unlocked,
            now,
        )
        .expect("missing alias source report");
        let alias_missing = alias_missing
            .iter()
            .find(|row| row.provider == "alias")
            .expect("alias provider row")
            .format_line();
        assert!(alias_missing.contains("vault=no"));
        assert!(alias_missing.contains(
            "daemon_effective_source=UNKNOWN(alias target absent; live LKG cache not observable)"
        ));
        assert!(
            !alias_missing.contains(alias_target),
            "missing alias target leaked"
        );
    }

    #[test]
    fn provider_doctor_rejects_malformed_alias_without_echoing_target() {
        let alias_sentinel = "BAD ALIAS TARGET MUST NOT LEAK";
        let providers = BTreeMap::from([(
            "safe".to_string(),
            serde_json::json!({"options":{"apiKey":"{env:ANTHROPIC_API_KEY}"}}),
        )]);
        let env_values = HashMap::from([(
            "ANTHROPIC_API_KEY".to_string(),
            format!("vault:{alias_sentinel}"),
        )]);
        let err = build_provider_rows_with_sources(
            &providers,
            &HashSet::new(),
            &HashMap::new(),
            &env_values,
            &HashMap::new(),
            ProviderVaultAccess::Unlocked,
            chrono::Utc::now(),
        )
        .expect_err("malformed alias must fail loudly");

        assert!(err.contains("ANTHROPIC_API_KEY"));
        assert!(err.contains("alias target redacted"));
        assert!(
            !err.contains(alias_sentinel),
            "alias target leaked in error"
        );
    }

    #[test]
    fn resolve_config_path_prefers_cli_then_env() {
        let cli = PathBuf::from("/tmp/cli-opencode.json");
        assert_eq!(resolve_opencode_config_path(Some(cli.clone())), cli);
        let _guard =
            crate::test_support::EnvRestore::set("TACHI_OPENCODE_CONFIG", "/tmp/env-opencode.json");
        assert_eq!(
            resolve_opencode_config_path(None),
            PathBuf::from("/tmp/env-opencode.json")
        );
        assert_eq!(
            resolve_opencode_config_path(Some(PathBuf::from("/tmp/cli-wins.json"))),
            PathBuf::from("/tmp/cli-wins.json")
        );
    }

    fn sha256_file(path: &Path) -> String {
        tachi_params::sha256_hex(&fs::read(path).expect("read for hash"))
    }

    /// Doctor --providers must leave durable fixture bytes untouched and open
    /// the vault store through the read-only CLI path.
    #[test]
    fn providers_doctor_leaves_opencode_and_vault_db_byte_identical_read_only() {
        use super::super::open_cli_store_read_only;
        use base64::{engine::general_purpose::STANDARD as B64, Engine};
        use memcore::vault::{VaultCipher, VaultConfig, VaultEntry};

        let _lock = crate::utils::global_test_lock()
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let secret = "r11-report-only-decrypt-high-entropy-62a1f98d-MUST-NOT-LEAK";
        let literal = "r11-literal-high-entropy-3ad8c170-MUST-NOT-LEAK";
        let _env = crate::test_support::EnvRestore::set("TACHI_R11_DOCTOR_FIXTURE_API_KEY", secret);

        let dir = tempfile::tempdir().expect("tempdir");
        let db_path = dir.path().join("memory.db");
        let config_path = dir.path().join("opencode.json");

        fs::write(
            &config_path,
            serde_json::json!({
                "provider": {
                    "unmanaged-fixture": {
                        "options": {"apiKey": "{env:TACHI_R11_DOCTOR_FIXTURE_API_KEY}"}
                    },
                    "literal": {"options": {"apiKey": literal}}
                }
            })
            .to_string(),
        )
        .expect("write opencode fixture");

        let salt = b"r11-doctor-salt";
        let key = crate::vault_crypto::derive_cheap("r11-password", salt).expect("derive fixture");
        let verifier = crate::vault_crypto::create_verifier(key.bytes()).expect("verifier");
        let (encrypted_value, nonce) =
            crate::vault_crypto::encrypt(key.bytes(), secret.as_bytes()).expect("encrypt fixture");
        {
            let store = MemoryStore::open(db_path.to_str().expect("utf8 db path"))
                .expect("create vault db");
            store
                .vault_set_config(&VaultConfig {
                    salt: B64.encode(salt),
                    verifier,
                    kdf_algorithm: "argon2id".to_string(),
                    kdf_params: crate::vault_crypto::cheap_kdf_params_json().to_string(),
                    cipher: VaultCipher::Aes256Gcm,
                    created_at: "2026-07-01T00:00:00Z".to_string(),
                    updated_at: "2026-07-01T00:00:00Z".to_string(),
                })
                .expect("seed vault config");
            store
                .vault_upsert_entry(&VaultEntry {
                    name: "TACHI_R11_DOCTOR_FIXTURE_API_KEY".to_string(),
                    encrypted_value,
                    nonce,
                    secret_type: "api_key".to_string(),
                    description: "providers doctor fixture".to_string(),
                    allowed_agents: None,
                    created_at: "2026-07-01T00:00:00Z".to_string(),
                    updated_at: "2026-07-11T00:00:00Z".to_string(),
                    accessed_at: "2026-06-30T00:00:00Z".to_string(),
                    access_count: 17,
                })
                .expect("seed vault entry");
        }

        let before_db = sha256_file(&db_path);
        let before_cfg = sha256_file(&config_path);

        // Same open path as VaultAction::Doctor { providers: true, ... }.
        let store = open_cli_store_read_only(&db_path).expect("open_cli_store_read_only");
        let touch_err = store
            .vault_touch_entry("TACHI_R11_DOCTOR_FIXTURE_API_KEY")
            .expect_err("read-only store must reject writes");
        let touch_msg = touch_err.to_string().to_lowercase();
        assert!(
            touch_msg.contains("readonly") || touch_msg.contains("read-only"),
            "expected readonly write failure, got: {touch_err}"
        );

        let before_entry = store
            .vault_get_entry("TACHI_R11_DOCTOR_FIXTURE_API_KEY")
            .expect("read fixture entry")
            .expect("fixture entry exists");
        run_providers_doctor(&store, Some(config_path.clone()), Some(key.bytes()))
            .expect("providers doctor");
        let after_entry = store
            .vault_get_entry("TACHI_R11_DOCTOR_FIXTURE_API_KEY")
            .expect("read fixture entry after doctor")
            .expect("fixture entry still exists");

        assert_eq!(after_entry.access_count, before_entry.access_count);
        assert_eq!(after_entry.accessed_at, before_entry.accessed_at);
        assert_eq!(after_entry.updated_at, before_entry.updated_at);

        assert_eq!(
            sha256_file(&db_path),
            before_db,
            "vault DB must stay byte-identical after providers doctor"
        );
        assert_eq!(
            sha256_file(&config_path),
            before_cfg,
            "opencode.json must stay byte-identical after providers doctor"
        );

        let providers = load_provider_blocks(&config_path).expect("reload provider fixture");
        let env_values = HashMap::from([(
            "TACHI_R11_DOCTOR_FIXTURE_API_KEY".to_string(),
            secret.to_string(),
        )]);
        let names = collect_vault_lookup_names(&providers, &env_values).expect("lookup names");
        let values =
            decrypt_vault_values_for_report(&store, key.bytes(), &names).expect("report decrypt");
        let rows = build_provider_rows_with_sources(
            &providers,
            &HashSet::new(),
            &vault_entry_updated_at_by_name(&store).expect("vault metadata"),
            &env_values,
            &values.0,
            ProviderVaultAccess::Unlocked,
            chrono::Utc::now(),
        )
        .expect("build decrypted report");
        let rendered = render_provider_report(&rows);
        assert!(rendered.contains("unmanaged-fixture"));
        assert!(rendered.contains("value_match=MATCH"));
        assert!(!rendered.contains(secret), "decrypted Vault value leaked");
        assert!(!rendered.contains(literal), "literal config value leaked");
        assert!(!rendered.contains(&tachi_params::sha256_hex(secret.as_bytes())));
        let comparison_digest = format!("{:x}", Blake2s256::digest(secret.as_bytes()));
        assert!(!rendered.contains(&comparison_digest));
        let final_entry = store
            .vault_get_entry("TACHI_R11_DOCTOR_FIXTURE_API_KEY")
            .expect("read fixture entry after helper")
            .expect("fixture entry still exists");
        assert_eq!(final_entry.access_count, before_entry.access_count);
        assert_eq!(final_entry.accessed_at, before_entry.accessed_at);
    }

    #[test]
    fn providers_doctor_rotation_snapshot_is_complete_honest_and_read_only() {
        use super::super::open_cli_store_read_only;
        use memcore::vault::{VaultEntry, VaultKeyHealth, VaultKeyRotation};

        let matching_secret = "r12-rotation-MATCH-cdd7f180-MUST-NOT-LEAK";
        let other_secret = "r12-rotation-OTHER-a27e45c9-MUST-NOT-LEAK";
        let disabled_secret = "r12-rotation-DISABLED-957a813e-MUST-NOT-LEAK";
        let mismatch_secret = "r12-env-MISMATCH-1116dfd0-MUST-NOT-LEAK";
        let empty_exact_secret = "r13-empty-EXACT-83a16bd4-MUST-NOT-LEAK";
        let missing_exact_secret = "r13-missing-EXACT-c1942f76-MUST-NOT-LEAK";
        let dir = tempfile::tempdir().expect("tempdir");
        let db_path = dir.path().join("memory.db");
        let config_path = dir.path().join("opencode.json");
        fs::write(
            &config_path,
            serde_json::json!({
                "provider": {
                    "voyage": {"options": {"apiKey": "{env:VOYAGE_API_KEY}"}},
                    "broken": {"options": {"apiKey": "{env:BROKEN_API_KEY}"}},
                    "empty": {"options": {"apiKey": "{env:EMPTY_API_KEY}"}},
                    "missing": {"options": {"apiKey": "{env:MISSING_API_KEY}"}}
                }
            })
            .to_string(),
        )
        .expect("write provider fixture");

        let key = crate::vault_crypto::derive_cheap("r12-password", b"r12-doctor-salt")
            .expect("derive fixture key");
        let make_entry = |name: &str, value: &str, updated_at: &str, access_count: i64| {
            let (encrypted_value, nonce) =
                crate::vault_crypto::encrypt(key.bytes(), value.as_bytes()).expect("encrypt entry");
            VaultEntry {
                name: name.to_string(),
                encrypted_value,
                nonce,
                secret_type: SECRET_TYPE_API_KEY.to_string(),
                description: "rotation doctor fixture".to_string(),
                allowed_agents: None,
                created_at: "2026-07-01T00:00:00Z".to_string(),
                updated_at: updated_at.to_string(),
                accessed_at: "2026-06-30T00:00:00Z".to_string(),
                access_count,
            }
        };
        let entry_names = [
            "VOYAGE_API_KEY",
            "VOYAGE_API_KEY_1",
            "VOYAGE_API_KEY_2",
            "VOYAGE_API_KEY_3",
            "VOYAGE_API_KEY_4",
            "BROKEN_API_KEY_1",
            "EMPTY_API_KEY",
            "EMPTY_API_KEY_1",
            "EMPTY_API_KEY_2",
            "MISSING_API_KEY",
        ];
        {
            let store = MemoryStore::open(db_path.to_str().expect("utf8 db path"))
                .expect("create fixture db");
            for entry in [
                make_entry("VOYAGE_API_KEY", "", "2026-07-16T00:00:00Z", 10),
                make_entry("VOYAGE_API_KEY_1", other_secret, "2026-07-19T00:00:00Z", 11),
                make_entry(
                    "VOYAGE_API_KEY_2",
                    matching_secret,
                    "2026-07-20T00:00:00Z",
                    12,
                ),
                make_entry(
                    "VOYAGE_API_KEY_3",
                    disabled_secret,
                    "2026-07-18T00:00:00Z",
                    13,
                ),
                make_entry("VOYAGE_API_KEY_4", "", "2026-07-17T00:00:00Z", 14),
                make_entry("BROKEN_API_KEY_1", other_secret, "2026-07-21T00:00:00Z", 21),
                make_entry(
                    "EMPTY_API_KEY",
                    empty_exact_secret,
                    "2026-07-22T00:00:00Z",
                    30,
                ),
                make_entry("EMPTY_API_KEY_1", "", "2026-07-22T00:00:00Z", 31),
                make_entry(
                    "EMPTY_API_KEY_2",
                    disabled_secret,
                    "2026-07-22T00:00:00Z",
                    32,
                ),
                make_entry(
                    "MISSING_API_KEY",
                    missing_exact_secret,
                    "2026-07-22T00:00:00Z",
                    40,
                ),
            ] {
                store.vault_upsert_entry(&entry).expect("seed entry");
            }
            let mut broken = store
                .vault_get_entry("BROKEN_API_KEY_1")
                .expect("read broken fixture")
                .expect("broken fixture exists");
            broken.encrypted_value = "R12_INVALID_CIPHERTEXT_SENTINEL".to_string();
            store.vault_upsert_entry(&broken).expect("corrupt fixture");
            for rotation in [
                VaultKeyRotation {
                    prefix: "VOYAGE_API_KEY".to_string(),
                    current_index: 1,
                    total_keys: 5,
                    rotation_strategy: "round_robin".to_string(),
                    created_at: "2026-07-01T00:00:00Z".to_string(),
                    updated_at: "2026-07-22T00:00:00Z".to_string(),
                },
                VaultKeyRotation {
                    prefix: "BROKEN_API_KEY".to_string(),
                    current_index: 1,
                    total_keys: 1,
                    rotation_strategy: "round_robin".to_string(),
                    created_at: "2026-07-01T00:00:00Z".to_string(),
                    updated_at: "2026-07-22T00:00:00Z".to_string(),
                },
                VaultKeyRotation {
                    prefix: "EMPTY_API_KEY".to_string(),
                    current_index: 1,
                    total_keys: 2,
                    rotation_strategy: "round_robin".to_string(),
                    created_at: "2026-07-01T00:00:00Z".to_string(),
                    updated_at: "2026-07-22T00:00:00Z".to_string(),
                },
                VaultKeyRotation {
                    prefix: "MISSING_API_KEY".to_string(),
                    current_index: 1,
                    total_keys: 2,
                    rotation_strategy: "round_robin".to_string(),
                    created_at: "2026-07-01T00:00:00Z".to_string(),
                    updated_at: "2026-07-22T00:00:00Z".to_string(),
                },
            ] {
                store.vault_set_rotation(&rotation).expect("seed rotation");
            }
            for (logical_name, key_id) in [
                ("VOYAGE_API_KEY", "VOYAGE_API_KEY_3"),
                ("EMPTY_API_KEY", "EMPTY_API_KEY_2"),
            ] {
                store
                    .vault_upsert_key_health(&VaultKeyHealth {
                        logical_name: logical_name.to_string(),
                        key_id: key_id.to_string(),
                        disabled: true,
                        updated_at: "2026-07-22T00:00:00Z".to_string(),
                        ..VaultKeyHealth::default()
                    })
                    .expect("seed disabled health");
            }
        }

        let before_db = sha256_file(&db_path);
        let before_cfg = sha256_file(&config_path);
        let store = open_cli_store_read_only(&db_path).expect("open read-only fixture");
        let before_entries: HashMap<_, _> = entry_names
            .iter()
            .map(|name| {
                (
                    *name,
                    store
                        .vault_get_entry(name)
                        .expect("read before entry")
                        .expect("before entry exists"),
                )
            })
            .collect();
        let providers = load_provider_blocks(&config_path).expect("load provider fixture");
        let names = HashSet::from([
            "VOYAGE_API_KEY".to_string(),
            "BROKEN_API_KEY".to_string(),
            "EMPTY_API_KEY".to_string(),
            "MISSING_API_KEY".to_string(),
        ]);
        // The store owns entry updated_at on upsert, so advance from wall time
        // instead of pretending the fixture-provided timestamp survives.
        let now = chrono::Utc::now() + chrono::Duration::days(4);
        let evidence = collect_provider_vault_evidence(&store, Some(key.bytes()), &names, now)
            .expect("collect rotation evidence");
        let env_values = HashMap::from([
            ("VOYAGE_API_KEY".to_string(), matching_secret.to_string()),
            ("BROKEN_API_KEY".to_string(), mismatch_secret.to_string()),
            ("EMPTY_API_KEY".to_string(), empty_exact_secret.to_string()),
            (
                "MISSING_API_KEY".to_string(),
                missing_exact_secret.to_string(),
            ),
        ]);
        let rows = build_provider_rows_with_evidence(
            &providers,
            &HashSet::new(),
            &env_values,
            &evidence,
            ProviderVaultAccess::Unlocked,
            now,
        )
        .expect("build rotation report");
        let rendered = render_provider_report(&rows);

        let voyage = rows.iter().find(|row| row.provider == "voyage").unwrap();
        assert_eq!(voyage.value_match, "MATCH_ANY");
        assert_eq!(voyage.daemon_effective_source, "VAULT_ROTATION");
        assert_eq!(voyage.age_days, Some(4));
        assert_eq!(voyage.age_basis.as_deref(), Some("oldest_active_member"));
        assert!(voyage.source_completeness.contains("member set partial"));
        assert!(voyage.source_completeness.contains("configured_members=5"));
        assert!(voyage
            .source_completeness
            .contains("present_configured_members=4"));
        assert!(voyage.source_completeness.contains("active_members=2"));

        let broken = rows.iter().find(|row| row.provider == "broken").unwrap();
        assert_eq!(
            broken.value_match,
            "UNKNOWN(rotation member could not be decrypted)"
        );
        assert!(broken.daemon_effective_source.starts_with("UNKNOWN("));
        let empty = rows.iter().find(|row| row.provider == "empty").unwrap();
        assert_eq!(empty.daemon_effective_source, "VAULT_EXACT_FALLBACK");
        assert_eq!(empty.value_match, "MATCH");
        assert!(empty
            .source_completeness
            .contains("rotation produced no usable pool"));
        assert!(empty.source_completeness.contains("active_members=0"));
        let missing = rows.iter().find(|row| row.provider == "missing").unwrap();
        assert_eq!(missing.daemon_effective_source, "VAULT_EXACT_FALLBACK");
        assert_eq!(missing.value_match, "MATCH");
        assert!(missing
            .source_completeness
            .contains("present_configured_members=0"));

        let mismatch_env =
            HashMap::from([("VOYAGE_API_KEY".to_string(), mismatch_secret.to_string())]);
        let mismatch_rows = build_provider_rows_with_evidence(
            &BTreeMap::from([(
                "voyage".to_string(),
                serde_json::json!({"options":{"apiKey":"{env:VOYAGE_API_KEY}"}}),
            )]),
            &HashSet::new(),
            &mismatch_env,
            &evidence,
            ProviderVaultAccess::Unlocked,
            now,
        )
        .expect("build mismatch report");
        assert_eq!(mismatch_rows[0].value_match, "MISMATCH_ALL");

        let locked_evidence =
            collect_provider_vault_evidence(&store, None, &names, now).expect("locked evidence");
        let locked_rows = build_provider_rows_with_evidence(
            &providers,
            &HashSet::new(),
            &env_values,
            &locked_evidence,
            ProviderVaultAccess::LockedOrUnavailable,
            now,
        )
        .expect("build locked report");
        let locked_rotation = locked_rows
            .iter()
            .find(|row| row.provider == "broken")
            .unwrap();
        assert!(locked_rotation.vault_rotation);
        assert_eq!(
            locked_rotation.value_match,
            "UNKNOWN(vault locked or unavailable)"
        );
        assert!(locked_rotation
            .daemon_effective_source
            .starts_with("UNKNOWN("));
        assert_eq!(
            locked_rotation.age_basis.as_deref(),
            Some("oldest_eligible_member")
        );

        for secret in [
            matching_secret,
            other_secret,
            disabled_secret,
            mismatch_secret,
            empty_exact_secret,
            missing_exact_secret,
            "R12_INVALID_CIPHERTEXT_SENTINEL",
        ] {
            assert!(!rendered.contains(secret), "secret/ciphertext leaked");
            assert!(!rendered.contains(&secret[..12]), "secret prefix leaked");
            assert!(!rendered.contains(&tachi_params::sha256_hex(secret.as_bytes())));
            assert!(!rendered.contains(&format!("{:x}", Blake2s256::digest(secret.as_bytes()))));
            assert!(!rendered.contains(&secret.len().to_string()));
        }
        for name in entry_names {
            if tachi_llm::parse_rotation_member_name(name).is_some() {
                assert!(!rendered.contains(name), "concrete rotation member leaked");
            }
            let after = store
                .vault_get_entry(name)
                .expect("read after entry")
                .expect("after entry exists");
            let before = &before_entries[name];
            assert_eq!(after.access_count, before.access_count);
            assert_eq!(after.accessed_at, before.accessed_at);
            assert_eq!(after.updated_at, before.updated_at);
        }
        assert_eq!(sha256_file(&db_path), before_db, "Vault DB bytes changed");
        assert_eq!(
            sha256_file(&config_path),
            before_cfg,
            "config bytes changed"
        );
    }
}
