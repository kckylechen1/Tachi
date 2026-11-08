use crate::vault_crypto as crypto;
use serde::Deserialize;

#[derive(Debug, Clone, Deserialize)]
pub struct VaultUnlockParams {
    #[serde(default)]
    pub password: String,
    #[serde(default)]
    pub password_fifo_path: Option<String>,
    /// Read the vault password from macOS Keychain (service `tachi-vault`,
    /// account `default`) instead of an inline `password` or
    /// `password_fifo_path`. Internal callers may use Keychain on macOS.
    /// The owner HTTP request accepts only a FIFO path; this domain parameter
    /// is not an MCP or owner-wire schema. Mutually exclusive with the other
    /// two sources; defaults to `false`.
    #[serde(default)]
    pub use_keychain: bool,
}

impl Drop for VaultUnlockParams {
    fn drop(&mut self) {
        crypto::zero_string(&mut self.password);
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct VaultListParams {
    #[serde(default)]
    pub secret_type: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct VaultSetupRotationParams {
    pub prefix: String,
    pub total_keys: i64,
    #[serde(default)]
    pub agent_id: Option<String>,
    #[serde(default = "default_rotation_strategy")]
    pub strategy: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct VaultLeaseApiKeyParams {
    /// Logical provider env name or standalone API key name.
    pub name: String,
    /// Optional child env var name. Defaults to `name`.
    #[serde(default)]
    pub env_name: Option<String>,
    /// Optional agent id for future restricted-secret checks.
    #[serde(default)]
    pub agent_id: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct VaultRecordKeyResultParams {
    /// Logical provider/env name, e.g. DEEPSEEK_API_KEY.
    pub logical_name: String,
    /// Concrete leased key id, e.g. DEEPSEEK_API_KEY_2.
    pub key_id: String,
    /// HTTP status code observed by the consumer, if available.
    #[serde(default)]
    pub status_code: Option<u16>,
    /// Outcome override: success | rate_limited | auth_failed | exhausted | error.
    #[serde(default)]
    pub outcome: Option<String>,
    /// Retry-After seconds for 429/cooldown responses.
    #[serde(default)]
    pub retry_after_secs: Option<u64>,
    /// Short non-secret reason or provider error class.
    #[serde(default)]
    pub reason: Option<String>,
}

fn default_rotation_strategy() -> String {
    "round_robin".to_string()
}
