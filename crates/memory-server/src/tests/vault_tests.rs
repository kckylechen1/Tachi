use super::make_server;
use crate::server_state::CachedVaultKey;
use crate::vault_crypto;
use crate::vault_ops::{
    VaultGetParams, VaultInitParams, VaultLeaseApiKeyParams, VaultListParams,
    VaultRecordKeyResultParams, VaultRemoveParams, VaultSetApiKeyPoolParams, VaultSetParams,
    VaultSetupRotationParams, VaultUnlockParams,
};
use rmcp::handler::server::wrapper::Parameters;
use serde_json::{json, Value};
use std::time::{Duration, Instant};

mod access_audit;
mod api_key_pool;
mod cached_key;
mod env_injection;
mod lifecycle;
mod rotation;
