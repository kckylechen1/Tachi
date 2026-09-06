//! Feature-gated friend surface for `tachi-vault-tests` (#1831 Track T).
//!
//! This module is not part of the product API. It exposes controlled server,
//! vault facade, and fixture helpers for external vault integration tests
//! while keeping them uncompiled in ordinary product builds.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use rmcp::handler::server::wrapper::Parameters;

use crate::server_state::MemoryServer;

pub use crate::vault_crypto;
pub use crate::vault_ops::{
    ProviderSecretScan, VaultGetParams, VaultInitParams, VaultLeaseApiKeyParams, VaultListParams,
    VaultRecordKeyResultParams, VaultRemoveParams, VaultSetApiKeyPoolParams, VaultSetParams,
    VaultSetupRotationParams, VaultUnlockParams, VAULT_MATERIALIZATION_INVALID_UTF8,
};
pub use memory_server_runtime::CachedVaultKey;

/// Per-process run dir under `$TMPDIR/tachi-tests/run-<pid>-<uuid>/` with
/// automated GC for stale leftovers.
pub fn test_fixture_root() -> PathBuf {
    crate::utils::test_fixture_root()
}

/// Join `name` under [`test_fixture_root`].
pub fn test_fixture_path(name: impl AsRef<Path>) -> PathBuf {
    crate::utils::test_fixture_path(name)
}

#[derive(Debug)]
struct TempFixtureDir(PathBuf);

impl Drop for TempFixtureDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[derive(Clone)]
pub struct VaultTestServer {
    server: Option<MemoryServer>,
    _fixture_root: Arc<TempFixtureDir>,
    pub llm: Arc<tachi_llm::LlmClient>,
}

impl VaultTestServer {
    pub fn make_server() -> Self {
        let fixture_root =
            crate::utils::test_fixture_root().join(format!("vault-test-{}", uuid::Uuid::new_v4()));
        let fixture_home = fixture_root.join("home");
        let db_path = fixture_root
            .join("global")
            .join(memcore::MEMORY_DB_FILENAME);
        std::fs::create_dir_all(db_path.parent().expect("global db parent"))
            .expect("create parent");
        std::fs::create_dir_all(&fixture_home).expect("create fixture home");
        let server = MemoryServer::new_isolated_with_home_for_test(db_path, None, fixture_home)
            .expect("create test server");
        let llm = Arc::clone(&server.llm);
        Self {
            server: Some(server),
            _fixture_root: Arc::new(TempFixtureDir(fixture_root)),
            llm,
        }
    }

    pub fn make_server_with_project_fixture(project_name: &str) -> (Self, PathBuf) {
        let fixture_root =
            crate::utils::test_fixture_root().join(format!("vault-test-{}", uuid::Uuid::new_v4()));
        let fixture_home = fixture_root.join("home");
        let db_path = fixture_root
            .join("global")
            .join(memcore::MEMORY_DB_FILENAME);
        std::fs::create_dir_all(db_path.parent().expect("global db parent"))
            .expect("create parent");
        std::fs::create_dir_all(&fixture_home).expect("create fixture home");

        let project_db_path = fixture_root
            .join("project")
            .join(".tachi")
            .join(memcore::MEMORY_DB_FILENAME);
        std::fs::create_dir_all(project_db_path.parent().expect("project db parent"))
            .expect("create parent");

        let mut manifest = crate::manifest::Manifest::empty();
        manifest.dbs.push(crate::manifest::DbEntry {
            path: project_db_path.display().to_string(),
            role: crate::manifest::DbRole::Project,
            owner: "test".to_string(),
            schema_kind: "tachi".to_string(),
            vec_enabled: true,
            allow_write: true,
            last_doctor_at: chrono::Utc::now().to_rfc3339(),
            last_classification: "healthy".to_string(),
            scope_hint: format!("project:{project_name}"),
            notes: String::new(),
        });
        manifest
            .save(&fixture_home.join("manifest.json"))
            .expect("save fixture manifest");

        let server = MemoryServer::new_isolated_with_home_for_test(
            db_path,
            Some(project_db_path.clone()),
            fixture_home,
        )
        .expect("create test server");
        let llm = Arc::clone(&server.llm);

        (
            Self {
                server: Some(server),
                _fixture_root: Arc::new(TempFixtureDir(fixture_root)),
                llm,
            },
            project_db_path,
        )
    }

