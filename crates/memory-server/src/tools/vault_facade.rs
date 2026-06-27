use rmcp::handler::server::wrapper::Parameters;
use rmcp::{tool, tool_router};

use crate::vault_ops::{
    handle_vault_get, handle_vault_init, handle_vault_lease_api_key, handle_vault_list,
    handle_vault_lock, handle_vault_record_key_result, handle_vault_remove, handle_vault_set,
    handle_vault_set_api_key_pool, handle_vault_setup_rotation, handle_vault_status,
    handle_vault_unlock, VaultGetParams, VaultInitParams, VaultLeaseApiKeyParams, VaultListParams,
    VaultRecordKeyResultParams, VaultRemoveParams, VaultSetApiKeyPoolParams, VaultSetParams,
    VaultSetupRotationParams, VaultUnlockParams,
};
use crate::MemoryServer;

#[tool_router(router = vault_tool_router, vis = "pub(crate)")]
impl MemoryServer {
    #[tool(description = "Initialize the vault with a master password. Can only be called once.")]
    pub(crate) async fn vault_init(
        &self,
        Parameters(params): Parameters<VaultInitParams>,
    ) -> Result<String, String> {
        handle_vault_init(self, params).await
    }

    #[tool(description = "Unlock the vault by verifying the master password.")]
    pub(crate) async fn vault_unlock(
        &self,
        Parameters(params): Parameters<VaultUnlockParams>,
    ) -> Result<String, String> {
        handle_vault_unlock(self, params).await
    }

    #[tool(description = "Lock the vault (clear encryption key from memory).")]
    pub(crate) async fn vault_lock(&self) -> Result<String, String> {
        handle_vault_lock(self).await
    }

    #[tool(
        description = "Store or update an encrypted secret in the vault. Supports multi-key rotation when name ends with _N."
    )]
    pub(crate) async fn vault_set(
        &self,
        Parameters(params): Parameters<VaultSetParams>,
    ) -> Result<String, String> {
        handle_vault_set(self, params).await
    }

    #[tool(
        description = "Retrieve and decrypt a secret from the vault. Supports auto-rotation for multi-key secrets."
    )]
    pub(crate) async fn vault_get(
        &self,
        Parameters(params): Parameters<VaultGetParams>,
    ) -> Result<String, String> {
        handle_vault_get(self, params).await
    }

    #[tool(
        description = "List all stored secrets (names and metadata only, not values). Does not require vault to be unlocked."
    )]
    pub(crate) async fn vault_list(
        &self,
        Parameters(params): Parameters<VaultListParams>,
    ) -> Result<String, String> {
        handle_vault_list(self, params).await
    }

    #[tool(description = "Delete a secret from the vault.")]
    pub(crate) async fn vault_remove(
        &self,
        Parameters(params): Parameters<VaultRemoveParams>,
    ) -> Result<String, String> {
        handle_vault_remove(self, params).await
    }

    #[tool(description = "Check vault status (initialized, locked/unlocked, entry count).")]
    pub(crate) async fn vault_status(&self) -> Result<String, String> {
        handle_vault_status(self).await
    }

    #[tool(
        description = "Setup key rotation for a prefix. Requires keys like PREFIX_1, PREFIX_2, etc. to already exist."
    )]
    pub(crate) async fn vault_setup_rotation(
        &self,
        Parameters(params): Parameters<VaultSetupRotationParams>,
    ) -> Result<String, String> {
        handle_vault_setup_rotation(self, params).await
    }

    #[tool(
        description = "Store multiple provider API keys as one logical Vault pool. Values are encrypted as PREFIX_1, PREFIX_2, ... and rotation is configured under PREFIX."
    )]
    pub(crate) async fn vault_set_api_key_pool(
        &self,
        Parameters(params): Parameters<VaultSetApiKeyPoolParams>,
    ) -> Result<String, String> {
        handle_vault_set_api_key_pool(self, params).await
    }

    #[tool(
        description = "Lease one usable provider API key from Vault and return an env injection map. Skips disabled/auth-failed/exhausted/rate-limited keys."
    )]
    pub(crate) async fn vault_lease_api_key(
        &self,
        Parameters(params): Parameters<VaultLeaseApiKeyParams>,
    ) -> Result<String, String> {
        handle_vault_lease_api_key(self, params).await
    }

    #[tool(
        description = "Record a provider API key result into Vault health. HTTP 429 enters cooldown, 401/403 marks auth_failed, success clears errors, and future leases skip unhealthy keys."
    )]
    pub(crate) async fn vault_record_key_result(
        &self,
        Parameters(params): Parameters<VaultRecordKeyResultParams>,
    ) -> Result<String, String> {
        handle_vault_record_key_result(self, params).await
    }
}
