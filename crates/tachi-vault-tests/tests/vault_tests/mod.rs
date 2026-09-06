//! External integration test suite for Tachi Vault (#1831 Track T).

pub use chrono::Utc;
pub use rmcp::handler::server::wrapper::Parameters;
pub use serde_json::{json, Value};
pub use std::time::{Duration, Instant};

pub use tachi_server::vault_test_api::*;

pub type TestServer = VaultTestServer;
pub type MemoryServer = VaultTestServer;

pub fn make_server() -> TestServer {
    VaultTestServer::make_server()
}

pub fn make_server_with_project_fixture(project_name: &str) -> (TestServer, std::path::PathBuf) {
    VaultTestServer::make_server_with_project_fixture(project_name)
}

pub mod tests {
    pub use super::{make_server, make_server_with_project_fixture, TestServer};
}

pub mod utils {
    use std::sync::Mutex;

    pub fn global_test_lock() -> &'static Mutex<()> {
        static LOCK: Mutex<()> = Mutex::new(());
        &LOCK
    }
}

pub mod test_support {
    use std::path::Path;

    pub struct EnvRestore {
        key: &'static str,
        old: Option<std::ffi::OsString>,
    }

    impl EnvRestore {
        pub fn set(key: &'static str, value: &str) -> Self {
            let old = std::env::var_os(key);
            std::env::set_var(key, value);
            Self { key, old }
        }

        pub fn set_path(key: &'static str, value: &Path) -> Self {
            let old = std::env::var_os(key);
            std::env::set_var(key, value);
            Self { key, old }
        }

        pub fn remove(key: &'static str) -> Self {
            let old = std::env::var_os(key);
            std::env::remove_var(key);
            Self { key, old }
        }
    }

    impl Drop for EnvRestore {
        fn drop(&mut self) {
            if let Some(value) = self.old.as_ref() {
                std::env::set_var(self.key, value);
            } else {
                std::env::remove_var(self.key);
            }
        }
    }

    pub fn with_unrestricted_fixture_connection<T>(
        path: &Path,
        operation: impl FnOnce(&rusqlite::Connection) -> rusqlite::Result<T>,
    ) -> rusqlite::Result<T> {
        let connection = rusqlite::Connection::open(path)?;
        operation(&connection)
    }
}

pub mod dispatch_ops {
    pub use super::apply_unlocked_vault_env;
}

pub mod bootstrap {
    pub use super::lease_api_key_from_store;
}

pub mod server_state {
    pub use super::CachedVaultKey;
}

pub mod provider_config {
    pub use tachi_server::vault_test_api::{
        format_skipped_alias_warning, materialize_for_server,
        materialize_for_server_with_hook_for_tests,
        materialize_for_server_without_keychain_for_tests,
        materialize_standalone_with_password_for_tests, provider_env_keys,
    };
}

pub mod vault_ops {
    pub use tachi_llm::is_lane_slot_secret_name;
    pub use tachi_server::vault_test_api::{
        load_unlocked_api_key_secret_pools, load_unlocked_api_key_secret_pools_with_drops,
        load_validated_unlocked_api_key_secret_pools_with_drops,
        VAULT_MATERIALIZATION_INVALID_UTF8,
    };

    pub mod account_bind {
        pub use tachi_server::vault_test_api::write_lane_slot_binding;
    }
}

pub mod status_ops {
    pub mod status_health {
        pub use tachi_server::vault_test_api::family_env_names_for_env_name;
    }
}

pub mod vault_crypto {
    pub use tachi_server::vault_crypto::*;
    pub use tachi_server::vault_test_api::decode_utf8_zeroizing;
}

mod access_audit;
mod api_key_pool;
mod cached_key;
mod env_injection;
mod lifecycle;
mod rotation;
