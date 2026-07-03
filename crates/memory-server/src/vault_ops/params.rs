use crate::vault_crypto as crypto;
use memory_core::vault::SECRET_TYPE_API_KEY;
use rmcp::schemars::{self, JsonSchema};
use serde::Deserialize;

fn default_secret_type() -> String {
    SECRET_TYPE_API_KEY.to_string()
}

#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub(crate) struct VaultInitParams {
    pub password: String,
}

impl Drop for VaultInitParams {
    fn drop(&mut self) {
        crypto::zero_string(&mut self.password);
    }
}

#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub(crate) struct VaultUnlockParams {
    #[serde(default)]
    pub password: String,
    #[serde(default)]
    pub password_fifo_path: Option<String>,
}

impl Drop for VaultUnlockParams {
    fn drop(&mut self) {
        crypto::zero_string(&mut self.password);
    }
}

#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub(crate) struct VaultSetParams {
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
}

#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub(crate) struct VaultGetParams {
    pub name: String,
    #[serde(default)]
    pub agent_id: Option<String>,
    #[serde(default)]
    pub auto_rotate: bool,
}

#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub(crate) struct VaultListParams {
    #[serde(default)]
    pub secret_type: Option<String>,
}

#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub(crate) struct VaultRemoveParams {
    pub name: String,
    #[serde(default)]
    pub agent_id: Option<String>,
}

#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub(crate) struct VaultSetupRotationParams {
    pub prefix: String,
    pub total_keys: i64,
    #[serde(default)]
    pub agent_id: Option<String>,
    #[serde(default = "default_rotation_strategy")]
    pub strategy: String,
}

#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub(crate) struct VaultSetApiKeyPoolParams {
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

#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub(crate) struct VaultLeaseApiKeyParams {
    /// Logical provider env name or standalone API key name.
    pub name: String,
    /// Optional child env var name. Defaults to `name`.
    #[serde(default)]
    pub env_name: Option<String>,
    /// Optional agent id for future restricted-secret checks.
    #[serde(default)]
    pub agent_id: Option<String>,
}

#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub(crate) struct VaultRecordKeyResultParams {
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
