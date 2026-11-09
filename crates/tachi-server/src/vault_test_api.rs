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
pub use crate::vault_ops::account_bind::LaneSlotDecision;
pub use crate::vault_ops::{
    ProviderSecretScan, VaultLeaseApiKeyParams, VaultListParams, VaultRecordKeyResultParams,
    VaultSetupRotationParams, VaultUnlockParams, VAULT_MATERIALIZATION_INVALID_UTF8,
};
pub use memory_server_runtime::CachedVaultKey;

#[derive(Clone)]
pub struct VaultSecretFixture {
    pub name: String,
    pub value: String,
    pub secret_type: String,
    pub description: String,
    pub allowed_agents: Option<Vec<String>>,
}

/// Seed config and an unlocked key; no init validation, audit or publication policy.
pub(crate) fn seed_vault(server: &MemoryServer, password: &str) -> Result<(), String> {
    use base64::{engine::general_purpose::STANDARD as B64, Engine};
    let salt = vault_crypto::generate_salt();
    // Owner CLI tests use the strict stored-parameter reader too.
    let (key, kdf_params) = (
        vault_crypto::DerivedVaultKey::derive(password, &salt)?,
        vault_crypto::active_kdf_params_json(),
    );
    let now = chrono::Utc::now().to_rfc3339();
    let config = memcore::vault::VaultConfig {
        salt: B64.encode(salt),
        verifier: vault_crypto::create_verifier(key.bytes())?,
        kdf_algorithm: "argon2id".into(),
        kdf_params: kdf_params.into(),
        cipher: memcore::vault::VaultCipher::Aes256Gcm,
        created_at: now.clone(),
        updated_at: now,
    };
    server.with_global_store(|store| store.vault_set_config(&config).map_err(|e| e.to_string()))?;
    {
        let mut state = server.vault_write();
        state.key = Some(CachedVaultKey::copy_from(key.bytes()));
        state.unlock_time = Some(std::time::Instant::now());
        state.failed_attempts = (0, None);
    }
    crate::utils::write_owner_only_file_atomic(&fixture_password_path(server), password.as_bytes())
        .map_err(|error| error.to_string())
}

pub(crate) fn seed_vault_secret(
    server: &MemoryServer,
    fixture: VaultSecretFixture,
) -> Result<(), String> {
    let key = CachedVaultKey::copy_from(
        server
            .vault_read()
            .key
            .as_ref()
            .expect("seed unlocked vault")
            .bytes(),
    );
    let (encrypted_value, nonce) = vault_crypto::encrypt(key.bytes(), fixture.value.as_bytes())?;
    let now = chrono::Utc::now().to_rfc3339();
    server.with_global_store(|store| {
        store
            .vault_upsert_entry(&memcore::vault::VaultEntry {
                name: fixture.name,
                encrypted_value,
                nonce,
                secret_type: fixture.secret_type,
                description: fixture.description,
                allowed_agents: fixture.allowed_agents,
                created_at: now.clone(),
                updated_at: now,
                accessed_at: String::new(),
                access_count: 0,
            })
            .map_err(|error| error.to_string())
    })
}

pub(crate) fn bind_vault_slot(
    server: &MemoryServer,
    name: &str,
    value: &str,
    rebind: bool,
    description: &str,
    allowed_agents: Option<Vec<String>>,
    effective_agent_id: Option<&str>,
) -> Result<LaneSlotDecision, String> {
    let key = CachedVaultKey::copy_from(
        server
            .vault_read()
            .key
            .as_ref()
            .expect("unlocked fixture vault")
            .bytes(),
    );
    server.with_global_store(|store| {
        write_lane_slot_binding(
            store,
            key.bytes(),
            name,
            value,
            rebind,
            description,
            allowed_agents,
            effective_agent_id,
        )
    })
}

fn fixture_password_path(server: &MemoryServer) -> PathBuf {
    server
        .global_db_path_buf()
        .with_extension("fixture-password")
}

pub(crate) async fn owner_set_vault_secret(
    server: &MemoryServer,
    name: &str,
    value: &str,
    secret_type: Option<&str>,
    description: &str,
    rebind: bool,
) -> Result<(), String> {
    owner_secret_action(
        server,
        tachi_bootstrap::cli::VaultAction::Set {
            name: name.into(),
            secret_type: secret_type.map(str::to_owned),
            description: Some(description.into()),
            stdin_password: false,
            keychain: false,
            password_file: Some(fixture_password_path(server)),
            insecure_password_file: false,
            value_stdin: true,
            rebind,
        },
        value,
    )
    .await
}

pub(crate) async fn owner_set_vault_pool(
    server: &MemoryServer,
    prefix: &str,
    values: &[String],
    strategy: &str,
) -> Result<(), String> {
    owner_secret_action(
        server,
        tachi_bootstrap::cli::VaultAction::SetPool {
            prefix: prefix.into(),
            strategy: strategy.into(),
            description: None,
            stdin_password: false,
            keychain: false,
            password_file: Some(fixture_password_path(server)),
            insecure_password_file: false,
            values_stdin: true,
        },
        &values.join("\n"),
    )
    .await
}

pub(crate) async fn owner_remove_vault_secret(
    server: &MemoryServer,
    name: &str,
) -> Result<(), String> {
    owner_secret_action(
        server,
        tachi_bootstrap::cli::VaultAction::Remove {
            name: name.into(),
            stdin_password: false,
            keychain: false,
            password_file: Some(fixture_password_path(server)),
            insecure_password_file: false,
        },
        "",
    )
    .await
}

