use crate::server_state::{CachedVaultKey, MemoryServer};
use crate::vault_crypto as crypto;
use base64::{engine::general_purpose::STANDARD as B64, Engine};
use chrono::Utc;
use memory_core::vault::{
    normalize_secret_type, VaultCipher, VaultConfig, VaultEntry, VaultKeyRotation,
    SECRET_TYPE_API_KEY,
};
use serde_json::json;
use std::sync::Arc;
use std::time::Instant;

use super::access::{
    authorize_vault_mutation, authorize_vault_pool_mutation, ensure_agent_allowed,
    load_unlocked_api_key_secret_pool, record_successful_vault_access, resolve_vault_acl_agent_id,
    select_vault_entry,
};
use super::audit::{record_vault_audit, result_with_vault_audit_warning};
use super::env::attach_provider_refresh_warning;
use super::params::{
    VaultGetParams, VaultInitParams, VaultLeaseApiKeyParams, VaultListParams,
    VaultRecordKeyResultParams, VaultRemoveParams, VaultSetApiKeyPoolParams, VaultSetParams,
    VaultSetupRotationParams, VaultUnlockParams,
};
use super::rotation::{
    collect_rotation_entries, normalize_allowed_agents, normalize_rotation_strategy,
};
use super::session::{
    clear_cached_vault_state, ensure_vault_unlock_allowed, is_vault_initialized,
    maybe_auto_lock_vault, read_unlock_password_fifo, record_vault_unlock_failure, with_vault_key,
};

mod health;
mod lifecycle;
mod listing;
mod pool;
mod secrets;

pub(crate) use self::health::handle_vault_record_key_result;
pub(crate) use self::lifecycle::{handle_vault_init, handle_vault_lock, handle_vault_unlock};
pub(crate) use self::listing::{handle_vault_list, handle_vault_remove, handle_vault_status};
pub(crate) use self::pool::{
    handle_vault_lease_api_key, handle_vault_set_api_key_pool, handle_vault_setup_rotation,
};
pub(crate) use self::secrets::{handle_vault_get, handle_vault_set};
