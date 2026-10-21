//! The capabilities Vault code needs from its host server.
//!
//! Vault code takes `&impl VaultCtx` instead of the concrete server type, so
//! the surface it can reach is this trait and nothing more. The server
//! implements it in `server_state::capabilities`.

use tachi_llm::llm_recorder::LlmCallRecorder;
use tachi_llm::LlmClient;
pub(crate) use tachi_runtime_core::{LlmAccess, RuntimeHome, StoreAccess, VaultAccess};

use crate::server_state::VaultState;
use tachi_llm::MaterializeReport;

pub(crate) trait VaultCtx:
    RuntimeHome
    + StoreAccess
    + VaultAccess<State = VaultState>
    + LlmAccess<Client = LlmClient, Recorder = LlmCallRecorder>
{
    /// Agent identity this server is bound to, for Vault ACL checks.
    fn bound_agent_id(&self) -> Option<String>;
    /// Re-materialize provider secrets after the Vault key changes.
    fn refresh_llm_provider_secrets_from_vault(&self) -> Result<MaterializeReport, String>;
    /// Requeue enrichment work that failed on missing credentials.
    fn requeue_auth_failed_enrichment_retries(&self, trigger: &str) -> usize;
    /// Keychain auto-unlock that installs the key only; the caller owns the
    /// provider refresh.
    fn auto_unlock_vault_key_from_keychain(&self) -> Result<bool, String>;
    /// Keychain auto-unlock followed by one provider refresh.
    fn auto_unlock_vault_from_keychain(&self) -> Result<bool, String>;
}
