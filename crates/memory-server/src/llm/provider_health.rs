// provider_health.rs — provider secret pools, key health, and lane configuration

use chrono::{DateTime, Utc};
use serde::Serialize;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};
use std::time::{Duration, Instant};

use memory_core::vault::VaultKeyHealth;

mod state;
mod types;

pub(super) use self::state::{
    ProviderHealthPersistState, ProviderHealthReloadState, ProviderHealthSnapshot, ProviderState,
};
pub(crate) use self::types::ProviderSecret;
pub(super) use self::types::{
    ChatLane, ChatLaneConfig, ClaudeCliFailure, ClaudeCliFailureKind, ClaudeCliSkip,
    KeyAvailability, KeyRetryStatus, SelectedProviderSecret,
};
pub(crate) use self::types::{ProviderHealthStatus, ProviderKeyCooldownStatus, ProviderPoolStatus};

const DEFAULT_CHAT_BASE_URL: &str = "https://api.siliconflow.cn/v1/chat/completions";
const DEFAULT_EXTRACT_MODEL: &str = "Qwen/Qwen3.5-27B";
const DEFAULT_REASONING_MODEL: &str = "Qwen/Qwen3.5-27B";
pub(super) const HEALTH_OK: &str = "ok";
const HEALTH_COOLDOWN: &str = "cooldown";
pub(super) const HEALTH_RATE_LIMITED: &str = "rate_limited";
const HEALTH_AUTH_FAILED: &str = "auth_failed";
const HEALTH_DISABLED: &str = "disabled";
const HEALTH_EXHAUSTED: &str = "exhausted";
pub(super) const CLAUDE_CLI_FAILURE_COOLDOWN: Duration = Duration::from_secs(600);

#[cfg(test)]
fn provider_key_health_persist_disabled_for_tests() -> bool {
    matches!(
        std::env::var("TACHI_TEST_DISABLE_PROVIDER_KEY_HEALTH_PERSIST")
            .ok()
            .as_deref(),
        Some("1") | Some("true") | Some("TRUE") | Some("yes")
    )
}

#[cfg(not(test))]
fn provider_key_health_persist_disabled_for_tests() -> bool {
    false
}

impl super::LlmClient {
    pub(super) const MAX_ATTEMPTS: usize = 3;
    pub(super) const BASE_RETRY_DELAY_MS: u64 = 500;
    pub(super) const RETRY_JITTER_PERCENT: u64 = 20;
    pub(super) const KEY_HEALTH_RELOAD_TTL: Duration = Duration::from_secs(30);

    pub fn new() -> Result<Self, String> {
        Self::new_with_vault_db(None)
    }

    pub fn new_with_vault_db(vault_db_path: Option<&Path>) -> Result<Self, String> {
        let vault_db_path = vault_db_path.map(|path| path.to_path_buf());

        // ── Front-line LLM layer (Extract + Summary) ──
        // Extract: EXTRACT_* → SILICONFLOW_*
        let extract = Self::load_lane(
            "extract",
            &["EXTRACT_API_KEY", "SILICONFLOW_API_KEY"],
            &[
                "EXTRACT_BASE_URL",
                "SILICONFLOW_BASE_URL",
                "EXTRACTOR_BASE_URL",
            ],
            &["EXTRACT_MODEL", "SILICONFLOW_MODEL", "EXTRACTOR_MODEL"],
            "TACHI_BACKEND_EXTRACT_TIER",
            DEFAULT_EXTRACT_MODEL,
        )?;

        // Summary: SUMMARY_* → EXTRACT_* → SILICONFLOW_*  (front-line default)
        let summary = Self::load_lane(
            "summary",
            &["SUMMARY_API_KEY", "EXTRACT_API_KEY", "SILICONFLOW_API_KEY"],
            &[
                "SUMMARY_BASE_URL",
                "EXTRACT_BASE_URL",
                "SILICONFLOW_BASE_URL",
                "EXTRACTOR_BASE_URL",
            ],
            &[
                "SUMMARY_MODEL",
                "EXTRACT_MODEL",
                "SILICONFLOW_MODEL",
                "EXTRACTOR_MODEL",
            ],
            "TACHI_BACKEND_SUMMARY_TIER",
            &extract.model,
        )?;

        // ── Foundry LLM layer (Distill + Reasoning) ──
        // Both lanes now fall back to the front-line Extract/SiliconFlow chain.
        // Dedicated DISTILL_*/REASONING_* env vars still override if set.
        let reasoning = Self::load_lane(
            "reasoning",
            &[
                "REASONING_API_KEY",
                "DEEPSEEK_API_KEY",
                "ZAI_API_KEY",
                "BIGMODEL_API_KEY",
                "DISTILL_API_KEY",
                "EXTRACT_API_KEY",
                "SILICONFLOW_API_KEY",
            ],
            &[
                "REASONING_BASE_URL",
                "DISTILL_BASE_URL",
                "EXTRACT_BASE_URL",
                "SILICONFLOW_BASE_URL",
            ],
            &[
                "REASONING_MODEL",
                "DISTILL_MODEL",
                "EXTRACT_MODEL",
                "SILICONFLOW_MODEL",
            ],
            "TACHI_BACKEND_REASONING_TIER",
            DEFAULT_REASONING_MODEL,
        )?;

        let distill = Self::load_lane(
            "distill",
            &[
                "DISTILL_API_KEY",
                "REASONING_API_KEY",
                "DEEPSEEK_API_KEY",
                "ZAI_API_KEY",
                "BIGMODEL_API_KEY",
                "EXTRACT_API_KEY",
                "SILICONFLOW_API_KEY",
            ],
            &[
                "DISTILL_BASE_URL",
                "REASONING_BASE_URL",
                "EXTRACT_BASE_URL",
                "SILICONFLOW_BASE_URL",
            ],
            &[
                "DISTILL_MODEL",
                "REASONING_MODEL",
                "EXTRACT_MODEL",
                "SILICONFLOW_MODEL",
            ],
            "TACHI_BACKEND_DISTILL_TIER",
            &reasoning.model,
        )?;

        let http = reqwest::Client::builder()
            .timeout(Duration::from_secs(60))
            .build()
            .map_err(|e| format!("Failed to build HTTP client: {e}"))?;

        let (provider_health, provider_health_reload) =
            Self::initial_key_health_from_db(vault_db_path.as_deref());

        // Warn when foundry lanes collapse to the same model/endpoint as extract.
        // This is expected when dedicated DISTILL_*/REASONING_* env vars are unset,
        // but the user should know so they can configure separation if needed.
        if distill.base_url == extract.base_url && distill.model == extract.model {
            tracing::info!(
                "LLM distill lane collapsed to extract endpoint ({}/{}). \
                 Set DISTILL_API_KEY / DISTILL_BASE_URL to separate.",
                extract.base_url,
                extract.model,
            );
        }

        Ok(Self {
            http,
            extract,
            distill,
            reasoning,
            summary,
            vault_db_path,
            provider_state: Arc::new(RwLock::new(ProviderState::with_health(provider_health))),
            provider_health_reload: Arc::new(RwLock::new(provider_health_reload)),
            provider_health_persist: Arc::new(RwLock::new(ProviderHealthPersistState::default())),
            claude_cli_failure: Arc::new(RwLock::new(None)),
        })
    }

