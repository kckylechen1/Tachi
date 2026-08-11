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
/// Keyed credential fingerprints (tachi#1680 D2).
///
/// **Must stay `admin`-gated**: this module hashes with `blake2`, which
/// `Cargo.toml` enables through `admin = ["dep:blake2"]` only. Declaring it
/// unconditionally compiles here (the workspace build turns `admin` on) while
/// breaking `portable-kernel`, whose whole point is a `memcore` without the
/// admin feature — a failure no default-feature build can see.
#[cfg(feature = "admin")]
pub mod fingerprint;

use serde::{Deserialize, Serialize};

pub const SECRET_TYPE_API_KEY: &str = "api_key";
pub const SECRET_TYPE_OAUTH_TOKEN: &str = "oauth_token";
pub const SECRET_TYPE_JSON_BLOB: &str = "json_blob";
pub const SECRET_TYPE_COOKIE: &str = "cookie";
pub const SECRET_TYPE_OTHER: &str = "other";
pub const SECRET_TYPES: &[&str] = &[
    SECRET_TYPE_API_KEY,
    SECRET_TYPE_OAUTH_TOKEN,
    SECRET_TYPE_JSON_BLOB,
    SECRET_TYPE_COOKIE,
    SECRET_TYPE_OTHER,
];

pub fn normalize_secret_type(value: &str) -> &'static str {
    match value.trim().to_ascii_lowercase().as_str() {
        SECRET_TYPE_API_KEY => SECRET_TYPE_API_KEY,
        SECRET_TYPE_OAUTH_TOKEN | "oauth" => SECRET_TYPE_OAUTH_TOKEN,
        SECRET_TYPE_JSON_BLOB | "json" => SECRET_TYPE_JSON_BLOB,
        SECRET_TYPE_COOKIE => SECRET_TYPE_COOKIE,
        _ => SECRET_TYPE_OTHER,
    }
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
/// Returns `Some(n)` if `name` matches the pattern `prefix_n` where n > 0.
pub fn api_key_pool_member_index(name: &str, prefix: &str) -> Option<usize> {
    name.strip_prefix(prefix)
        .and_then(|suffix| suffix.strip_prefix('_'))
        .and_then(|suffix| suffix.parse::<usize>().ok())
        .filter(|idx| *idx > 0)
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
        assert_eq!(normalize_secret_type("weird"), SECRET_TYPE_OTHER);
        assert_eq!(
            SECRET_TYPES,
            &[
                SECRET_TYPE_API_KEY,
                SECRET_TYPE_OAUTH_TOKEN,
                SECRET_TYPE_JSON_BLOB,
                SECRET_TYPE_COOKIE,
                SECRET_TYPE_OTHER,
            ]
        );
    }
}
