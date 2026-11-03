use crate::vault_crypto as crypto;
use serde::Deserialize;

#[cfg(any(test, feature = "vault-test-api"))]
fn default_secret_type() -> String {
    String::new()
}

#[cfg(any(test, feature = "vault-test-api"))]
#[derive(Debug, Clone, Deserialize)]
pub struct VaultInitParams {
    pub password: String,
}

#[cfg(any(test, feature = "vault-test-api"))]
impl Drop for VaultInitParams {
    fn drop(&mut self) {
        crypto::zero_string(&mut self.password);
    }
}

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

#[cfg(any(test, feature = "vault-test-api"))]
#[derive(Debug, Clone, Deserialize)]
pub struct VaultSetParams {
    pub name: String,
    pub value: String,
    #[serde(default)]
    pub agent_id: Option<String>,
    #[serde(default = "default_secret_type")]
    pub secret_type: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub allowed_agents: Option<Vec<String>>,
    #[serde(default)]
    pub enable_rotation: bool,
    #[serde(default)]
    pub rotation_strategy: Option<String>,
    /// Lane slots (`EXTRACT_API_KEY`, …) refuse a fingerprint-changing
    /// overwrite unless this is set (tachi#1855). Account names ignore it.
    #[serde(default)]
    pub rebind: bool,
}

#[cfg(any(test, feature = "vault-test-api"))]
#[derive(Debug, Clone, Deserialize)]
pub struct VaultGetParams {
    pub name: String,
    #[serde(default)]
    pub agent_id: Option<String>,
    #[serde(default)]
    pub auto_rotate: bool,
}

#[derive(Debug, Clone, Deserialize)]
pub struct VaultListParams {
    #[serde(default)]
    pub secret_type: Option<String>,
}

#[cfg(any(test, feature = "vault-test-api"))]
#[derive(Debug, Clone, Deserialize)]
pub struct VaultRemoveParams {
    pub name: String,
    #[serde(default)]
    pub agent_id: Option<String>,
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

#[cfg(any(test, feature = "vault-test-api"))]
#[derive(Debug, Clone, Deserialize)]
pub struct VaultSetApiKeyPoolParams {
    /// Logical provider env name, e.g. OPENAI_API_KEY or ROUTER_API_KEY.
    pub prefix: String,
    /// Concrete key values. Stored as PREFIX_1, PREFIX_2, ...
    pub values: Vec<String>,
    #[serde(default)]
    pub agent_id: Option<String>,
    #[serde(default = "default_rotation_strategy")]
    pub strategy: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub allowed_agents: Option<Vec<String>>,
}

#[cfg(any(test, feature = "vault-test-api"))]
impl Drop for VaultSetApiKeyPoolParams {
    fn drop(&mut self) {
        for value in &mut self.values {
            crypto::zero_string(value);
        }
    }
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
