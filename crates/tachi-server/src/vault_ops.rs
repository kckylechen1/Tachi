// vault_ops.rs — MCP tool handlers for Tachi Vault

mod access;
pub(crate) mod account_bind;
pub(crate) mod account_events;
mod alias_integrity;
mod audit;
mod env;
mod handlers;
pub mod params;
mod resolver;
mod rotation;
mod session;
mod slot_rebind;

pub(crate) use account_bind::is_lane_slot_secret_name;
pub(crate) use alias_integrity::unusable_skip_class;
pub(crate) use resolver::{classify_vault_read_error, is_env_fallback_eligible, VaultReadState};
pub(crate) use rotation::collect_rotation_entries;

pub(crate) use access::canonical_api_key_health_logical_name;
pub(crate) use access::is_lane_config_name;
pub(crate) use access::load_unlocked_api_key_secret_pools;
#[cfg(feature = "vault-test-api")]
pub(crate) use access::load_unlocked_api_key_secret_pools_with_drops;
pub(crate) use access::load_validated_unlocked_api_key_secret_pools_with_drops;
#[cfg(feature = "vault-test-api")]
pub use access::ProviderSecretScan;
pub(crate) use access::{
    materialize_unrestricted_vault_entries_from_store, read_usable_vault_secret_from_store_direct,
};
pub(crate) use access::{
    vault_materialization_acl_revision_from_rows, VaultMaterializationRevision,
};

/// Public-safe failure for background Vault materialization. The raw UTF-8
/// decoder error carries byte offsets/lengths, and the scanned entry name may
/// be an alias target rather than the operator-visible config key (#1854).
pub const VAULT_MATERIALIZATION_INVALID_UTF8: &str =
    "A listed Vault payload is not valid UTF-8; refusing materialization";
pub(crate) use access::read_unlocked_vault_secret;
pub(crate) use env::{
    load_unlocked_env_secrets_for_child_env, load_unlocked_env_secrets_for_child_env_with_consumer,
};
pub(crate) use handlers::{
    handle_vault_get, handle_vault_init, handle_vault_lease_api_key, handle_vault_list,
    handle_vault_lock, handle_vault_record_key_result, handle_vault_remove, handle_vault_set,
    handle_vault_set_api_key_pool, handle_vault_setup_rotation, handle_vault_status,
    handle_vault_unlock,
};
pub use params::{
    VaultGetParams, VaultInitParams, VaultLeaseApiKeyParams, VaultListParams,
    VaultRecordKeyResultParams, VaultRemoveParams, VaultSetApiKeyPoolParams, VaultSetParams,
    VaultSetupRotationParams, VaultUnlockParams,
};
pub(crate) use slot_rebind::{
    validate_existing_lane_slot_secret_type, validate_lane_slot_secret_type,
};

#[cfg(test)]
mod tests;
