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

/// Operator-facing warning when env/config.env is ignored because vault won.
/// Names only — never interpolates secret values.
pub(crate) fn format_bypassed_env_warning(name: &str) -> String {
    format!(
        "[provider] env/config.env value ignored for {name} — vault wins; if your env value is fresher: tachi vault set {name}"
    )
}

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

    pub(crate) fn refresh_llm_provider_secrets_from_vault(
        &self,
    ) -> Result<crate::provider_config::MaterializeReport, String> {
        let report = crate::provider_config::materialize_for_server(self)?;
        if report.from_alias > 0 || report.env_fallbacks_bypassed > 0 {
            tracing::info!(
                "[provider] materialized {} secret(s) (vault={}, aliases={}, env_fallbacks_bypassed={})",
                report.loaded,
                report.from_vault,
                report.from_alias,
                report.env_fallbacks_bypassed
            );
        }
        if report.env_fallbacks_bypassed > 0 {
            for name in &report.bypassed_names {
                tracing::warn!("{}", format_bypassed_env_warning(name));
            }
        }
        // #1279: this is the single refresh seam (daemon auto-refresh, keychain
        // auto-unlock, ensure_materialized, vault_set/unlock). A per-alias skip
        // must never be swallowed by the discarded report — log each skipped alias
        // loudly with its vault_unlock/vault_set remediation before returning.
        for (key, reason) in &report.skipped_aliases {
            tracing::warn!(
                "[provider] skipped alias for '{key}': {}",
                crate::provider_config::format_skipped_alias_reason(reason)
            );
        }
        Ok(report)
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

#[cfg(test)]
mod tests {
    use super::format_bypassed_env_warning;

    /// #1403 R2 / #1393: bypassed-env warn text must name the key and must not
    /// leak env or vault secret values into the operator-facing string.
    #[test]
    fn bypassed_env_warning_includes_name_not_secret_values() {
        let env_sentinel = "ENV_SENTINEL_VALUE_DO_NOT_LEAK";
        let vault_sentinel = "VAULT_SENTINEL_VALUE_DO_NOT_LEAK";
        let name = "OPENAI_API_KEY";

        // Fixtures: distinct env vs vault payloads that a buggy formatter might
        // interpolate. Materialize would record `name` in bypassed_names when
        // vault wins; we format that name only.
        let bypassed_names = [name.to_string()];
        let warning = format_bypassed_env_warning(&bypassed_names[0]);

        assert!(
            warning.contains(name),
            "warning must name the bypassed key: {warning}"
        );
        assert_eq!(
            warning,
            "[provider] env/config.env value ignored for OPENAI_API_KEY — vault wins; if your env value is fresher: tachi vault set OPENAI_API_KEY"
        );
        assert!(
            !warning.contains(env_sentinel),
            "warning must not leak env sentinel: {warning}"
        );
        assert!(
            !warning.contains(vault_sentinel),
            "warning must not leak vault sentinel: {warning}"
        );
    }

    /// A capturing [`tracing_subscriber::fmt::MakeWriter`] so the production
    /// `tracing::warn!` in `refresh_llm_provider_secrets_from_vault` can be
    /// asserted on directly. Same idiom as
    /// `bootstrap::clean_cli::tests::sweep_leaves_lease_whose_stat_is_inconclusive_and_logs_why`.
    #[derive(Clone, Default)]
    struct BufWriter(std::sync::Arc<std::sync::Mutex<Vec<u8>>>);

    impl std::io::Write for BufWriter {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(buf);
            Ok(buf.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for BufWriter {
        type Writer = BufWriter;
        fn make_writer(&'a self) -> Self::Writer {
            self.clone()
        }
    }

    /// #1403 R2 / #1393 cold review (codex-2d6e7): the formatter-only test
    /// above calls `format_bypassed_env_warning` directly, so deleting the
    /// production warn loop below —
    /// ```ignore
    /// if report.env_fallbacks_bypassed > 0 {
    ///     for name in &report.bypassed_names {
    ///         tracing::warn!("{}", format_bypassed_env_warning(name));
    ///     }
    /// }
    /// ```
    /// (the body of `refresh_llm_provider_secrets_from_vault`, this file,
    /// currently a few lines above the `tests` module) — would leave the
    /// formatter test green, because it never calls
    /// `refresh_llm_provider_secrets_from_vault` at all.
    ///
    /// This test drives the real emission path instead: `vault_set` on a
    /// live `MemoryServer` with a plaintext env fallback already present
    /// under the same key. Inside `handle_vault_set`,
    /// `attach_provider_refresh_warning` calls the production
    /// `refresh_llm_provider_secrets_from_vault()`, which finds
    /// `env_fallbacks_bypassed > 0` (vault wins over the env plaintext) and
    /// — if the loop above still exists — emits `tracing::warn!` naming the
    /// bypassed key. We capture the real tracing output with a `MakeWriter`
    /// held across the async work via `tracing::subscriber::set_default`
    /// inside `block_on` (same thread, so the guard's thread-local stays
    /// valid for the whole emission path).
    ///
    /// Plain `#[test]` + `new_current_thread().block_on` (not `#[tokio::test]`),
    /// matching the `global_test_lock` convention in `vault_ops/tests.rs`: the
    /// guard serializes process-wide `SILICONFLOW_API_KEY` env against other
    /// tests, so it must stay held for the whole init/set sequence including
    /// its internal awaits — a current_thread runtime keeps TLS and
    /// `tracing::subscriber::set_default` on one thread for the whole
    /// emission path, and `block_on` runs that future to completion without
    /// a top-level `.await` for clippy's `await_holding_lock` lint while the
    /// guard's coverage is unchanged.
    ///
    /// DISCRIMINATION (RED/GREEN proof executed by the build seat, not run
    /// here — this lane is edit-only, no cargo): comment out or delete the
    /// `if report.env_fallbacks_bypassed > 0 { ... }` block in
    /// `refresh_llm_provider_secrets_from_vault` above and this test must go
    /// RED (the captured buffer no longer contains the bypassed key name);
    /// restoring the block must bring it back GREEN.
    #[test]
    fn refresh_from_vault_emits_bypassed_env_warning_via_real_emission_path() {
        let _lock = crate::utils::global_test_lock()
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());

        // `crate::tests::make_server()` runs `ensure_test_env()` (a
        // process-wide `Once`) which sets a baseline `SILICONFLOW_API_KEY`.
        // Construct the server FIRST so that `Once` fires before we install
        // our own sentinel value below — otherwise, if this were the first
        // test in the binary to touch `ensure_test_env()`, the baseline
        // value could be set AFTER ours and clobber it.
        let server = crate::tests::make_server();

        let key_name = "SILICONFLOW_API_KEY";
        let env_sentinel = "ENV_SENTINEL_VALUE_DO_NOT_LEAK";
        let vault_sentinel = "VAULT_SENTINEL_VALUE_DO_NOT_LEAK";
        let _env = crate::test_support::EnvRestore::set(key_name, env_sentinel);

        let buf = BufWriter::default();
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("tokio current_thread runtime")
            .block_on(async {
                server
                    .vault_init(rmcp::handler::server::wrapper::Parameters(
                        crate::vault_ops::VaultInitParams {
                            password: "emission-path-test-password".to_string(),
                        },
                    ))
                    .await
                    .expect("vault_init should succeed");

                let subscriber = tracing_subscriber::fmt()
                    .with_writer(buf.clone())
                    .with_ansi(false)
                    .finish();
                let _tracing_guard = tracing::subscriber::set_default(subscriber);

                // vault_set stores `vault_sentinel` under the same name the env
                // fallback already occupies, then internally calls
                // `refresh_llm_provider_secrets_from_vault()` — the real production
                // trigger for the warn loop under test.
                server
                    .vault_set(rmcp::handler::server::wrapper::Parameters(
                        crate::vault_ops::VaultSetParams {
                            name: key_name.to_string(),
                            value: vault_sentinel.to_string(),
                            agent_id: None,
                            secret_type: "api_key".to_string(),
                            description: "emission-path discrimination test".to_string(),
                            allowed_agents: None,
                            enable_rotation: false,
                            rotation_strategy: None,
                        },
                    ))
                    .await
                    .expect("vault_set should succeed");
            });

        let logged = String::from_utf8(buf.0.lock().unwrap().clone()).unwrap();
        assert!(
            logged.contains(key_name),
            "the real refresh_llm_provider_secrets_from_vault() emission must name the \
             bypassed key when the warn loop is present: {logged}"
        );
        assert!(
            logged.contains("env/config.env value ignored for"),
            "must be the bypassed-env warning, not some other log line: {logged}"
        );
        assert!(
            !logged.contains(env_sentinel),
            "must not leak the env plaintext value: {logged}"
        );
        assert!(
            !logged.contains(vault_sentinel),
            "must not leak the vault plaintext value: {logged}"
        );
    }
}
