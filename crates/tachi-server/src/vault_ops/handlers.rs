use crate::vault_crypto as crypto;
use crate::vault_ops::ctx::*;
#[cfg(any(test, feature = "vault-test-api"))]
use base64::{engine::general_purpose::STANDARD as B64, Engine};
use chrono::Utc;
use memcore::vault::{normalize_secret_type, VaultKeyRotation};
#[cfg(any(test, feature = "vault-test-api"))]
use memcore::vault::{VaultCipher, VaultConfig, VaultEntry, SECRET_TYPE_API_KEY};
use memory_server_runtime::CachedVaultKey;
use serde_json::json;
use std::sync::Arc;
use std::time::Instant;

#[cfg(any(test, feature = "vault-test-api"))]
use super::access::{authorize_vault_mutation, select_authorized_vault_entry_and_record_access};
use super::access::{
    authorize_vault_pool_mutation, ensure_agent_allowed, lease_authorized_api_key,
    resolve_vault_acl_agent_id,
};
use super::audit::{record_vault_audit, result_with_vault_audit_warning};
use super::env::attach_provider_refresh_warning;
#[cfg(any(test, feature = "vault-test-api"))]
use super::params::{
    VaultGetParams, VaultInitParams, VaultRemoveParams, VaultSetApiKeyPoolParams, VaultSetParams,
};
use super::params::{
    VaultLeaseApiKeyParams, VaultListParams, VaultRecordKeyResultParams, VaultSetupRotationParams,
    VaultUnlockParams,
};
#[cfg(any(test, feature = "vault-test-api"))]
use super::rotation::normalize_allowed_agents;
use super::rotation::normalize_rotation_strategy;
#[cfg(any(test, feature = "vault-test-api"))]
use super::session::with_vault_key;
use super::session::{
    clear_cached_vault_state, ensure_vault_unlock_allowed, is_vault_initialized,
    maybe_auto_lock_vault, read_unlock_password_fifo, record_vault_unlock_failure,
};
#[cfg(any(test, feature = "vault-test-api"))]
use super::slot_rebind::{is_lane_slot_secret_name, validate_lane_slot_secret_type};

mod health;
mod lifecycle;
mod listing;
mod pool;
#[cfg(any(test, feature = "vault-test-api"))]
mod secrets;

pub(crate) use self::health::handle_vault_record_key_result;
#[cfg(any(test, feature = "vault-test-api"))]
pub(crate) use self::lifecycle::handle_vault_init;
pub(crate) use self::lifecycle::{handle_vault_lock, handle_vault_unlock};
#[cfg(any(test, feature = "vault-test-api"))]
pub(crate) use self::listing::handle_vault_remove;
pub(crate) use self::listing::{build_vault_list_payload, handle_vault_list, handle_vault_status};
#[cfg(any(test, feature = "vault-test-api"))]
pub(crate) use self::pool::handle_vault_set_api_key_pool;
pub(crate) use self::pool::{handle_vault_lease_api_key, handle_vault_setup_rotation};
#[cfg(any(test, feature = "vault-test-api"))]
pub(crate) use self::secrets::{handle_vault_get, handle_vault_set};