async fn owner_secret_action(
    server: &MemoryServer,
    action: tachi_bootstrap::cli::VaultAction,
    input: &str,
) -> Result<(), String> {
    let db = server.global_db_path_buf();
    crate::bootstrap::run_secret_action_with_reader(
        &db,
        db.parent().expect("fixture parent"),
        action,
        &mut std::io::Cursor::new(input.as_bytes()),
    )
    .await
    .map_err(|error| error.to_string())
}

#[cfg(test)]
impl MemoryServer {
    pub(crate) fn seed_vault(&self, password: &str) -> Result<(), String> {
        seed_vault(self, password)
    }
    pub(crate) fn seed_vault_secret(&self, fixture: VaultSecretFixture) -> Result<(), String> {
        seed_vault_secret(self, fixture)
    }
}

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
            aliases: Vec::new(),
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

    // --- Fixture seeding and live owner/domain operations ---

    pub fn seed_vault(&self, password: &str) -> Result<(), String> {
        seed_vault(self.inner(), password)
    }

    pub async fn vault_unlock(
        &self,
        params: Parameters<VaultUnlockParams>,
    ) -> Result<String, String> {
        crate::vault_ops::handle_vault_unlock(self.inner(), params.0).await
    }

    pub async fn vault_lock(&self) -> Result<String, String> {
        crate::vault_ops::handle_vault_lock(self.inner()).await
    }

    pub fn seed_vault_secret(&self, fixture: VaultSecretFixture) -> Result<(), String> {
        seed_vault_secret(self.inner(), fixture)
    }

    pub fn read_vault_secret(
        &self,
        name: &str,
        agent_id: Option<&str>,
        auto_rotate: bool,
    ) -> Result<String, String> {
        crate::vault_ops::read_unlocked_vault_secret(self.inner(), name, agent_id, auto_rotate)
    }

    pub fn bind_vault_slot(
        &self,
        name: &str,
        value: &str,
        rebind: bool,
    ) -> Result<LaneSlotDecision, String> {
        bind_vault_slot(
            self.inner(),
            name,
            value,
            rebind,
            "slot fixture",
            None,
            None,
        )
    }

    pub async fn owner_set_vault_secret(
        &self,
        name: &str,
        value: &str,
        secret_type: Option<&str>,
        description: &str,
        rebind: bool,
    ) -> Result<(), String> {
        owner_set_vault_secret(self.inner(), name, value, secret_type, description, rebind).await
    }

    pub async fn owner_set_vault_pool(
        &self,
        prefix: &str,
        values: &[String],
        strategy: &str,
    ) -> Result<(), String> {
        owner_set_vault_pool(self.inner(), prefix, values, strategy).await
    }

    pub async fn vault_list(&self, params: Parameters<VaultListParams>) -> Result<String, String> {
        crate::vault_ops::handle_vault_list(self.inner(), params.0).await
    }

    pub async fn owner_remove_vault_secret(&self, name: &str) -> Result<(), String> {
        owner_remove_vault_secret(self.inner(), name).await
    }

    pub async fn vault_status(&self) -> Result<String, String> {
        crate::vault_ops::handle_vault_status(self.inner()).await
    }

    pub async fn vault_setup_rotation(
        &self,
        params: Parameters<VaultSetupRotationParams>,
    ) -> Result<String, String> {
        crate::vault_ops::handle_vault_setup_rotation(self.inner(), params.0).await
    }

    pub async fn vault_record_key_result(
        &self,
        params: Parameters<VaultRecordKeyResultParams>,
    ) -> Result<String, String> {
        crate::vault_ops::handle_vault_record_key_result(self.inner(), params.0).await
    }

    pub async fn vault_lease_api_key(
        &self,
        params: Parameters<VaultLeaseApiKeyParams>,
    ) -> Result<String, String> {
        crate::vault_ops::handle_vault_lease_api_key(self.inner(), params.0).await
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

#[cfg(feature = "vault-test-api")]
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

#[cfg(feature = "vault-test-api")]
pub fn materialize_standalone_with_password_for_tests(
    llm: &tachi_llm::LlmClient,
    global_db_path: &Path,
    password: &str,
    after_vault_pools_resolved: Option<Box<dyn FnOnce() + Send>>,
) -> Result<tachi_llm::MaterializeReport, String> {
    crate::provider_config::materialize_standalone_with_password_for_tests(
        llm,
        global_db_path,
        password,
        after_vault_pools_resolved,
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

pub fn load_validated_unlocked_api_key_secret_pools_with_drops(
    server: &VaultTestServer,
) -> Result<ProviderSecretScan, String> {
    crate::vault_ops::load_validated_unlocked_api_key_secret_pools_with_drops(server.inner())
}

pub fn provider_env_keys() -> std::collections::HashSet<String> {
    crate::provider_config::provider_env_keys()
}

pub fn family_env_names_for_env_name(name: &str) -> Option<Vec<&'static str>> {
    crate::provider_config::key_registry::family_env_names_for_env_name(name)
}

pub fn decode_utf8_zeroizing(
    bytes: Vec<u8>,
    invalid_utf8_error: impl Into<String>,
) -> Result<String, String> {
    crate::vault_crypto::decode_utf8_zeroizing(bytes, invalid_utf8_error)
}

/// Exercise the same atomic binding operation as the owner CLI.
pub fn write_lane_slot_binding(
    store: &mut memcore::MemoryStore,
    master_key: &[u8; 32],
    name: &str,
    new_value: &str,
    rebind: bool,
    description: &str,
    allowed_agents: Option<Vec<String>>,
    effective_agent_id: Option<&str>,
) -> Result<LaneSlotDecision, String> {
    crate::vault_ops::account_bind::write_lane_slot_binding(
        store,
        master_key,
        name,
        new_value,
        rebind,
        description,
        allowed_agents,
        effective_agent_id,
    )
    .map(|(decision, _)| decision)
}
