use super::cache::CachedResult;
use super::runtime::{AgentRuntime, EnrichmentRuntime, FoundryRuntime};
use super::tachi_server::MemoryServer;
use super::{RateLimiter, VaultState};
use crate::foundry_runtime_ops::FoundryMaintenanceItem;
use crate::shared_defs::DeadLetter;
use crate::utils::{lock_or_recover, read_or_recover, write_or_recover};
use crate::vault_ops::load_unlocked_env_secrets_for_child_env;
use memcore::MemoryStore;
use memory_server_runtime::{event_db_route, EventDbRoute};
use std::collections::{HashMap, VecDeque};
use std::path::PathBuf;
use tokio::sync::mpsc;

impl MemoryServer {
    pub(crate) fn set_work_claim_connection(
        &self,
        agent_identity_id: Option<String>,
        connection_id: String,
        admission: String,
    ) {
        self.agent_runtime_write().work_claim_connection =
            Some(super::runtime::WorkClaimConnection {
                agent_identity_id,
                connection_id,
                admission,
            });
    }

    pub(crate) fn work_claim_connection(&self) -> Option<(Option<String>, String, String)> {
        self.agent_runtime_read()
            .work_claim_connection
            .as_ref()
            .map(|connection| {
                (
                    connection.agent_identity_id.clone(),
                    connection.connection_id.clone(),
                    connection.admission.clone(),
                )
            })
    }

    pub(crate) fn tool_cache_lock(
        &self,
    ) -> std::sync::MutexGuard<'_, HashMap<String, CachedResult>> {
        lock_or_recover(&self.tool_discovery.tool_cache, "tool_cache")
    }

    pub(crate) fn dead_letters_lock(&self) -> std::sync::MutexGuard<'_, VecDeque<DeadLetter>> {
        lock_or_recover(&self.tool_discovery.dead_letters, "dead_letters")
    }

    #[cfg(test)]
    pub(crate) fn global_read_pool_size_for_tests(&self) -> usize {
        self.db.global_read_pool_size()
    }

    pub(crate) fn refresh_llm_provider_secrets_from_vault(&self) -> Result<usize, String> {
        crate::provider_config::materialize_for_server(self).map(|report| {
            if report.from_alias > 0 || report.env_fallbacks_bypassed > 0 {
                tracing::info!(
                    "[provider] materialized {} secret(s) (vault={}, aliases={}, env_fallbacks_bypassed={})",
                    report.loaded,
                    report.from_vault,
                    report.from_alias,
                    report.env_fallbacks_bypassed
                );
            }
            report.loaded
        })
    }

    pub(crate) fn ensure_provider_secrets_materialized(&self, keys: &[&str]) {
        if keys
            .iter()
            .all(|key| self.llm.has_configured_secret(&[*key]))
        {
            return;
        }
        let _ = self.refresh_llm_provider_secrets_from_vault();
    }

    pub(crate) fn unlocked_env_secrets_for_child_env(
        &self,
        cwd: Option<&std::path::Path>,
    ) -> Result<Vec<(String, String)>, String> {
        load_unlocked_env_secrets_for_child_env(self, cwd)
    }

    /// Clone the foundry maintenance sender so external supervisors
    /// (e.g. the multi-DB FoundryScheduler) can re-inject jobs into the
    /// same in-process worker that handles enrichment-driven enqueues.
    pub(crate) fn enrichment_lock(&self) -> &EnrichmentRuntime {
        &self.enrichment
    }

    pub(crate) fn foundry_lock(&self) -> &FoundryRuntime {
        &self.foundry
    }

    pub(crate) fn foundry_tx_clone(&self) -> mpsc::Sender<FoundryMaintenanceItem> {
        self.foundry_lock().foundry_tx.clone()
    }

    pub(crate) fn vault_read(&self) -> std::sync::RwLockReadGuard<'_, VaultState> {
        self.vault.read().unwrap_or_else(|e| e.into_inner())
    }

    pub(crate) fn vault_write(&self) -> std::sync::RwLockWriteGuard<'_, VaultState> {
        self.vault.write().unwrap_or_else(|e| e.into_inner())
    }

    pub(crate) fn rate_limiter_lock(&self) -> std::sync::MutexGuard<'_, RateLimiter> {
        self.rate_limiter.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Path to this server's global memory DB (canonicalized at boot).
    pub(crate) fn global_db_path_buf(&self) -> PathBuf {
        self.db.global_db_path_buf()
    }

    /// Path to this server's project memory DB, when one is bound.
    pub(crate) fn project_db_path_buf(&self) -> Option<PathBuf> {
        self.db.project_db_path_buf()
    }

    pub(crate) fn global_vec_available(&self) -> bool {
        self.db.global_vec_available
    }

    pub(crate) fn project_vec_available(&self) -> bool {
        self.db.project_vec_available()
    }

    pub(crate) fn agent_runtime_read(&self) -> std::sync::RwLockReadGuard<'_, AgentRuntime> {
        read_or_recover(&self.agent_runtime, "agent_runtime")
    }

    pub(crate) fn agent_runtime_write(&self) -> std::sync::RwLockWriteGuard<'_, AgentRuntime> {
        write_or_recover(&self.agent_runtime, "agent_runtime")
    }

    pub(crate) fn event_db_route(&self, project: Option<&str>) -> EventDbRoute {
        event_db_route(project, self.has_project_db())
    }

    pub(crate) fn with_event_route_store<T>(
        &self,
        route: &EventDbRoute,
        f: impl FnOnce(&mut MemoryStore) -> Result<T, String>,
    ) -> Result<T, String> {
        match route {
            EventDbRoute::NamedProject(name) => self.with_named_project_store(name, f),
            EventDbRoute::Project => self.with_project_store(f),
            EventDbRoute::Global => self.with_global_store(f),
        }
    }

    pub(crate) fn with_event_route_store_read<T>(
        &self,
        route: &EventDbRoute,
        f: impl FnOnce(&mut MemoryStore) -> Result<T, String>,
    ) -> Result<T, String> {
        match route {
            EventDbRoute::NamedProject(name) => self.with_named_project_store_read(name, f),
            EventDbRoute::Project => self.with_project_store_read(f),
            EventDbRoute::Global => self.with_global_store_read(f),
        }
    }

    pub(crate) fn bound_agent_id(&self) -> Option<String> {
        read_or_recover(&self.bound_agent_id, "bound_agent_id").clone()
    }

    #[cfg(test)]
    pub(crate) fn set_bound_agent_id_for_test(&self, agent_id: Option<&str>) {
        *write_or_recover(&self.bound_agent_id, "bound_agent_id") = agent_id
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_string);
    }

    /// This server's Tachi home directory, resolved once at construction
    /// (see `home_dir` field doc). Handlers reachable after `MemoryServer::new`
    /// should call this instead of re-reading `path_utils::tachi_home()` /
    /// `TACHI_HOME`-family env vars directly.
    pub(crate) fn tachi_home_dir(&self) -> PathBuf {
        (*self.home_dir).clone()
    }

    pub(crate) fn routing_config(
        &self,
    ) -> &crate::memory_search_ops::routing_config::RoutingConfigProvider {
        &self.routing_config
    }
}
