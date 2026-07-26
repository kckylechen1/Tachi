//! Report-only `tachi vault doctor --providers` view.
//!
//! Reads OpenCode config, classifies each provider's apiKey shape without
//! printing secret values, and joins against vault *metadata* only (no decrypt).

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
    /// Non-empty literal string — print length only, never the value.
    Literal { len: usize },
    /// Missing, null, empty, or non-string.
    Absent,
}

impl ApiKeyShape {
    pub(super) fn display(&self) -> String {
        match self {
            Self::EnvRef { name } => format!("{ENV_REF_PREFIX}{name}{ENV_REF_SUFFIX}"),
            Self::Literal { len } => format!("LITERAL({len} chars)"),
            Self::Absent => "absent".to_string(),
        }
    }

    pub(super) fn env_name(&self) -> Option<&str> {
        match self {
            Self::EnvRef { name } => Some(name.as_str()),
            Self::Literal { .. } | Self::Absent => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct ProviderDoctorRow {
    pub provider: String,
    pub shape: ApiKeyShape,
    pub admitted: Option<bool>,
    pub vault: Option<bool>,
    pub age_days: Option<u64>,
}

impl ProviderDoctorRow {
    pub(super) fn format_line(&self) -> String {
        let admitted = match self.admitted {
            Some(true) => "yes",
            Some(false) => "no",
            None => "n/a",
        };
        let vault = match self.vault {
            Some(true) => "yes",
            Some(false) => "no",
            None => "n/a",
        };
        let age = match self.age_days {
            Some(days) => days.to_string(),
            None => "n/a".to_string(),
        };
        format!(
            "{} · {} · admitted={} · vault={} · age_days={}",
            self.provider,
            self.shape.display(),
            admitted,
            vault,
            age
        )
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
            ApiKeyShape::Literal {
                len: trimmed.chars().count(),
            }
        }
        _ => ApiKeyShape::Absent,
    }
}

fn parse_env_ref(value: &str) -> Option<String> {
    if !value.starts_with(ENV_REF_PREFIX) || !value.ends_with(ENV_REF_SUFFIX) {
        return None;
    }
    let inner = &value[ENV_REF_PREFIX.len()..value.len() - ENV_REF_SUFFIX.len()];
    if inner.is_empty() || inner.contains('{') || inner.contains('}') {
        return None;
    }
    Some(inner.to_string())
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
        out.insert(name.clone(), block.clone());
    }
    Ok(out)
}

pub(super) fn build_provider_rows(
    providers: &BTreeMap<String, serde_json::Value>,
    admitted: &HashSet<String>,
    vault_updated_at: &HashMap<String, String>,
    now: chrono::DateTime<chrono::Utc>,
) -> Vec<ProviderDoctorRow> {
    providers
        .iter()
        .map(|(name, block)| {
            let shape = classify_api_key_value(extract_api_key_value(block));
            let (admitted_flag, vault_flag, age_days) = match shape.env_name() {
                Some(env_name) => {
                    let admitted_flag = Some(admitted.contains(env_name));
                    match vault_updated_at.get(env_name) {
                        Some(updated_at) => (
                            admitted_flag,
                            Some(true),
                            age_days_from_updated_at(updated_at, now),
                        ),
                        None => (admitted_flag, Some(false), None),
                    }
                }
                None => (None, None, None),
            };
            ProviderDoctorRow {
                provider: name.clone(),
                shape,
                admitted: admitted_flag,
                vault: vault_flag,
                age_days,
            }
        })
        .collect()
}

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
) -> Result<(), Box<dyn std::error::Error>> {
    let config_path = resolve_opencode_config_path(opencode_config);
    let providers = load_provider_blocks(&config_path)?;
    let admitted = crate::status_ops::status_health::provider_api_key_env_names();
    let vault_meta = vault_entry_updated_at_by_name(store)?;
    let rows = build_provider_rows(&providers, &admitted, &vault_meta, chrono::Utc::now());
    for row in rows {
        println!("{}", row.format_line());
    }
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
            ApiKeyShape::Literal { len: 19 }
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
    fn literal_masks_to_length_only_never_secret_string() {
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

        assert!(joined.contains(&format!(
            "alpha · LITERAL({} chars) · admitted=n/a · vault=n/a · age_days=n/a",
            secret.chars().count()
        )));
        assert!(
            joined.contains("beta · {env:OPENAI_API_KEY} · admitted=yes · vault=yes · age_days=12")
        );
        assert!(joined.contains("gamma · absent · admitted=n/a · vault=n/a · age_days=n/a"));
        assert!(joined.contains("delta · absent · admitted=n/a · vault=n/a · age_days=n/a"));
        assert!(
            !joined.contains(secret),
            "literal secret must never appear in report output: {joined}"
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
            "custom · {env:NOT_ADMITTED_KEY} · admitted=no · vault=no · age_days=n/a"
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
        use memcore::vault::VaultEntry;

        let dir = tempfile::tempdir().expect("tempdir");
        let db_path = dir.path().join("memory.db");
        let config_path = dir.path().join("opencode.json");

        fs::write(
            &config_path,
            r#"{
              "provider": {
                "openai": { "options": { "apiKey": "{env:OPENAI_API_KEY}" } },
                "literal": { "options": { "apiKey": "sk-fixture-literal" } }
              }
            }"#,
        )
        .expect("write opencode fixture");

        {
            let store = MemoryStore::open(db_path.to_str().expect("utf8 db path"))
                .expect("create vault db");
            store
                .vault_upsert_entry(&VaultEntry {
                    name: "OPENAI_API_KEY".to_string(),
                    encrypted_value: "ciphertext-fixture".to_string(),
                    nonce: "nonce-fixture".to_string(),
                    secret_type: "api_key".to_string(),
                    description: "providers doctor fixture".to_string(),
                    allowed_agents: None,
                    created_at: "2026-07-01T00:00:00Z".to_string(),
                    updated_at: "2026-07-11T00:00:00Z".to_string(),
                    accessed_at: String::new(),
                    access_count: 0,
                })
                .expect("seed vault entry");
        }

        let before_db = sha256_file(&db_path);
        let before_cfg = sha256_file(&config_path);

        // Same open path as VaultAction::Doctor { providers: true, ... }.
        let store = open_cli_store_read_only(&db_path).expect("open_cli_store_read_only");
        let touch_err = store
            .vault_touch_entry("OPENAI_API_KEY")
            .expect_err("read-only store must reject writes");
        let touch_msg = touch_err.to_string().to_lowercase();
        assert!(
            touch_msg.contains("readonly") || touch_msg.contains("read-only"),
            "expected readonly write failure, got: {touch_err}"
        );

        run_providers_doctor(&store, Some(config_path.clone())).expect("providers doctor");

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
    }
}