    fn initial_key_health_from_db(
        vault_db_path: Option<&Path>,
    ) -> (
        HashMap<String, HashMap<String, VaultKeyHealth>>,
        ProviderHealthReloadState,
    ) {
        let Some(path) = vault_db_path else {
            return (HashMap::new(), ProviderHealthReloadState::memory_only());
        };
        let now = Instant::now();
        let now_utc = Self::format_now_utc();
        let mut reload = ProviderHealthReloadState::vault_db_attempt(now, now_utc.clone());
        match Self::load_key_health_from_db(path) {
            Ok(health) => {
                reload.mark_success(now, now_utc);
                (health, reload)
            }
            Err(err) => {
                reload.mark_error(now, now_utc, err);
                (HashMap::new(), reload)
            }
        }
    }

    fn load_key_health_from_db(
        path: &Path,
    ) -> Result<HashMap<String, HashMap<String, VaultKeyHealth>>, String> {
        if !path.exists() {
            return Ok(HashMap::new());
        }
        let Some(db_path) = path.to_str() else {
            return Err("Invalid vault db path".to_string());
        };
        let store = memory_core::MemoryStore::open_read_only(db_path)
            .map_err(|e| format!("Open vault db failed: {e}"))?;
        let rows = store
            .vault_list_key_health(None)
            .map_err(|e| format!("Load vault key health failed: {e}"))?;

        Ok(rows.into_iter().fold(HashMap::new(), |mut map, row| {
            map.entry(row.logical_name.clone())
                .or_insert_with(HashMap::new)
                .insert(row.key_id.clone(), row);
            map
        }))
    }

    fn key_health_snapshot_is_newer_or_equal(
        incoming: &ProviderHealthSnapshot,
        existing: &ProviderHealthSnapshot,
    ) -> bool {
        match (incoming.updated_at, existing.updated_at) {
            (Some(incoming_ts), Some(existing_ts)) => incoming_ts >= existing_ts,
            (Some(_), None) => true,
            (None, Some(_)) => false,
            (None, None) => true,
        }
    }

    fn merge_loaded_key_health(&self, loaded: HashMap<String, HashMap<String, VaultKeyHealth>>) {
        let now_utc = Self::now_utc();
        let loaded = loaded
            .into_iter()
            .flat_map(|(logical_name, members)| {
                members.into_iter().map(move |(key_id, health)| {
                    let snapshot = ProviderHealthSnapshot::from_health(&health);
                    (logical_name.clone(), key_id, health, snapshot)
                })
            })
            .collect::<Vec<_>>();
        let mut state = self
            .provider_state
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        for (logical_name, key_id, incoming, snapshot) in loaded {
            let should_apply = state
                .health_snapshots
                .get(&logical_name)
                .and_then(|target| target.get(&key_id))
                .map(|existing| Self::key_health_snapshot_is_newer_or_equal(&snapshot, existing))
                .unwrap_or(true);
            if should_apply {
                let (availability, remaining_seconds) = snapshot.availability_at(now_utc);
                let still_cooling =
                    availability == KeyAvailability::Cooldown && remaining_seconds.unwrap_or(0) > 0;
                if !still_cooling {
                    state.cooldowns.remove(&key_id);
                }
                state.set_health_entry_with_snapshot(logical_name, key_id, incoming, snapshot);
            }
        }
    }

