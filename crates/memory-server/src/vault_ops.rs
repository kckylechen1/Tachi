// vault_ops.rs — MCP tool handlers for Tachi Vault

mod access;
mod audit;
mod env;
mod handlers;
mod params;
mod rotation;
mod session;

pub(crate) use access::load_unlocked_api_key_secret_pools;
pub(crate) use access::read_unlocked_vault_secret;
pub(crate) use env::load_unlocked_env_secrets_for_child_env;
pub(crate) use handlers::{
    handle_vault_get, handle_vault_init, handle_vault_lease_api_key, handle_vault_list,
    handle_vault_lock, handle_vault_record_key_result, handle_vault_remove, handle_vault_set,
    handle_vault_set_api_key_pool, handle_vault_setup_rotation, handle_vault_status,
    handle_vault_unlock,
};
pub(crate) use params::{
    VaultGetParams, VaultInitParams, VaultLeaseApiKeyParams, VaultListParams,
    VaultRecordKeyResultParams, VaultRemoveParams, VaultSetApiKeyPoolParams, VaultSetParams,
    VaultSetupRotationParams, VaultUnlockParams,
};

#[cfg(test)]
mod tests;
