// vault.rs — encrypted secret storage types

/// Provider-account metadata (tachi#1680): who a credential belongs to, as
/// opposed to what the credential is. Sits beside [`VaultKeyHealth`] for the
/// same reason that type does — it is vault-adjacent product data whose SQL
/// lives in `db`.
///
/// `admin`-gated for the same reason `db::vault_accounts` is: these are
/// operator-surface product types, and the `portable-kernel` facade resolves
/// `memcore` with `default-features = false`.
#[cfg(feature = "admin")]
pub mod accounts;
/// The bound reconcile plan apply consumes (tachi#1680 D4). `admin`-gated for
/// the same reason [`accounts`] is: it is built out of those types.
#[cfg(feature = "admin")]
pub mod apply;
/// Keyed credential fingerprints (tachi#1680 D2).
///
/// **Must stay `admin`-gated**: this module hashes with `blake2`, which
/// `Cargo.toml` enables through `admin = ["dep:blake2"]` only. Declaring it
/// unconditionally compiles here (the workspace build turns `admin` on) while
/// breaking `portable-kernel`, whose whole point is a `memcore` without the
/// admin feature — a failure no default-feature build can see.
#[cfg(feature = "admin")]
pub mod fingerprint;
/// The single writer for [`VaultKeyHealth`] (tachi#1680 D6): one outcome
/// vocabulary, one row transition, and the evidence kind that says whether an
/// outcome was probed or self-reported. Every channel that used to build a
/// health row by hand goes through it.
#[cfg(feature = "admin")]
pub mod health;

use serde::{Deserialize, Serialize};

pub const SECRET_TYPE_API_KEY: &str = "api_key";
pub const SECRET_TYPE_OAUTH_TOKEN: &str = "oauth_token";
pub const SECRET_TYPE_JSON_BLOB: &str = "json_blob";
pub const SECRET_TYPE_COOKIE: &str = "cookie";
pub const SECRET_TYPE_CONFIG: &str = "config";
pub const SECRET_TYPE_OTHER: &str = "other";
pub const SECRET_TYPES: &[&str] = &[
    SECRET_TYPE_API_KEY,
    SECRET_TYPE_OAUTH_TOKEN,
    SECRET_TYPE_JSON_BLOB,
    SECRET_TYPE_COOKIE,
    SECRET_TYPE_CONFIG,
    SECRET_TYPE_OTHER,
];

pub fn normalize_secret_type(value: &str) -> &'static str {
    match value.trim().to_ascii_lowercase().as_str() {
        SECRET_TYPE_API_KEY => SECRET_TYPE_API_KEY,
        SECRET_TYPE_OAUTH_TOKEN | "oauth" => SECRET_TYPE_OAUTH_TOKEN,
        SECRET_TYPE_JSON_BLOB | "json" => SECRET_TYPE_JSON_BLOB,
        SECRET_TYPE_COOKIE => SECRET_TYPE_COOKIE,
        SECRET_TYPE_CONFIG => SECRET_TYPE_CONFIG,
        _ => SECRET_TYPE_OTHER,
    }
}

fn lane_config_stem(name: &str) -> &str {
    let mut name = name.trim();
    while let Some((stem, suffix)) = name.rsplit_once('_') {
        if suffix.is_empty() || !suffix.bytes().all(|b| b.is_ascii_digit()) {
            break;
        }
        name = stem;
    }
    name
}

fn is_lane_config_stem(name: &str) -> bool {
    name.starts_with("ENABLE_")
        || name.ends_with("_BASE_URL")
        || name.ends_with("_URL")
        || name.ends_with("_MODEL")
        || name.ends_with("_BACKEND")
        || name.ends_with("_TIMEOUT")
        || name.ends_with("_ENABLED")
}

/// Lane URLs, models, and flags. These are not credentials: they must not
/// default to `api_key` and must not enter API-key pools. Rotation members
/// (`EXTRACT_BASE_URL_1`) follow the prefix.
pub fn is_lane_config_secret_name(name: &str) -> bool {
    let name = name.trim();
    is_lane_config_stem(name) || is_lane_config_stem(lane_config_stem(name))
}