    fn key_health_reload_due(&self, now: Instant) -> bool {
        if self.vault_db_path.is_none() {
            return false;
        }
        let reload = self
            .provider_health_reload
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        reload
            .last_attempt
            .map(|last| now.duration_since(last) >= Self::KEY_HEALTH_RELOAD_TTL)
            .unwrap_or(true)
    }

    async fn refresh_key_health_from_db_if_stale(&self) {
        let Some(db_path) = self.vault_db_path.clone() else {
            return;
        };
        let now = Instant::now();
        if !self.key_health_reload_due(now) {
            return;
        }
        {
            let mut reload = self
                .provider_health_reload
                .write()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            if reload
                .last_attempt
                .map(|last| now.duration_since(last) < Self::KEY_HEALTH_RELOAD_TTL)
                .unwrap_or(false)
            {
                return;
            }
            reload.last_attempt = Some(now);
            reload.last_attempt_at = Some(Self::format_now_utc());
        }

        let loaded = tokio::task::spawn_blocking(move || Self::load_key_health_from_db(&db_path))
            .await
            .map_err(|err| format!("key health reload task failed: {err}"))
            .and_then(|inner| inner);
        let completed_at = Instant::now();
        let completed_at_utc = Self::format_now_utc();
        match loaded {
            Ok(health) => {
                self.merge_loaded_key_health(health);
                self.provider_health_reload
                    .write()
                    .unwrap_or_else(|poisoned| poisoned.into_inner())
                    .mark_success(completed_at, completed_at_utc);
            }
            Err(err) => {
                tracing::warn!("[provider] failed to reload vault key health: {err}");
                self.provider_health_reload
                    .write()
                    .unwrap_or_else(|poisoned| poisoned.into_inner())
                    .mark_error(completed_at, completed_at_utc, err);
            }
        }
    }

    fn now_utc() -> DateTime<Utc> {
        Utc::now()
    }

    fn format_now_utc() -> String {
        Self::now_utc().to_rfc3339()
    }

    fn parse_timestamp(ts: &str) -> Option<DateTime<Utc>> {
        DateTime::parse_from_rfc3339(ts)
            .ok()
            .map(|parsed| parsed.with_timezone(&Utc))
    }

