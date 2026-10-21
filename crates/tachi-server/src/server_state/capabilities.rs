//! Capability-trait implementations for `MemoryServer` (#2077 PR C).
//!
//! Pure delegation to the existing inherent methods and fields — no
//! locking, ordering, or behavior change. No caller is converted to the
//! traits yet; that is the next leaf, one domain per PR.

use std::path::PathBuf;
use std::sync::Arc;

use memcore::MemoryStore;
use tachi_llm::llm_recorder::LlmCallRecorder;
use tachi_llm::LlmClient;
use tachi_runtime_core::{LlmAccess, RuntimeHome, StoreAccess, VaultAccess};

use crate::vault_ops::VaultCtx;

use super::{DbScope, MemoryServer, VaultState};

impl RuntimeHome for MemoryServer {
    fn tachi_home_dir(&self) -> PathBuf {
        MemoryServer::tachi_home_dir(self)
    }
    fn global_db_path_buf(&self) -> PathBuf {
        MemoryServer::global_db_path_buf(self)
    }
    fn project_db_path_buf(&self) -> Option<PathBuf> {
        MemoryServer::project_db_path_buf(self)
    }
    fn has_project_db(&self) -> bool {
        MemoryServer::has_project_db(self)
    }
}

impl StoreAccess for MemoryServer {
    type Scope = DbScope;

    fn with_global_store<T>(
        &self,
        f: impl FnOnce(&mut MemoryStore) -> Result<T, String>,
    ) -> Result<T, String> {
        MemoryServer::with_global_store(self, f)
    }
    fn with_global_store_read<T>(
        &self,
        f: impl FnOnce(&mut MemoryStore) -> Result<T, String>,
    ) -> Result<T, String> {
        MemoryServer::with_global_store_read(self, f)
    }
    fn with_project_store<T>(
        &self,
        f: impl FnOnce(&mut MemoryStore) -> Result<T, String>,
    ) -> Result<T, String> {
        MemoryServer::with_project_store(self, f)
    }
    fn with_project_store_read<T>(
        &self,
        f: impl FnOnce(&mut MemoryStore) -> Result<T, String>,
    ) -> Result<T, String> {
        MemoryServer::with_project_store_read(self, f)
    }
    fn with_named_project_store<T>(
        &self,
        project_name: &str,
        f: impl FnOnce(&mut MemoryStore) -> Result<T, String>,
    ) -> Result<T, String> {
        MemoryServer::with_named_project_store(self, project_name, f)
    }
    fn with_named_project_store_read<T>(
        &self,
        project_name: &str,
        f: impl FnOnce(&mut MemoryStore) -> Result<T, String>,
    ) -> Result<T, String> {
        MemoryServer::with_named_project_store_read(self, project_name, f)
    }
    fn with_store_for_scope<T>(
        &self,
        scope: Self::Scope,
        f: impl FnOnce(&mut MemoryStore) -> Result<T, String>,
    ) -> Result<T, String> {
        MemoryServer::with_store_for_scope(self, scope, f)
    }
    fn with_store_for_scope_read<T>(
        &self,
        scope: Self::Scope,
        f: impl FnOnce(&mut MemoryStore) -> Result<T, String>,
    ) -> Result<T, String> {
        MemoryServer::with_store_for_scope_read(self, scope, f)
    }
    fn with_path_store<T>(
        &self,
        db_path: &std::path::Path,
        f: impl FnOnce(&mut MemoryStore) -> Result<T, String>,
    ) -> Result<T, String> {
        MemoryServer::with_path_store(self, db_path, f)
    }
    fn with_path_store_read<T>(
        &self,
        db_path: &std::path::Path,
        f: impl FnOnce(&mut MemoryStore) -> Result<T, String>,
    ) -> Result<T, String> {
        MemoryServer::with_path_store_read(self, db_path, f)
    }
}

impl LlmAccess for MemoryServer {
    type Client = LlmClient;
    type Recorder = LlmCallRecorder;

    fn llm(&self) -> &Arc<LlmClient> {
        &self.llm
    }
    fn llm_recorder(&self) -> &Arc<LlmCallRecorder> {
        &self.llm_recorder
    }
}

impl VaultAccess for MemoryServer {
    type State = VaultState;

    fn vault_read(&self) -> std::sync::RwLockReadGuard<'_, Self::State> {
        MemoryServer::vault_read(self)
    }
    fn vault_write(&self) -> std::sync::RwLockWriteGuard<'_, Self::State> {
        MemoryServer::vault_write(self)
    }
}

impl VaultCtx for MemoryServer {
    fn bound_agent_id(&self) -> Option<String> {
        MemoryServer::bound_agent_id(self)
    }
    fn refresh_llm_provider_secrets_from_vault(
        &self,
    ) -> Result<tachi_llm::MaterializeReport, String> {
        MemoryServer::refresh_llm_provider_secrets_from_vault(self)
    }
    fn requeue_auth_failed_enrichment_retries(&self, trigger: &str) -> usize {
        MemoryServer::requeue_auth_failed_enrichment_retries(self, trigger)
    }
    fn auto_unlock_vault_key_from_keychain(&self) -> Result<bool, String> {
        crate::provider_config::auto_unlock_vault_key_from_keychain(self)
    }
    fn auto_unlock_vault_from_keychain(&self) -> Result<bool, String> {
        crate::provider_config::auto_unlock_vault_from_keychain(self)
    }
}