/// Infer a vault `secret_type` from an env-style name when the caller omits
/// `--secret-type`.
///
/// Lane URLs, models, and flags are not credentials. Storing them as
/// `api_key` made `tachi vault list` indistinguishable from real keys and
/// let `EXTRACT_BASE_URL` sit in the same injection class as
/// `DEEPSEEK_API_KEY`. `ENABLE_*` wins over a trailing `_API_KEY` so a name
/// like `ENABLE_FALLBACK_API_KEY` infers config instead of being inferred as
/// a key and then refused.
pub fn infer_vault_secret_type(name: &str) -> &'static str {
    let name = name.trim();
    if is_lane_config_secret_name(name) {
        return SECRET_TYPE_CONFIG;
    }
    if name.ends_with("_API_KEY") || name.ends_with("_TOKEN") || name.ends_with("_SECRET") {
        return SECRET_TYPE_API_KEY;
    }
    SECRET_TYPE_API_KEY
}

/// Lane-config names whose values are endpoints. Rotation members such as
/// `EXTRACT_BASE_URL_1` follow the prefix stem, not the `_1` suffix.
pub fn is_lane_config_url_name(name: &str) -> bool {
    if !is_lane_config_secret_name(name) {
        return false;
    }
    let stem = lane_config_stem(name);
    stem.ends_with("_URL") || stem.ends_with("_BASE_URL")
}

/// Read-time classifier: lane-config names are config regardless of the
/// stored type. Pre-#1857 omitted types defaulted to `api_key`, so leftover
/// `EXTRACT_BASE_URL` rows must list as config without a schema bump.
pub fn effective_vault_secret_type(name: &str, stored: &str) -> &'static str {
    if is_lane_config_secret_name(name) {
        SECRET_TYPE_CONFIG
    } else {
        normalize_secret_type(stored)
    }
}

/// Explicit `api_key` on a lane-config name is refused. Inference already
/// chooses `config`; this blocks `--secret-type api_key`.
pub fn reject_api_key_type_for_lane_config(name: &str, secret_type: &str) -> Result<(), String> {
    if is_lane_config_secret_name(name) && normalize_secret_type(secret_type) == SECRET_TYPE_API_KEY
    {
        return Err(format!(
            "Vault name '{name}' is lane config, not a credential; refusing secret_type=api_key"
        ));
    }
    Ok(())
}

/// Supported vault ciphers.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq)]
pub enum VaultCipher {
    #[serde(rename = "aes-256-gcm")]
    #[default]
    Aes256Gcm,
}

impl VaultCipher {
    pub fn as_str(&self) -> &str {
        match self {
            Self::Aes256Gcm => "aes-256-gcm",
        }
    }
}

impl std::str::FromStr for VaultCipher {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "aes-256-gcm" => Ok(Self::Aes256Gcm),
            other => Err(format!("unsupported vault cipher: {other}")),
        }
    }
}

/// Vault configuration stored in vault_config table (exactly one row).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VaultConfig {
    pub salt: String,          // base64-encoded 32 bytes
    pub verifier: String,      // base64-encoded encrypted verifier
    pub kdf_algorithm: String, // "argon2id"
    pub kdf_params: String,    // JSON: {"m":65536,"t":3,"p":4}
    pub cipher: VaultCipher,   // only aes-256-gcm is supported
    pub created_at: String,
    pub updated_at: String,
}

/// A secret entry stored in vault_entries table.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VaultEntry {
    pub name: String,
    pub encrypted_value: String, // base64-encoded ciphertext
    pub nonce: String,           // base64-encoded 12 bytes
    pub secret_type: String,     // one of SECRET_TYPES
    pub description: String,
    pub allowed_agents: Option<Vec<String>>,
    pub created_at: String,
    pub updated_at: String,
    pub accessed_at: String,
    pub access_count: i64,
}

/// Multi-key rotation state for a secret name prefix.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VaultKeyRotation {
    pub prefix: String,            // e.g., "GEMINI_API_KEY"
    pub current_index: i64,        // which key is current (1, 2, 3...)
    pub total_keys: i64,           // total number of rotated keys
    pub rotation_strategy: String, // "round_robin" | "random" | "least_recently_used"
    pub created_at: String,
    pub updated_at: String,
}

/// Runtime health metadata for a concrete provider key entry.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VaultKeyHealth {
    pub logical_name: String,
    pub key_id: String,
    pub status: String,
    pub cooldown_until: Option<String>,
    pub last_success: Option<String>,
    pub last_attempt: Option<String>,
    pub last_error: Option<String>,
    pub error_count: i64,
    pub auth_failed: bool,
    pub disabled: bool,
    pub metadata: String,
    pub updated_at: String,
}

