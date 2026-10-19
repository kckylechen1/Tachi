//! Capability traits for the `MemoryServer` runtime seam (#2077 PR C).
//!
//! These traits exist so domain code can depend on narrow capabilities
//! instead of `&MemoryServer`; `MemoryServer` implements all of them by
//! pure delegation (see `tachi-server`'s `server_state::capabilities`).
//! No handler is converted to the traits yet — that is the next leaf.
//!
//! `StoreAccess::Scope`, `VaultAccess::State` and the `LlmAccess` client and
//! recorder are associated types so this crate stays a leaf: `DbScope` and
//! `VaultState` live in `memory-server-runtime`, and the LLM types in
//! `tachi-llm`, neither of which core may depend on. `LlmAccess` exposes
//! field references (`llm`/`llm_recorder` are fields on `MemoryServer`).

use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLockReadGuard, RwLockWriteGuard};

use memcore::MemoryStore;

/// Home-directory and DB-path resolution.
pub trait RuntimeHome {
    fn tachi_home_dir(&self) -> PathBuf;
    fn global_db_path_buf(&self) -> PathBuf;
    fn project_db_path_buf(&self) -> Option<PathBuf>;
    fn has_project_db(&self) -> bool;
}

/// Store access doors, matching the inherent `MemoryServer` methods.
pub trait StoreAccess {
    /// Scope enum for the `with_store_for_scope` family
    /// (`memory_server_runtime::DbScope` on `MemoryServer`).
    type Scope;

    fn with_global_store<T>(
        &self,
        f: impl FnOnce(&mut MemoryStore) -> Result<T, String>,
    ) -> Result<T, String>;
    fn with_global_store_read<T>(
        &self,
        f: impl FnOnce(&mut MemoryStore) -> Result<T, String>,
    ) -> Result<T, String>;
    fn with_project_store<T>(
        &self,
        f: impl FnOnce(&mut MemoryStore) -> Result<T, String>,
    ) -> Result<T, String>;
    fn with_project_store_read<T>(
        &self,
        f: impl FnOnce(&mut MemoryStore) -> Result<T, String>,
    ) -> Result<T, String>;
    fn with_named_project_store<T>(
        &self,
        project_name: &str,
        f: impl FnOnce(&mut MemoryStore) -> Result<T, String>,
    ) -> Result<T, String>;
    fn with_named_project_store_read<T>(
        &self,
        project_name: &str,
        f: impl FnOnce(&mut MemoryStore) -> Result<T, String>,
    ) -> Result<T, String>;
    fn with_store_for_scope<T>(
        &self,
        scope: Self::Scope,
        f: impl FnOnce(&mut MemoryStore) -> Result<T, String>,
    ) -> Result<T, String>;
    fn with_store_for_scope_read<T>(
        &self,
        scope: Self::Scope,
        f: impl FnOnce(&mut MemoryStore) -> Result<T, String>,
    ) -> Result<T, String>;
    fn with_path_store<T>(
        &self,
        db_path: &Path,
        f: impl FnOnce(&mut MemoryStore) -> Result<T, String>,
    ) -> Result<T, String>;
    fn with_path_store_read<T>(
        &self,
        db_path: &Path,
        f: impl FnOnce(&mut MemoryStore) -> Result<T, String>,
    ) -> Result<T, String>;
}

/// LLM client and call-recorder access.
pub trait LlmAccess {
    /// `tachi_llm::LlmClient` on `MemoryServer`.
    type Client;
    /// `tachi_llm::llm_recorder::LlmCallRecorder` on `MemoryServer`.
    type Recorder;

    fn llm(&self) -> &Arc<Self::Client>;
    fn llm_recorder(&self) -> &Arc<Self::Recorder>;
}

/// Vault lock access.
pub trait VaultAccess {
    /// Vault state type (`memory_server_runtime::VaultState` on
    /// `MemoryServer`).
    type State;

    fn vault_read(&self) -> RwLockReadGuard<'_, Self::State>;
    fn vault_write(&self) -> RwLockWriteGuard<'_, Self::State>;
}
