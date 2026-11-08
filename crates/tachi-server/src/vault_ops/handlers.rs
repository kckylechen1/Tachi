use crate::vault_crypto as crypto;
use crate::vault_ops::ctx::*;
use chrono::Utc;
use memcore::vault::{normalize_secret_type, VaultKeyRotation};
use memory_server_runtime::CachedVaultKey;
use serde_json::json;
use std::sync::Arc;
use std::time::Instant;

use super::access::{
    authorize_vault_pool_mutation, ensure_agent_allowed, lease_authorized_api_key,
    resolve_vault_acl_agent_id,
};
use super::audit::{record_vault_audit, result_with_vault_audit_warning};
use super::env::attach_provider_refresh_warning;
use super::params::{
    VaultLeaseApiKeyParams, VaultListParams, VaultRecordKeyResultParams, VaultSetupRotationParams,
    VaultUnlockParams,
};
use super::rotation::normalize_rotation_strategy;
use super::session::{
    clear_cached_vault_state, ensure_vault_unlock_allowed, is_vault_initialized,
    maybe_auto_lock_vault, read_unlock_password_fifo, record_vault_unlock_failure,
};

mod health;
mod lifecycle;
mod listing;
mod pool;

pub(crate) use self::health::handle_vault_record_key_result;
pub(crate) use self::lifecycle::{handle_vault_lock, handle_vault_unlock};
pub(crate) use self::listing::{build_vault_list_payload, handle_vault_list, handle_vault_status};
pub(crate) use self::pool::{handle_vault_lease_api_key, handle_vault_setup_rotation};