impl Default for VaultKeyHealth {
    fn default() -> Self {
        Self {
            logical_name: String::new(),
            key_id: String::new(),
            status: "ok".to_string(),
            cooldown_until: None,
            last_success: None,
            last_attempt: None,
            last_error: None,
            error_count: 0,
            auth_failed: false,
            disabled: false,
            metadata: "{}".to_string(),
            updated_at: String::new(),
        }
    }
}

impl Default for VaultConfig {
    fn default() -> Self {
        Self {
            salt: String::new(),
            verifier: String::new(),
            kdf_algorithm: "argon2id".to_string(),
            kdf_params: r#"{"m":65536,"t":3,"p":4}"#.to_string(),
            cipher: VaultCipher::default(),
            created_at: String::new(),
            updated_at: String::new(),
        }
    }
}

impl Default for VaultEntry {
    fn default() -> Self {
        Self {
            name: String::new(),
            encrypted_value: String::new(),
            nonce: String::new(),
            secret_type: SECRET_TYPE_API_KEY.to_string(),
            description: String::new(),
            allowed_agents: None,
            created_at: String::new(),
            updated_at: String::new(),
            accessed_at: String::new(),
            access_count: 0,
        }
    }
}

/// Extract the 1-based member index from an API key pool entry name.
/// Returns `Some(n)` for every numeric `prefix_n` suffix, including zero so
/// validation can reject it instead of letting runtime grouping disagree.
pub fn api_key_pool_member_index(name: &str, prefix: &str) -> Option<usize> {
    name.strip_prefix(prefix)
        .and_then(|suffix| suffix.strip_prefix('_'))
        .and_then(|suffix| suffix.parse::<usize>().ok())
}

/// Validate the complete structural member set for an API-key rotation.
/// Every numeric `prefix_N` row participates, regardless of a rotation row's
/// declared count, and indices must be contiguous from one.
pub fn validate_api_key_rotation_members(
    entries: &[VaultEntry],
    prefix: &str,
) -> Result<usize, String> {
    let mut indices = Vec::new();
    for entry in entries {
        let Some(index) = api_key_pool_member_index(&entry.name, prefix) else {
            continue;
        };
        let effective = effective_vault_secret_type(&entry.name, &entry.secret_type);
        if effective != SECRET_TYPE_API_KEY {
            return Err(format!(
                "Vault rotation member '{}' is {effective}, not an API-key credential",
                entry.name
            ));
        }
        indices.push(index);
    }
    indices.sort_unstable();
    for (offset, index) in indices.iter().enumerate() {
        let expected = offset + 1;
        if *index != expected {
            return Err(format!(
                "Vault rotation '{prefix}' has non-contiguous member index {index}; expected {expected}"
            ));
        }
    }
    Ok(indices.len())
}

