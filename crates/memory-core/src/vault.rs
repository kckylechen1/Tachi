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
