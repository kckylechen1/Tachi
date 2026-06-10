// vault.rs — encrypted secret storage types

use serde::{Deserialize, Serialize};

/// Vault configuration stored in vault_config table (exactly one row).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VaultConfig {
    pub salt: String,          // base64-encoded 32 bytes
    pub verifier: String,      // base64-encoded encrypted verifier
    pub kdf_algorithm: String, // "argon2id"
    pub kdf_params: String,    // JSON: {"m":65536,"t":3,"p":4}
    pub cipher: String,        // "aes-256-gcm"
    pub created_at: String,
    pub updated_at: String,
}

/// A secret entry stored in vault_entries table.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VaultEntry {
    pub name: String,
    pub encrypted_value: String, // base64-encoded ciphertext
    pub nonce: String,           // base64-encoded 12 bytes
    pub secret_type: String,     // api_key | oauth_token | json_blob | cookie | other
    pub description: String,
    pub allowed_agents: Option<Vec<String>>,
    pub created_at: String,
    pub updated_at: String,
    pub accessed_at: String,
    pub access_count: i64,
}

/// Secret types supported by the vault.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum SecretType {
    ApiKey,
    OAuthToken,
    JsonBlob,
    Cookie,
    Other,
}

impl SecretType {
    pub fn as_str(&self) -> &str {
        match self {
            Self::ApiKey => "api_key",
            Self::OAuthToken => "oauth_token",
            Self::JsonBlob => "json_blob",
            Self::Cookie => "cookie",
            Self::Other => "other",
        }
    }

    #[allow(clippy::should_implement_trait)]
    pub fn from_str(s: &str) -> Self {
        match s {
            "api_key" => Self::ApiKey,
            "oauth_token" => Self::OAuthToken,
            "json_blob" => Self::JsonBlob,
            "cookie" => Self::Cookie,
            _ => Self::Other,
        }
    }
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
            cipher: "aes-256-gcm".to_string(),
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
            secret_type: "api_key".to_string(),
            description: String::new(),
            allowed_agents: None,
            created_at: String::new(),
            updated_at: String::new(),
            accessed_at: String::new(),
            access_count: 0,
        }
    }
}

impl VaultEntry {
    /// Check if this entry is part of a rotation group.
    pub fn is_rotation_key(&self) -> Option<String> {
        // Check if name matches pattern like "KEY_1", "KEY_2", etc.
        if let Some(pos) = self.name.rfind('_') {
            let suffix = &self.name[pos + 1..];
            if suffix.parse::<u32>().is_ok() {
                return Some(self.name[..pos].to_string());
            }
        }
        None
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