/// Validate that a persisted rotation row exactly describes its complete
/// structural member set. A valid member append or removal is still invalid
/// until the rotation row is updated in the same transaction.
pub fn validate_api_key_rotation(
    entries: &[VaultEntry],
    rotation: &VaultKeyRotation,
) -> Result<usize, String> {
    let member_count = validate_api_key_rotation_members(entries, &rotation.prefix)?;
    if rotation.total_keys < 0 || member_count != rotation.total_keys as usize {
        return Err(format!(
            "Vault rotation '{}' declares {} keys but has {} contiguous API-key members",
            rotation.prefix, rotation.total_keys, member_count
        ));
    }
    Ok(member_count)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn vault_cipher_defaults_to_aes_256_gcm() {
        let cipher = VaultCipher::default();
        assert_eq!(cipher, VaultCipher::Aes256Gcm);
        assert_eq!(cipher.as_str(), "aes-256-gcm");
    }

    #[test]
    fn vault_cipher_roundtrips_through_string() {
        assert_eq!(
            "aes-256-gcm".parse::<VaultCipher>().unwrap(),
            VaultCipher::Aes256Gcm
        );
        assert!("unknown".parse::<VaultCipher>().is_err());
    }

    #[test]
    fn vault_cipher_serializes_as_string() {
        let cipher = VaultCipher::Aes256Gcm;
        let json = serde_json::to_string(&cipher).unwrap();
        assert_eq!(json, "\"aes-256-gcm\"");
        let decoded: VaultCipher = serde_json::from_str(&json).unwrap();
        assert_eq!(decoded, cipher);
    }

    #[test]
    fn secret_type_strings_are_canonicalized_without_an_orphan_enum() {
        assert_eq!(normalize_secret_type("api_key"), SECRET_TYPE_API_KEY);
        assert_eq!(normalize_secret_type("oauth"), SECRET_TYPE_OAUTH_TOKEN);
        assert_eq!(normalize_secret_type("json"), SECRET_TYPE_JSON_BLOB);
        assert_eq!(normalize_secret_type("cookie"), SECRET_TYPE_COOKIE);
        assert_eq!(normalize_secret_type("config"), SECRET_TYPE_CONFIG);
        assert_eq!(normalize_secret_type("weird"), SECRET_TYPE_OTHER);
        assert_eq!(
            SECRET_TYPES,
            &[
                SECRET_TYPE_API_KEY,
                SECRET_TYPE_OAUTH_TOKEN,
                SECRET_TYPE_JSON_BLOB,
                SECRET_TYPE_COOKIE,
                SECRET_TYPE_CONFIG,
                SECRET_TYPE_OTHER,
            ]
        );
    }

    #[test]
    fn infer_vault_secret_type_keeps_keys_and_demotes_lane_config() {
        assert_eq!(
            infer_vault_secret_type("DEEPSEEK_API_KEY"),
            SECRET_TYPE_API_KEY
        );
        assert_eq!(
            infer_vault_secret_type("LONGBRIDGE_APP_SECRET"),
            SECRET_TYPE_API_KEY
        );
        assert_eq!(
            infer_vault_secret_type("TUSHARE_TOKEN"),
            SECRET_TYPE_API_KEY
        );
        assert_eq!(
            infer_vault_secret_type("EXTRACT_BASE_URL"),
            SECRET_TYPE_CONFIG
        );
        assert_eq!(infer_vault_secret_type("DISTILL_MODEL"), SECRET_TYPE_CONFIG);
        assert_eq!(
            infer_vault_secret_type("ENABLE_PIPELINE"),
            SECRET_TYPE_CONFIG
        );
        assert_eq!(
            infer_vault_secret_type("FOUNDRY_DISTILL_BACKEND"),
            SECRET_TYPE_CONFIG
        );
        assert!(is_lane_config_secret_name("EXTRACT_BASE_URL_1"));
        assert!(is_lane_config_secret_name("EXTRACT_BASE_URL_1_1"));
        assert!(is_lane_config_url_name("EXTRACT_BASE_URL_1_1"));
        assert_eq!(
            infer_vault_secret_type("EXTRACT_BASE_URL_1_1"),
            SECRET_TYPE_CONFIG
        );
        assert!(!is_lane_config_secret_name("DEEPSEEK_API_KEY_1"));
        assert!(!is_lane_config_secret_name("DEEPSEEK_API_KEY_1_1"));
        assert_eq!(
            infer_vault_secret_type("ENABLE_FALLBACK_API_KEY"),
            SECRET_TYPE_CONFIG
        );
        assert!(is_lane_config_url_name("EXTRACT_BASE_URL"));
        assert!(is_lane_config_url_name("EXTRACT_BASE_URL_1"));
        assert!(!is_lane_config_url_name("DISTILL_MODEL"));
        assert!(!is_lane_config_url_name("DEEPSEEK_API_KEY"));
        assert!(reject_api_key_type_for_lane_config("EXTRACT_BASE_URL", "api_key").is_err());
        assert!(reject_api_key_type_for_lane_config("DEEPSEEK_API_KEY", "api_key").is_ok());
        assert_eq!(
            effective_vault_secret_type("EXTRACT_BASE_URL", "other"),
            SECRET_TYPE_CONFIG
        );
        assert_eq!(
            effective_vault_secret_type("EXTRACT_BASE_URL", "api_key"),
            SECRET_TYPE_CONFIG
        );
        assert_eq!(
            effective_vault_secret_type("DEEPSEEK_API_KEY", "other"),
            SECRET_TYPE_OTHER
        );
        assert_eq!(
            effective_vault_secret_type("CUSTOM_ENDPOINT", "config"),
            SECRET_TYPE_CONFIG
        );
    }
}