    #[inline]
    pub(crate) fn inner(&self) -> &MemoryServer {
        self.server.as_ref().expect("server present")
    }

    pub fn global_db_path_buf(&self) -> PathBuf {
        self.inner().global_db_path_buf()
    }

    pub fn project_db_path_buf(&self) -> Option<PathBuf> {
        self.inner().project_db_path_buf()
    }

    pub fn tachi_home_dir(&self) -> PathBuf {
        self.inner().tachi_home_dir()
    }

    pub fn replace_llm(&mut self, llm: tachi_llm::LlmClient) {
        let arc = Arc::new(llm);
        self.llm = Arc::clone(&arc);
        self.server.as_mut().expect("server present").llm = arc;
    }

    pub fn set_bound_agent_id_for_test(&self, agent_id: Option<&str>) {
        self.inner().set_bound_agent_id_for_test(agent_id);
    }

    pub fn set_agent_profile_for_test(&self, profile: Option<memory_server_runtime::AgentProfile>) {
        self.inner().agent_runtime_write().agent_profile = profile;
    }

    pub fn agent_runtime_write(
        &self,
    ) -> std::sync::RwLockWriteGuard<'_, crate::server_state::AgentRuntime> {
        self.inner().agent_runtime_write()
    }

    // --- Vault MCP Tool Facades ---

    pub async fn vault_init(&self, params: Parameters<VaultInitParams>) -> Result<String, String> {
        self.inner().vault_init(params).await
    }

    pub async fn vault_unlock(
        &self,
        params: Parameters<VaultUnlockParams>,
    ) -> Result<String, String> {
        self.inner().vault_unlock(params).await
    }

    pub async fn vault_lock(&self) -> Result<String, String> {
        self.inner().vault_lock().await
    }

    pub async fn vault_set(&self, params: Parameters<VaultSetParams>) -> Result<String, String> {
        self.inner().vault_set(params).await
    }

    pub async fn vault_get(&self, params: Parameters<VaultGetParams>) -> Result<String, String> {
        self.inner().vault_get(params).await
    }

    pub async fn vault_list(&self, params: Parameters<VaultListParams>) -> Result<String, String> {
        self.inner().vault_list(params).await
    }

    pub async fn vault_remove(
        &self,
        params: Parameters<VaultRemoveParams>,
    ) -> Result<String, String> {
        self.inner().vault_remove(params).await
    }

    pub async fn vault_status(&self) -> Result<String, String> {
        self.inner().vault_status().await
    }

    pub async fn vault_setup_rotation(
        &self,
        params: Parameters<VaultSetupRotationParams>,
    ) -> Result<String, String> {
        self.inner().vault_setup_rotation(params).await
    }

    pub async fn vault_record_key_result(
        &self,
        params: Parameters<VaultRecordKeyResultParams>,
    ) -> Result<String, String> {
        self.inner().vault_record_key_result(params).await
    }

    pub async fn vault_set_api_key_pool(
        &self,
        params: Parameters<VaultSetApiKeyPoolParams>,
    ) -> Result<String, String> {
        self.inner().vault_set_api_key_pool(params).await
    }

    pub async fn vault_lease_api_key(
        &self,
        params: Parameters<VaultLeaseApiKeyParams>,
    ) -> Result<String, String> {
        self.inner().vault_lease_api_key(params).await
    }

    // --- Vault State Inspection & Mutation ---

    pub fn vault_read(&self) -> std::sync::RwLockReadGuard<'_, memory_server_runtime::VaultState> {
        self.inner().vault_read()
    }

    pub fn vault_write(
        &self,
    ) -> std::sync::RwLockWriteGuard<'_, memory_server_runtime::VaultState> {
        self.inner().vault_write()
    }

    pub fn clear_cached_vault_key(&self) {
        let mut vault = self.inner().vault_write();
        vault.key = None;
        vault.unlock_time = None;
    }

    pub fn unlocked_key_bytes(&self) -> [u8; 32] {
        let vault = self.inner().vault_read();
        *vault.key.as_ref().expect("unlocked key").bytes()
    }

    // --- Store Access ---

    pub fn with_global_store<T>(
        &self,
        f: impl FnOnce(&mut memcore::MemoryStore) -> Result<T, String>,
    ) -> Result<T, String> {
        self.inner().with_global_store(f)
    }

    pub fn with_global_store_read<T>(
        &self,
        f: impl FnOnce(&mut memcore::MemoryStore) -> Result<T, String>,
    ) -> Result<T, String> {
        self.inner().with_global_store_read(f)
    }

    pub fn with_project_store<T>(
        &self,
        f: impl FnOnce(&mut memcore::MemoryStore) -> Result<T, String>,
    ) -> Result<T, String> {
        self.inner().with_project_store(f)
    }

    pub fn refresh_llm_provider_secrets_from_vault(
        &self,
    ) -> Result<tachi_llm::MaterializeReport, String> {
        self.inner().refresh_llm_provider_secrets_from_vault()
    }
}