    fn status_from_key_health_status(raw: &str) -> &'static str {
        match raw {
            HEALTH_COOLDOWN | HEALTH_RATE_LIMITED => HEALTH_RATE_LIMITED,
            HEALTH_AUTH_FAILED => HEALTH_AUTH_FAILED,
            HEALTH_DISABLED => HEALTH_DISABLED,
            HEALTH_EXHAUSTED => HEALTH_EXHAUSTED,
            _ => HEALTH_OK,
        }
    }

    fn key_health_blocked_in_state(
        state: &ProviderState,
        logical_name: &str,
        key_id: &str,
        now: DateTime<Utc>,
    ) -> (KeyAvailability, Option<i64>) {
        state
            .health_snapshots
            .get(logical_name)
            .and_then(|members| members.get(key_id))
            .map(|snapshot| snapshot.availability_at(now))
            .unwrap_or((KeyAvailability::Available, None))
    }

    fn persist_key_health(&self, health: &VaultKeyHealth) {
        if provider_key_health_persist_disabled_for_tests() {
            return;
        }
        let Some(db_path) = self.vault_db_path.clone() else {
            return;
        };
        let mut health = health.clone();
        health.updated_at = Self::format_now_utc();
        if let Ok(handle) = tokio::runtime::Handle::try_current() {
            let logical_name = health.logical_name.clone();
            let key_id = health.key_id.clone();
            let persist_state = Arc::clone(&self.provider_health_persist);
            handle.spawn(async move {
                let result = tokio::task::spawn_blocking(move || {
                    Self::persist_key_health_blocking(db_path, health)
                })
                .await
                .map_err(|err| {
                    format!("persist vault key health for {logical_name}:{key_id}: {err}")
                })
                .and_then(|inner| inner);
                Self::record_key_health_persist_result(&persist_state, result);
            });
        } else {
            let result = Self::persist_key_health_blocking(db_path, health);
            Self::record_key_health_persist_result(&self.provider_health_persist, result);
        }
    }

    fn persist_key_health_now(&self, health: &VaultKeyHealth) {
        if provider_key_health_persist_disabled_for_tests() {
            return;
        }
        let Some(db_path) = self.vault_db_path.clone() else {
            return;
        };
        let mut health = health.clone();
        health.updated_at = Self::format_now_utc();
        let result = Self::persist_key_health_blocking(db_path, health);
        Self::record_key_health_persist_result(&self.provider_health_persist, result);
    }

    fn persist_key_health_blocking(db_path: PathBuf, health: VaultKeyHealth) -> Result<(), String> {
        let target = format!("{}:{}", health.logical_name, health.key_id);
        let Some(db_path) = db_path.to_str() else {
            return Err(format!(
                "persist vault key health for {target}: invalid db path"
            ));
        };
        match memory_core::MemoryStore::open(db_path) {
            Ok(store) => {
                store
                    .vault_upsert_key_health(&health)
                    .map_err(|err| format!("persist vault key health for {target}: {err}"))?;
                Ok(())
            }
            Err(err) => Err(format!("persist vault key health for {target}: {err}")),
        }
    }

    fn record_key_health_persist_result(
        persist_state: &Arc<RwLock<ProviderHealthPersistState>>,
        result: Result<(), String>,
    ) {
        let now = Instant::now();
        let now_utc = Self::format_now_utc();
        let mut state = persist_state
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        match result {
            Ok(()) => state.mark_success(now, now_utc),
            Err(err) => {
                tracing::warn!("[provider] {err}");
                state.mark_error(now_utc, err);
            }
        }
    }

    fn read_key_health_entry(&self, logical_name: &str, key_id: &str) -> Option<VaultKeyHealth> {
        self.provider_state
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .health
            .get(logical_name)
            .and_then(|members| members.get(key_id))
            .cloned()
    }

    fn with_key_health(
        &self,
        logical_name: &str,
        key_id: &str,
        mutator: impl FnOnce(&mut VaultKeyHealth),
    ) {
        let now = Self::now_utc();
        let now_utc = now.to_rfc3339();
        let mut state = self
            .provider_state
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let persisted = {
            let health = state.get_or_insert_health(logical_name, key_id);
            mutator(health);
            health.last_attempt = Some(now_utc.clone());
            health.updated_at = now_utc;
            health.clone()
        };
        state.set_health_snapshot(
            logical_name,
            key_id,
            ProviderHealthSnapshot::from_health_parts(&persisted, Some(now), None),
        );
        drop(state);
        self.persist_key_health(&persisted);
    }

    pub(super) fn mark_secret_auth_failed(
        &self,
        selected: &SelectedProviderSecret,
        reason: Option<&str>,
    ) {
        self.with_key_health(&selected.logical_name, &selected.key_id, |health| {
            health.status = HEALTH_AUTH_FAILED.to_string();
            health.auth_failed = true;
            health.disabled = false;
            health.cooldown_until = None;
            health.last_error = reason.map(|value| value.to_string());
            health.error_count += 1;
        });
    }

    pub(super) fn mark_secret_success(&self, selected: &SelectedProviderSecret) {
        let now = Self::now_utc();
        let now_utc = now.to_rfc3339();
        let mut state = self
            .provider_state
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let persisted = {
            let health = state.get_or_insert_health(&selected.logical_name, &selected.key_id);
            health.status = HEALTH_OK.to_string();
            health.auth_failed = false;
            health.last_success = Some(now_utc.clone());
            health.last_attempt = Some(now_utc.clone());
            health.last_error = None;
            health.error_count = 0;
            health.cooldown_until = None;
            health.updated_at = now_utc;
            health.clone()
        };
        state.cooldowns.remove(&selected.key_id);
        state.set_health_snapshot(
            &selected.logical_name,
            &selected.key_id,
            ProviderHealthSnapshot::from_health_parts(&persisted, Some(now), None),
        );
        drop(state);
        self.persist_key_health(&persisted);
    }

    fn load_lane(
        lane: &str,
        api_key_envs: &[&'static str],
        base_url_envs: &[&str],
        model_envs: &[&str],
        tier_env: &str,
        default_model: &str,
    ) -> Result<ChatLaneConfig, String> {
        let base_url =
            Self::first_env(base_url_envs).unwrap_or_else(|| DEFAULT_CHAT_BASE_URL.to_string());
        let explicit = model_envs
            .first()
            .and_then(|&key| std::env::var(key).ok())
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty());
        let fallback = if model_envs.len() > 1 {
            Self::first_env(&model_envs[1..])
        } else {
            None
        };
        let model = crate::backend_tier::resolve_lane_model(
            lane,
            tier_env,
            explicit,
            fallback,
            default_model,
        );

        Ok(ChatLaneConfig {
            base_url,
            model,
            api_key_envs: api_key_envs.to_vec(),
        })
    }

    fn first_env(keys: &[&str]) -> Option<String> {
        keys.iter().find_map(|key| {
            std::env::var(key)
                .ok()
                .map(|value| value.trim().to_string())
                .filter(|value| !value.is_empty())
        })
    }

    pub(super) fn lane(&self, lane: ChatLane) -> &ChatLaneConfig {
        match lane {
            ChatLane::Extract => &self.extract,
            ChatLane::Distill => &self.distill,
            ChatLane::Reasoning => &self.reasoning,
            ChatLane::Summary => &self.summary,
        }
    }

    #[cfg(test)]
    pub fn set_provider_secret(&self, name: &str, value: &str) -> bool {
        let name = name.trim();
        let value = value.trim();
        if name.is_empty() || value.is_empty() {
            return false;
        }

        self.set_provider_secret_pool(
            name,
            vec![ProviderSecret {
                key_id: name.to_string(),
                value: value.to_string(),
            }],
        )
    }

    pub(crate) fn set_provider_secret_pool(
        &self,
        name: &str,
        entries: Vec<ProviderSecret>,
    ) -> bool {
        let name = name.trim();
        if name.is_empty() {
            return false;
        }
        let entries: Vec<ProviderSecret> = entries
            .into_iter()
            .filter(|entry| !entry.key_id.trim().is_empty() && !entry.value.trim().is_empty())
            .collect();
        if entries.is_empty() {
            return false;
        }

        let mut state = self
            .provider_state
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        state.secrets.insert(name.to_string(), entries);
        state.indices.remove(name);
        true
    }

    pub(crate) fn set_provider_secret_pools<I>(&self, pools: I) -> usize
    where
        I: IntoIterator<Item = (String, Vec<ProviderSecret>)>,
    {
        pools
            .into_iter()
            .filter(|(name, entries)| self.set_provider_secret_pool(name, entries.clone()))
            .count()
    }

    pub fn clear_provider_secrets(&self) {
        let mut state = self
            .provider_state
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        state.secrets.clear();
        state.cooldowns.clear();
        state.indices.clear();
    }

    pub fn provider_secret_count(&self) -> usize {
        self.provider_state
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .secrets
            .len()
    }

    pub(crate) fn provider_pool_statuses(&self) -> Vec<ProviderPoolStatus> {
        let now = Instant::now();
        let now_utc = Self::now_utc();
        let state = self
            .provider_state
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner());

        let mut statuses = state
            .secrets
            .iter()
            .map(|(logical_name, entries)| {
                let mut unavailable_keys = Vec::new();
                for entry in entries.iter() {
                    let (availability, remaining_seconds) = Self::key_health_blocked_in_state(
                        &state,
                        logical_name,
                        &entry.key_id,
                        now_utc,
                    );
                    let memory_blocked = state
                        .cooldowns
                        .get(&entry.key_id)
                        .is_some_and(|until| *until > now);

                    let is_blocked = memory_blocked
                        || matches!(
                            availability,
                            KeyAvailability::AuthFailed
                                | KeyAvailability::Disabled
                                | KeyAvailability::Exhausted
                        )
                        || matches!(availability, KeyAvailability::Cooldown)
                            && remaining_seconds.unwrap_or(0) > 0;

                    if is_blocked {
                        let remaining_seconds = if memory_blocked {
                            state
                                .cooldowns
                                .get(&entry.key_id)
                                .map(|until| {
                                    until.saturating_duration_since(now).as_secs().max(1) as i64
                                })
                                .unwrap_or(0)
                        } else {
                            remaining_seconds.unwrap_or(0)
                        };
                        let remaining_seconds = if remaining_seconds < 0 {
                            0
                        } else {
                            remaining_seconds as u64
                        };

                        unavailable_keys.push(ProviderKeyCooldownStatus {
                            key_id: entry.key_id.clone(),
                            remaining_seconds,
                        });
                    }
                }

                unavailable_keys.sort_by(|a, b| a.key_id.cmp(&b.key_id));
                ProviderPoolStatus {
                    logical_name: logical_name.to_string(),
                    total_keys: entries.len(),
                    available_keys: entries.len().saturating_sub(unavailable_keys.len()),
                    rate_limited_keys: unavailable_keys,
                    current_index: *state.indices.get(logical_name).unwrap_or(&0),
                    strategy: "round_robin_skip_cooldown",
                }
            })
            .collect::<Vec<_>>();
        statuses.sort_by(|a, b| a.logical_name.cmp(&b.logical_name));
        statuses
    }

    pub(crate) fn provider_health_status(&self) -> ProviderHealthStatus {
        let reload = self
            .provider_health_reload
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let persist = self
            .provider_health_persist
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        ProviderHealthStatus {
            source_of_truth: reload.source_of_truth,
            reload_ttl_secs: Self::KEY_HEALTH_RELOAD_TTL.as_secs(),
            last_attempt_at: reload.last_attempt_at.clone(),
            last_success_at: reload.last_success_at.clone(),
            last_success_age_secs: reload
                .last_success
                .map(|instant| instant.elapsed().as_secs()),
            last_error: reload.last_error.clone(),
            persist_last_attempt_at: persist.last_attempt_at.clone(),
            persist_last_success_at: persist.last_success_at.clone(),
            persist_last_success_age_secs: persist
                .last_success
                .map(|instant| instant.elapsed().as_secs()),
            persist_last_error: persist.last_error.clone(),
        }
    }

    /// Return the current in-memory provider key health map.
    ///
    /// This is used when loading API key pools so that tests (which may disable
    /// background persistence) still see health mutations made in the same
    /// process without requiring a DB round-trip.
    pub(crate) fn provider_health_memory_snapshot(
        &self,
    ) -> HashMap<String, HashMap<String, VaultKeyHealth>> {
        let state = self
            .provider_state
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        state.health.clone()
    }

    fn select_secret(&self, keys: &[&str]) -> Option<SelectedProviderSecret> {
        let now = Instant::now();
        let now_utc = Self::now_utc();
        let mut state = self
            .provider_state
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        state.prune_expired_cooldowns(now);

        let vault_value = keys.iter().find_map(|key| {
            let entries = state.secrets.get(*key)?;
            if entries.is_empty() {
                return None;
            }
            let start = *state.indices.get(*key).unwrap_or(&0);
            let mut selected: Option<(usize, usize, ProviderSecret)> = None;
            for offset in 0..entries.len() {
                let idx = (start + offset) % entries.len();
                let entry = &entries[idx];
                if state.cooldowns.contains_key(&entry.key_id) {
                    continue;
                }

                let (availability, remaining_seconds) =
                    Self::key_health_blocked_in_state(&state, key, &entry.key_id, now_utc);
                let unusable = match availability {
                    KeyAvailability::AuthFailed
                    | KeyAvailability::Disabled
                    | KeyAvailability::Exhausted => true,
                    KeyAvailability::Cooldown => remaining_seconds.unwrap_or(0) > 0,
                    KeyAvailability::Available => false,
                };
                if !unusable {
                    selected = Some((idx, entries.len(), entry.clone()));
                    break;
                }
            }

            selected
                .map(|(idx, entries_len, entry)| {
                    state
                        .indices
                        .insert((*key).to_string(), (idx + 1) % entries_len);
                    SelectedProviderSecret {
                        logical_name: (*key).to_string(),
                        key_id: entry.key_id,
                        value: entry.value.trim().to_string(),
                    }
                })
                .filter(|entry| !entry.value.is_empty())
        });

        vault_value.or_else(|| {
            keys.iter().find_map(|key| {
                let (availability, remaining_seconds) =
                    Self::key_health_blocked_in_state(&state, key, key, now_utc);
                let unusable = match availability {
                    KeyAvailability::AuthFailed
                    | KeyAvailability::Disabled
                    | KeyAvailability::Exhausted => true,
                    KeyAvailability::Cooldown => remaining_seconds.unwrap_or(0) > 0,
                    KeyAvailability::Available => false,
                };

                if unusable || state.cooldowns.contains_key(*key) {
                    return None;
                }
                Self::first_env(&[*key])
                    .filter(|value| !crate::provider_config::is_vault_alias(value))
                    .map(|value| SelectedProviderSecret {
                        logical_name: (*key).to_string(),
                        key_id: (*key).to_string(),
                        value,
                    })
            })
        })
    }

    #[cfg(test)]
    fn first_secret(&self, keys: &[&str]) -> Option<String> {
        self.select_secret(keys).map(|selected| selected.value)
    }

    #[cfg(test)]
    pub(super) fn required_secret(&self, keys: &[&str]) -> Result<String, String> {
        self.first_secret(keys)
            .ok_or_else(|| self.provider_secret_unavailable_error(keys))
    }

    fn required_selected_secret(&self, keys: &[&str]) -> Result<SelectedProviderSecret, String> {
        self.select_secret(keys)
            .ok_or_else(|| self.provider_secret_unavailable_error(keys))
    }

    pub(crate) fn has_configured_secret(&self, keys: &[&str]) -> bool {
        self.select_secret(keys).is_some()
    }

    fn provider_secret_unavailable_error(&self, keys: &[&str]) -> String {
        let now = Instant::now();
        let now_utc = Self::now_utc();
        let mut state = self
            .provider_state
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        state.prune_expired_cooldowns(now);

        let mut configured = 0usize;
        let mut empty = 0usize;
        let mut status_counts: HashMap<&'static str, usize> = HashMap::new();
        let mut retry_after = None;

        for key in keys {
            if let Some(entries) = state.secrets.get(*key) {
                for entry in entries {
                    if entry.value.trim().is_empty() {
                        empty += 1;
                        continue;
                    }
                    configured += 1;
                    if let Some(until) = state
                        .cooldowns
                        .get(&entry.key_id)
                        .filter(|until| **until > now)
                    {
                        Self::remember_min_retry_delay(
                            &mut retry_after,
                            until.saturating_duration_since(now),
                        );
                        *status_counts.entry("cooldown").or_default() += 1;
                        continue;
                    }
                    let (availability, remaining_seconds) =
                        Self::key_health_blocked_in_state(&state, key, &entry.key_id, now_utc);
                    if availability == KeyAvailability::Cooldown {
                        let delay =
                            Duration::from_secs(remaining_seconds.unwrap_or(1).max(1) as u64);
                        Self::remember_min_retry_delay(&mut retry_after, delay);
                    }
                    *status_counts.entry(availability.label()).or_default() += 1;
                }
            }

            if let Some(value) = Self::first_env(&[*key])
                .filter(|value| !value.trim().is_empty())
                .filter(|value| !crate::provider_config::is_vault_alias(value))
            {
                if value.trim().is_empty() {
                    empty += 1;
                    continue;
                }
                configured += 1;
                if let Some(until) = state.cooldowns.get(*key).filter(|until| **until > now) {
                    Self::remember_min_retry_delay(
                        &mut retry_after,
                        until.saturating_duration_since(now),
                    );
                    *status_counts.entry("cooldown").or_default() += 1;
                    continue;
                }
                let (availability, remaining_seconds) =
                    Self::key_health_blocked_in_state(&state, key, key, now_utc);
                if availability == KeyAvailability::Cooldown {
                    let delay = Duration::from_secs(remaining_seconds.unwrap_or(1).max(1) as u64);
                    Self::remember_min_retry_delay(&mut retry_after, delay);
                }
                *status_counts.entry(availability.label()).or_default() += 1;
            }
        }

        if configured == 0 {
            return "Missing API key. Add one to Tachi Vault or set the appropriate env var."
                .to_string();
        }

        let names = keys.join(", ");
        if let Some(delay) = retry_after {
            return format!(
                "API key unavailable for [{names}]: all configured provider keys are temporarily unavailable; retry after about {}s",
                delay.as_secs().max(1)
            );
        }

        let mut reasons = status_counts
            .into_iter()
            .filter(|(status, _)| *status != "available")
            .map(|(status, count)| format!("{status}: {count}"))
            .collect::<Vec<_>>();
        if empty > 0 {
            reasons.push(format!("empty: {empty}"));
        }
        reasons.sort();
        let reason = if reasons.is_empty() {
            "no usable configured key was selected".to_string()
        } else {
            reasons.join(", ")
        };
        format!("API key unavailable for [{names}]: all configured provider keys are unusable ({reason})")
    }

    fn remember_min_retry_delay(slot: &mut Option<Duration>, delay: Duration) {
        if delay.is_zero() {
            return;
        }
        match slot {
            Some(existing) if *existing <= delay => {}
            _ => *slot = Some(delay),
        }
    }

    fn key_retry_status(
        &self,
        logical_name: &str,
        key_id: &str,
        now: Instant,
        now_utc: DateTime<Utc>,
        state: &ProviderState,
    ) -> KeyRetryStatus {
        let mut retry_after = None;
        if let Some(until) = state.cooldowns.get(key_id).filter(|until| **until > now) {
            Self::remember_min_retry_delay(&mut retry_after, until.saturating_duration_since(now));
        }

        let (availability, remaining_seconds) =
            Self::key_health_blocked_in_state(state, logical_name, key_id, now_utc);
        match availability {
            KeyAvailability::Available => match retry_after {
                Some(delay) => KeyRetryStatus::RetryAfter(delay),
                None => KeyRetryStatus::Available,
            },
            KeyAvailability::Cooldown => {
                let remaining_seconds = remaining_seconds.unwrap_or(1).max(1) as u64;
                Self::remember_min_retry_delay(
                    &mut retry_after,
                    Duration::from_secs(remaining_seconds),
                );
                retry_after
                    .map(KeyRetryStatus::RetryAfter)
                    .unwrap_or(KeyRetryStatus::Unavailable)
            }
            KeyAvailability::AuthFailed
            | KeyAvailability::Disabled
            | KeyAvailability::Exhausted => KeyRetryStatus::Unavailable,
        }
    }

    pub(super) fn selected_secret_retry_delay(&self, keys: &[&str]) -> Option<Duration> {
        let now = Instant::now();
        let now_utc = Self::now_utc();
        let mut state = self
            .provider_state
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        state.prune_expired_cooldowns(now);

        let mut saw_configured_key = false;
        let mut retry_after = None;

        for key in keys {
            if let Some(entries) = state.secrets.get(*key) {
                for entry in entries
                    .iter()
                    .filter(|entry| !entry.value.trim().is_empty())
                {
                    saw_configured_key = true;
                    match self.key_retry_status(key, &entry.key_id, now, now_utc, &state) {
                        KeyRetryStatus::Available => return None,
                        KeyRetryStatus::RetryAfter(delay) => {
                            Self::remember_min_retry_delay(&mut retry_after, delay);
                        }
                        KeyRetryStatus::Unavailable => {}
                    }
                }
            }

            if Self::first_env(&[*key])
                .filter(|value| !value.trim().is_empty())
                .is_some_and(|value| !crate::provider_config::is_vault_alias(&value))
            {
                saw_configured_key = true;
                match self.key_retry_status(key, key, now, now_utc, &state) {
                    KeyRetryStatus::Available => return None,
                    KeyRetryStatus::RetryAfter(delay) => {
                        Self::remember_min_retry_delay(&mut retry_after, delay);
                    }
                    KeyRetryStatus::Unavailable => {}
                }
            }
        }

        if saw_configured_key {
            retry_after
        } else {
            None
        }
    }

    pub(super) async fn required_selected_secret_or_wait(
        &self,
        keys: &[&str],
        attempt: usize,
        context: &str,
    ) -> Result<Option<SelectedProviderSecret>, String> {
        self.refresh_key_health_from_db_if_stale().await;
        match self.required_selected_secret(keys) {
            Ok(selected) => Ok(Some(selected)),
            Err(err) => {
                if attempt < Self::MAX_ATTEMPTS {
                    if let Some(retry_after) = self.selected_secret_retry_delay(keys) {
                        let wait = retry_after.min(Self::retry_delay(attempt));
                        tracing::warn!(
                            "[provider] {context} keys are temporarily unavailable; retrying selection in {}ms",
                            wait.as_millis()
                        );
                        tokio::time::sleep(wait).await;
                        return Ok(None);
                    }
                }
                Err(err)
            }
        }
    }

    pub(super) fn mark_secret_rate_limited(
        &self,
        selected: &SelectedProviderSecret,
        retry_after: Option<u64>,
    ) {
        let now = Self::now_utc();
        let cooldown = retry_after.unwrap_or(60).clamp(1, 3600);
        let cooldown_until = now + chrono::Duration::seconds(cooldown as i64);
        let now_utc = now.to_rfc3339();
        let until = Instant::now() + Duration::from_secs(cooldown);
        let mut state = self
            .provider_state
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        state.cooldowns.insert(selected.key_id.clone(), until);
        let persisted = {
            let health = state.get_or_insert_health(&selected.logical_name, &selected.key_id);
            health.status = HEALTH_RATE_LIMITED.to_string();
            health.cooldown_until = Some(cooldown_until.to_rfc3339());
            health.last_attempt = Some(now_utc.clone());
            health.last_error = Some(format!("rate limited; retry after {cooldown}s"));
            health.error_count += 1;
            health.updated_at = now_utc;
            health.clone()
        };
        state.set_health_snapshot(
            &selected.logical_name,
            &selected.key_id,
            ProviderHealthSnapshot::from_health_parts(&persisted, Some(now), Some(cooldown_until)),
        );
        drop(state);
        tracing::warn!(
            "[provider] key {} for {} is rate-limited; cooling down for {}s",
            selected.key_id,
            selected.logical_name,
            cooldown
        );
        self.persist_key_health(&persisted);
    }

    #[cfg(test)]
    pub(crate) fn provider_key_id_for_tests(&self, keys: &[&str]) -> Option<String> {
        self.select_secret(keys).map(|selected| selected.key_id)
    }

    #[cfg(test)]
    pub(crate) fn mark_provider_key_rate_limited_for_tests(
        &self,
        key_id: &str,
        retry_after: Option<u64>,
    ) {
        let logical_name = crate::provider_config::parse_rotation_member_name(key_id)
            .map(|(prefix, _)| prefix)
            .unwrap_or(key_id);
        let selected = SelectedProviderSecret {
            logical_name: logical_name.to_string(),
            key_id: key_id.to_string(),
            value: String::new(),
        };
        self.mark_secret_rate_limited(&selected, retry_after);
        if let Some(health) = self.read_key_health_entry(&selected.logical_name, &selected.key_id) {
            self.persist_key_health_now(&health);
        }
    }

    #[cfg(test)]
    pub(crate) fn mark_provider_key_auth_failed_for_tests(&self, logical_name: &str, key_id: &str) {
        let selected = SelectedProviderSecret {
            logical_name: logical_name.to_string(),
            key_id: key_id.to_string(),
            value: String::new(),
        };
        self.mark_secret_auth_failed(&selected, Some("forced auth failure"));
        if let Some(health) = self.read_key_health_entry(logical_name, key_id) {
            self.persist_key_health_now(&health);
        }
    }

    pub(crate) fn record_provider_key_result(
        &self,
        logical_name: &str,
        key_id: &str,
        status_code: Option<u16>,
        outcome: Option<&str>,
        retry_after: Option<u64>,
        reason: Option<&str>,
    ) -> VaultKeyHealth {
        let selected = SelectedProviderSecret {
            logical_name: logical_name.to_string(),
            key_id: key_id.to_string(),
            value: String::new(),
        };
        let outcome = outcome.map(|value| value.to_ascii_lowercase());
        if status_code == Some(429)
            || matches!(outcome.as_deref(), Some("rate_limited" | "cooldown"))
        {
            self.mark_secret_rate_limited(&selected, retry_after);
        } else if matches!(status_code, Some(401 | 403))
            || matches!(outcome.as_deref(), Some("auth_failed"))
        {
            self.mark_secret_auth_failed(&selected, reason.or(Some("auth failure")));
        } else if matches!(outcome.as_deref(), Some("exhausted")) {
            self.with_key_health(logical_name, key_id, |health| {
                health.status = "exhausted".to_string();
                health.last_error = reason
                    .map(str::to_string)
                    .or_else(|| Some("key exhausted".to_string()));
                health.error_count += 1;
            });
        } else if status_code.is_some_and(|code| (200..300).contains(&code))
            || matches!(outcome.as_deref(), Some("success" | "ok"))
        {
            self.mark_secret_success(&selected);
        } else {
            self.with_key_health(logical_name, key_id, |health| {
                health.status = "error".to_string();
                health.last_error = reason
                    .map(str::to_string)
                    .or_else(|| status_code.map(|code| format!("provider returned HTTP {code}")));
                health.error_count += 1;
            });
        }
        self.read_key_health_entry(logical_name, key_id)
            .unwrap_or_else(|| VaultKeyHealth {
                logical_name: logical_name.to_string(),
                key_id: key_id.to_string(),
                ..VaultKeyHealth::default()
            })
    }

    pub(crate) fn record_provider_key_result_blocking(
        &self,
        logical_name: &str,
        key_id: &str,
        status_code: Option<u16>,
        outcome: Option<&str>,
        retry_after: Option<u64>,
        reason: Option<&str>,
    ) -> VaultKeyHealth {
        let health = self.record_provider_key_result(
            logical_name,
            key_id,
            status_code,
            outcome,
            retry_after,
            reason,
        );
        self.persist_key_health_now(&health);
        health
    }

    #[cfg(test)]
    pub(crate) fn provider_secret_for_tests(&self, keys: &[&str]) -> Option<String> {
        self.first_secret(keys)
    }

    #[cfg(test)]
    pub(crate) fn force_provider_health_reload_due_for_tests(&self) {
        let mut reload = self
            .provider_health_reload
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        reload.last_attempt =
            Instant::now().checked_sub(Self::KEY_HEALTH_RELOAD_TTL + Duration::from_secs(1));
    }
}