pub fn apply_unlocked_vault_env(
    cmd: &mut tokio::process::Command,
    server: &VaultTestServer,
    root: Option<&Path>,
) -> usize {
    crate::dispatch_ops::apply_unlocked_vault_env(cmd, server.inner(), root)
}

pub fn lease_api_key_from_store(
    store: &memcore::MemoryStore,
    key: &[u8; 32],
    logical_name: &str,
) -> Result<(String, String, String), Box<dyn std::error::Error>> {
    crate::bootstrap::lease_api_key_from_store(store, key, logical_name)
}

pub fn materialize_for_server(
    server: &VaultTestServer,
) -> Result<tachi_llm::MaterializeReport, String> {
    crate::provider_config::materialize_for_server(server.inner())
}

pub fn materialize_for_server_without_keychain_for_tests(
    server: &VaultTestServer,
) -> Result<tachi_llm::MaterializeReport, String> {
    crate::provider_config::materialize_for_server_without_keychain_for_tests(server.inner())
}

pub fn materialize_for_server_with_hook_for_tests(
    server: &VaultTestServer,
    after_vault_pools_resolved: impl FnOnce() + Send + 'static,
) -> Result<tachi_llm::MaterializeReport, String> {
    crate::provider_config::materialize_for_server_with_hook_for_tests(
        server.inner(),
        after_vault_pools_resolved,
    )
}

pub fn materialize_standalone_with_password_for_tests(
    llm: &tachi_llm::LlmClient,
    global_db_path: &Path,
    password: &str,
) -> Result<tachi_llm::MaterializeReport, String> {
    crate::provider_config::materialize_standalone_with_password_for_tests(
        llm,
        global_db_path,
        password,
    )
}

pub fn format_skipped_alias_warning(
    key: &str,
    retained: bool,
    class: tachi_llm::AliasSkipClass,
) -> String {
    crate::provider_config::format_skipped_alias_warning(key, retained, class)
}

pub fn load_unlocked_api_key_secret_pools(
    server: &VaultTestServer,
) -> Result<HashMap<String, Vec<tachi_llm::ProviderSecret>>, String> {
    crate::vault_ops::load_unlocked_api_key_secret_pools(server.inner())
}

pub fn load_unlocked_api_key_secret_pools_with_drops(
    server: &VaultTestServer,
) -> Result<ProviderSecretScan, String> {
    crate::vault_ops::load_unlocked_api_key_secret_pools_with_drops(server.inner())
}
