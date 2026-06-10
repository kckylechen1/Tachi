// llm.rs — LLM & Embedding client for memory server
//
// Uses raw reqwest for OpenAI-compatible chat completions.
// SiliconFlow/Qwen still gets `enable_thinking: false` to avoid empty content.

use chrono::{DateTime, Utc};
use reqwest::header::{AUTHORIZATION, CONTENT_TYPE};
use serde::Serialize;
use serde_json::Value;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};
use std::time::{Duration, Instant};

use memory_core::vault::VaultKeyHealth;

const DEFAULT_CHAT_BASE_URL: &str = "https://api.siliconflow.cn/v1/chat/completions";
const DEFAULT_EXTRACT_MODEL: &str = "Qwen/Qwen3.5-27B";
const DEFAULT_REASONING_MODEL: &str = "Qwen/Qwen3.5-27B";
const HEALTH_OK: &str = "ok";
const HEALTH_COOLDOWN: &str = "cooldown";
const HEALTH_RATE_LIMITED: &str = "rate_limited";
const HEALTH_AUTH_FAILED: &str = "auth_failed";
const HEALTH_DISABLED: &str = "disabled";
const HEALTH_EXHAUSTED: &str = "exhausted";
const CLAUDE_CLI_FAILURE_COOLDOWN: Duration = Duration::from_secs(600);

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

#[derive(Clone)]
struct ChatLaneConfig {
    base_url: String,
    model: String,
    api_key_envs: Vec<&'static str>,
}

#[derive(Clone)]
pub(crate) struct ProviderSecret {
    pub(crate) key_id: String,
    pub(crate) value: String,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct ProviderKeyCooldownStatus {
    pub(crate) key_id: String,
    pub(crate) remaining_seconds: u64,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct ProviderPoolStatus {
    pub(crate) logical_name: String,
    pub(crate) total_keys: usize,
    pub(crate) available_keys: usize,
    pub(crate) rate_limited_keys: Vec<ProviderKeyCooldownStatus>,
    pub(crate) current_index: usize,
    pub(crate) strategy: &'static str,
}

#[derive(Clone)]
struct SelectedProviderSecret {
    logical_name: String,
    key_id: String,
    value: String,
}

#[derive(Clone, Copy)]
enum ChatLane {
    Extract,
    Distill,
    Reasoning,
    Summary,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum KeyAvailability {
    Available,
    Cooldown,
    AuthFailed,
    Disabled,
    Exhausted,
}

#[derive(Debug, Clone)]
struct ClaudeCliFailure {
    kind: ClaudeCliFailureKind,
    failed_at: Instant,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ClaudeCliFailureKind {
    SpawnFailed,
    Timeout,
}

impl ClaudeCliFailureKind {
    fn from_error(error: &str) -> Option<Self> {
        if error.contains("spawn failed") {
            Some(Self::SpawnFailed)
        } else if error.contains("timeout") {
            Some(Self::Timeout)
        } else {
            None
        }
    }

    fn as_str(self) -> &'static str {
        match self {
            Self::SpawnFailed => "spawn_failed",
            Self::Timeout => "timeout",
        }
    }
}

#[derive(Debug, Clone, Copy)]
struct ClaudeCliSkip {
    kind: ClaudeCliFailureKind,
    remaining: Duration,
}

fn non_empty_rerank_documents(documents: &[String]) -> (Vec<&String>, Vec<usize>) {
    documents
        .iter()
        .enumerate()
        .filter(|(_, doc)| !doc.trim().is_empty())
        .map(|(idx, doc)| (doc, idx))
        .unzip()
}

/// LLM and embedding client using Voyage API for embeddings
/// and lane-specific OpenAI-compatible chat providers.
#[derive(Clone)]
pub struct LlmClient {
    http: reqwest::Client,
    extract: ChatLaneConfig,
    distill: ChatLaneConfig,
    reasoning: ChatLaneConfig,
    summary: ChatLaneConfig,
    vault_db_path: Option<PathBuf>,
    provider_secrets: Arc<RwLock<HashMap<String, Vec<ProviderSecret>>>>,
    provider_cooldowns: Arc<RwLock<HashMap<String, Instant>>>,
    provider_indices: Arc<RwLock<HashMap<String, usize>>>,
    provider_health: Arc<RwLock<HashMap<String, HashMap<String, VaultKeyHealth>>>>,
    claude_cli_failure: Arc<RwLock<Option<ClaudeCliFailure>>>,
}

impl LlmClient {
    const MAX_ATTEMPTS: usize = 3;
    const BASE_RETRY_DELAY_MS: u64 = 500;

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

        let provider_health = vault_db_path
            .as_ref()
            .and_then(|path| Self::load_key_health_from_db(path).ok())
            .unwrap_or_default();

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
            provider_secrets: Arc::new(RwLock::new(HashMap::new())),
            provider_cooldowns: Arc::new(RwLock::new(HashMap::new())),
            provider_indices: Arc::new(RwLock::new(HashMap::new())),
            provider_health: Arc::new(RwLock::new(provider_health)),
            claude_cli_failure: Arc::new(RwLock::new(None)),
        })
    }

    fn load_key_health_from_db(
        path: &Path,
    ) -> Result<HashMap<String, HashMap<String, VaultKeyHealth>>, String> {
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

    fn key_health_blocked_at(
        &self,
        logical_name: &str,
        key_id: &str,
        now: DateTime<Utc>,
    ) -> (KeyAvailability, Option<i64>) {
        let status_map = self
            .provider_health
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let health = status_map
            .get(logical_name)
            .and_then(|members| members.get(key_id));

        let Some(health) = health else {
            return (KeyAvailability::Available, None);
        };

        if health.disabled {
            return (KeyAvailability::Disabled, None);
        }

        if health.auth_failed {
            return (KeyAvailability::AuthFailed, None);
        }

        let status = Self::status_from_key_health_status(health.status.as_str());
        if status == HEALTH_EXHAUSTED {
            return (KeyAvailability::Exhausted, None);
        }

        if status == HEALTH_RATE_LIMITED {
            if let Some(cooldown_until) = health.cooldown_until.as_deref() {
                if let Some(until) = Self::parse_timestamp(cooldown_until) {
                    let remaining_seconds = (until - now).num_seconds().max(0);
                    if until > now {
                        return (KeyAvailability::Cooldown, Some(remaining_seconds));
                    }
                }
            }
        }

        (KeyAvailability::Available, None)
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
            handle.spawn_blocking(move || {
                if std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    Self::persist_key_health_blocking(db_path, health);
                }))
                .is_err()
                {
                    tracing::warn!(
                        "[provider] key health persistence task panicked for {}:{}",
                        logical_name,
                        key_id
                    );
                }
            });
        } else {
            Self::persist_key_health_blocking(db_path, health);
        }
    }

    fn persist_key_health_now(&self, health: &VaultKeyHealth) {
        let Some(db_path) = self.vault_db_path.clone() else {
            return;
        };
        let mut health = health.clone();
        health.updated_at = Self::format_now_utc();
        Self::persist_key_health_blocking(db_path, health);
    }

    fn persist_key_health_blocking(db_path: PathBuf, health: VaultKeyHealth) {
        let Some(db_path) = db_path.to_str() else {
            tracing::warn!(
                "[provider] failed to persist vault key health for {}:{}: invalid db path",
                health.logical_name,
                health.key_id
            );
            return;
        };
        match memory_core::MemoryStore::open(db_path) {
            Ok(store) => {
                if let Err(err) = store.vault_upsert_key_health(&health) {
                    tracing::warn!(
                        "[provider] failed to persist vault key health for {}:{}: {err}",
                        health.logical_name,
                        health.key_id
                    );
                }
            }
            Err(err) => {
                tracing::warn!(
                    "[provider] failed to persist vault key health for {}:{}: {err}",
                    health.logical_name,
                    health.key_id
                );
            }
        }
    }

    fn read_key_health_entry(&self, logical_name: &str, key_id: &str) -> Option<VaultKeyHealth> {
        self.provider_health
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .get(logical_name)
            .and_then(|members| members.get(key_id))
            .cloned()
    }

    fn write_key_health_entry(&self, mut health: VaultKeyHealth) {
        health.updated_at = Self::format_now_utc();
        {
            let mut all = self
                .provider_health
                .write()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            all.entry(health.logical_name.clone())
                .or_default()
                .insert(health.key_id.clone(), health.clone());
        }
        self.persist_key_health(&health);
    }

    fn with_key_health(
        &self,
        logical_name: &str,
        key_id: &str,
        mutator: impl FnOnce(&mut VaultKeyHealth),
    ) {
        let existing = self
            .read_key_health_entry(logical_name, key_id)
            .unwrap_or_else(|| VaultKeyHealth {
                logical_name: logical_name.to_string(),
                key_id: key_id.to_string(),
                status: HEALTH_OK.to_string(),
                cooldown_until: None,
                last_success: None,
                last_attempt: None,
                last_error: None,
                error_count: 0,
                auth_failed: false,
                disabled: false,
                metadata: "{}".to_string(),
                updated_at: Self::format_now_utc(),
            });
        let mut health = existing;
        mutator(&mut health);
        health.last_attempt = Some(Self::format_now_utc());
        self.write_key_health_entry(health);
    }

    fn mark_secret_auth_failed(&self, selected: &SelectedProviderSecret, reason: Option<&str>) {
        self.with_key_health(&selected.logical_name, &selected.key_id, |health| {
            health.status = HEALTH_AUTH_FAILED.to_string();
            health.auth_failed = true;
            health.disabled = false;
            health.cooldown_until = None;
            health.last_error = reason.map(|value| value.to_string());
            health.error_count += 1;
        });
    }

    fn mark_secret_success(&self, selected: &SelectedProviderSecret) {
        self.with_key_health(&selected.logical_name, &selected.key_id, |health| {
            health.status = HEALTH_OK.to_string();
            health.auth_failed = false;
            health.last_success = Some(Self::format_now_utc());
            health.last_error = None;
            health.error_count = 0;
            health.cooldown_until = None;
        });

        self.provider_cooldowns
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .remove(&selected.key_id);
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

    fn lane(&self, lane: ChatLane) -> &ChatLaneConfig {
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

        let mut secrets = self
            .provider_secrets
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        secrets.insert(name.to_string(), entries);
        self.provider_indices
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .remove(name);
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
        self.provider_secrets
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clear();
        self.provider_cooldowns
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clear();
        self.provider_indices
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clear();
    }

    pub fn provider_secret_count(&self) -> usize {
        self.provider_secrets
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .len()
    }

    pub(crate) fn provider_pool_statuses(&self) -> Vec<ProviderPoolStatus> {
        let now = Instant::now();
        let now_utc = Self::now_utc();
        {
            let mut cooldowns = self
                .provider_cooldowns
                .write()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            cooldowns.retain(|_, until| *until > now);
        }

        let secrets = self
            .provider_secrets
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let cooldowns = self
            .provider_cooldowns
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let indices = self
            .provider_indices
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner());

        let mut statuses = secrets
            .iter()
            .map(|(logical_name, entries)| {
                let mut unavailable_keys = Vec::new();
                for entry in entries.iter() {
                    let (availability, remaining_seconds) =
                        self.key_health_blocked_at(logical_name, &entry.key_id, now_utc);
                    let memory_blocked = cooldowns
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
                            cooldowns
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
                    current_index: *indices.get(logical_name).unwrap_or(&0),
                    strategy: "round_robin_skip_cooldown",
                }
            })
            .collect::<Vec<_>>();
        statuses.sort_by(|a, b| a.logical_name.cmp(&b.logical_name));
        statuses
    }

    fn select_secret(&self, keys: &[&str]) -> Option<SelectedProviderSecret> {
        let now = Instant::now();
        let now_utc = Self::now_utc();
        {
            let mut cooldowns = self
                .provider_cooldowns
                .write()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            cooldowns.retain(|_, until| *until > now);
        }

        let vault_value = {
            let secrets = self
                .provider_secrets
                .read()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            let cooldowns = self
                .provider_cooldowns
                .read()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            let mut indices = self
                .provider_indices
                .write()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            keys.iter().find_map(|key| {
                secrets
                    .get(*key)
                    .and_then(|entries| {
                        if entries.is_empty() {
                            return None;
                        }
                        let start = *indices.get(*key).unwrap_or(&0);
                        let mut found_usable = None;
                        for offset in 0..entries.len() {
                            let idx = (start + offset) % entries.len();
                            if cooldowns.contains_key(&entries[idx].key_id) {
                                continue;
                            }

                            let (availability, remaining_seconds) =
                                self.key_health_blocked_at(key, &entries[idx].key_id, now_utc);
                            let unusable = match availability {
                                KeyAvailability::AuthFailed
                                | KeyAvailability::Disabled
                                | KeyAvailability::Exhausted => true,
                                KeyAvailability::Cooldown => remaining_seconds.unwrap_or(0) > 0,
                                KeyAvailability::Available => false,
                            };
                            if unusable {
                                continue;
                            }
                            found_usable = Some(idx);
                            break;
                        }

                        found_usable.and_then(|idx| {
                            indices.insert((*key).to_string(), (idx + 1) % entries.len());
                            entries.get(idx)
                        })
                    })
                    .map(|entry| SelectedProviderSecret {
                        logical_name: (*key).to_string(),
                        key_id: entry.key_id.clone(),
                        value: entry.value.trim().to_string(),
                    })
                    .filter(|entry| !entry.value.is_empty())
            })
        };

        vault_value.or_else(|| {
            let cooldowns = self
                .provider_cooldowns
                .read()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            keys.iter().find_map(|key| {
                let (availability, remaining_seconds) =
                    self.key_health_blocked_at(key, key, now_utc);
                let unusable = match availability {
                    KeyAvailability::AuthFailed
                    | KeyAvailability::Disabled
                    | KeyAvailability::Exhausted => true,
                    KeyAvailability::Cooldown => remaining_seconds.unwrap_or(0) > 0,
                    KeyAvailability::Available => false,
                };

                if unusable || cooldowns.contains_key(*key) {
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
    fn required_secret(&self, keys: &[&str]) -> Result<String, String> {
        self.first_secret(keys).ok_or_else(|| {
            "Missing API key. Add one to Tachi Vault or set the appropriate env var.".to_string()
        })
    }

    fn required_selected_secret(&self, keys: &[&str]) -> Result<SelectedProviderSecret, String> {
        self.select_secret(keys).ok_or_else(|| {
            "Missing API key. Add one to Tachi Vault or set the appropriate env var.".to_string()
        })
    }

    fn mark_secret_rate_limited(
        &self,
        selected: &SelectedProviderSecret,
        retry_after: Option<u64>,
    ) {
        let now = Self::now_utc();
        let cooldown = retry_after.unwrap_or(60).clamp(1, 3600);
        let until = Instant::now() + Duration::from_secs(cooldown);
        self.provider_cooldowns
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .insert(selected.key_id.clone(), until);
        tracing::warn!(
            "[provider] key {} for {} is rate-limited; cooling down for {}s",
            selected.key_id,
            selected.logical_name,
            cooldown
        );

        self.with_key_health(&selected.logical_name, &selected.key_id, |health| {
            health.status = HEALTH_RATE_LIMITED.to_string();
            health.cooldown_until =
                Some((now + chrono::Duration::seconds(cooldown as i64)).to_rfc3339());
            health.last_error = Some(format!("rate limited; retry after {cooldown}s"));
            health.error_count += 1;
        });
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

    fn should_disable_thinking(base_url: &str, model: &str) -> bool {
        // Check environment variable for explicit override
        if let Ok(env_val) = std::env::var("TACHI_DISABLE_THINKING_MODELS") {
            let env_lower = env_val.to_ascii_lowercase();
            if env_lower == "all" || env_lower == "1" || env_lower == "true" {
                return true;
            }
            if env_lower == "none" || env_lower == "0" || env_lower == "false" {
                return false;
            }
            // Treat as comma-separated model name patterns
            let model_lower = model.to_ascii_lowercase();
            for pattern in env_lower.split(',') {
                if model_lower.contains(pattern.trim()) {
                    return true;
                }
            }
            return false;
        }

        // Legacy heuristic: disable for specific provider/model combinations
        // This is kept as a fallback for backwards compatibility
        if !base_url.to_ascii_lowercase().contains("siliconflow") {
            return false;
        }
        let model = model.to_ascii_lowercase();
        model.contains("qwen") || model.contains("deepseek")
    }

    /// Call Voyage-4 embedding API and return 1024-dim f32 vector.
    /// Convenience wrapper around embed_voyage_batch for single-item use.
    pub async fn embed_voyage(&self, text: &str, input_type: &str) -> Result<Vec<f32>, String> {
        let results = self
            .embed_voyage_batch(&[text.to_string()], input_type)
            .await?;
        results
            .into_iter()
            .next()
            .ok_or_else(|| "Empty batch result".to_string())
    }

    /// Batch call Voyage-4 embedding API. Returns one 1024-dim f32 vector per input text.
    /// Voyage supports up to 128 inputs per request; this method handles chunking internally.
    pub async fn embed_voyage_batch(
        &self,
        texts: &[String],
        input_type: &str,
    ) -> Result<Vec<Vec<f32>>, String> {
        if texts.is_empty() {
            return Ok(vec![]);
        }

        const VOYAGE_MAX_BATCH: usize = 128;
        let mut all_embeddings: Vec<Vec<f32>> = Vec::with_capacity(texts.len());
        for chunk in texts.chunks(VOYAGE_MAX_BATCH) {
            let body = serde_json::json!({
                "model": "voyage-4",
                "input": chunk,
                "input_type": input_type
            });
            let mut response_json: Option<Value> = None;
            let mut last_err = String::new();
            for attempt in 1..=Self::MAX_ATTEMPTS {
                let selected = self.required_selected_secret(&["VOYAGE_API_KEY"])?;
                let response = self
                    .http
                    .post("https://api.voyageai.com/v1/embeddings")
                    .header(CONTENT_TYPE, "application/json")
                    .header(AUTHORIZATION, format!("Bearer {}", selected.value))
                    .json(&body)
                    .send()
                    .await;
                let response = match response {
                    Ok(response) => response,
                    Err(err) => {
                        last_err = format!("Voyage batch API request failed: {err}");
                        if attempt < Self::MAX_ATTEMPTS {
                            tokio::time::sleep(Self::retry_delay(attempt)).await;
                            continue;
                        }
                        return Err(last_err);
                    }
                };

                let status = response.status();
                let retry_after = response
                    .headers()
                    .get("retry-after")
                    .and_then(|v| v.to_str().ok())
                    .and_then(|v| v.parse::<u64>().ok());
                let text = response
                    .text()
                    .await
                    .map_err(|e| format!("Voyage batch response body read failed: {e}"))?;
                if status.as_u16() == 429 {
                    self.mark_secret_rate_limited(&selected, retry_after);
                    last_err = format!("Voyage batch API error: {} - {}", status, text);
                    if attempt < Self::MAX_ATTEMPTS {
                        continue;
                    }
                    return Err(last_err);
                }
                if status.as_u16() == 401 || status.as_u16() == 403 {
                    self.mark_secret_auth_failed(
                        &selected,
                        Some(&format!("Voyage batch auth failure {status}")),
                    );
                    return Err(format!("Voyage batch API error: {} - {}", status, text));
                }
                if !status.is_success() {
                    return Err(format!("Voyage batch API error: {} - {}", status, text));
                }
                response_json = Some(
                    serde_json::from_str(&text)
                        .map_err(|e| format!("Failed to parse Voyage batch response: {}", e))?,
                );
                self.mark_secret_success(&selected);
                break;
            }

            let json = response_json.ok_or_else(|| {
                if last_err.is_empty() {
                    "Voyage batch API failed without a response".to_string()
                } else {
                    last_err
                }
            })?;

            let data = json["data"]
                .as_array()
                .ok_or("Invalid Voyage batch response: missing data array")?;

            for item in data {
                let embedding = item["embedding"]
                    .as_array()
                    .ok_or("Invalid Voyage batch response: missing embedding in item")?;

                let vec: Vec<f32> = embedding
                    .iter()
                    .filter_map(|v| v.as_f64().map(|f| f as f32))
                    .collect();

                if vec.len() != 1024 {
                    return Err(format!("Expected 1024-dim embedding, got {}", vec.len()));
                }

                all_embeddings.push(vec);
            }
        }

        if all_embeddings.len() != texts.len() {
            return Err(format!(
                "Voyage batch returned {} embeddings for {} inputs",
                all_embeddings.len(),
                texts.len()
            ));
        }

        Ok(all_embeddings)
    }

    /// Call Voyage rerank API and return (original_index, relevance_score) pairs.
    pub async fn rerank_voyage(
        &self,
        query: &str,
        documents: &[String],
        top_k: usize,
    ) -> Result<Vec<(usize, f64)>, String> {
        let (filtered_docs, index_map) = non_empty_rerank_documents(documents);
        if filtered_docs.is_empty() {
            return Ok(vec![]);
        }
        let effective_top_k = top_k.max(1).min(filtered_docs.len());

        let body = serde_json::json!({
            "model": "rerank-2.5",
            "query": query,
            "documents": filtered_docs,
            "top_k": effective_top_k,
        });

        let mut json: Option<Value> = None;
        let mut last_err = String::new();
        for attempt in 1..=Self::MAX_ATTEMPTS {
            let selected =
                self.required_selected_secret(&["VOYAGE_RERANK_API_KEY", "VOYAGE_API_KEY"])?;
            let response = self
                .http
                .post("https://api.voyageai.com/v1/rerank")
                .header(CONTENT_TYPE, "application/json")
                .header(AUTHORIZATION, format!("Bearer {}", selected.value))
                .json(&body)
                .send()
                .await;
            let response = match response {
                Ok(response) => response,
                Err(err) => {
                    last_err = format!("Voyage rerank API request failed: {err}");
                    if attempt < Self::MAX_ATTEMPTS {
                        tokio::time::sleep(Self::retry_delay(attempt)).await;
                        continue;
                    }
                    return Err(last_err);
                }
            };

            let status = response.status();
            let retry_after = response
                .headers()
                .get("retry-after")
                .and_then(|v| v.to_str().ok())
                .and_then(|v| v.parse::<u64>().ok());
            let text = response
                .text()
                .await
                .map_err(|e| format!("Voyage rerank response body read failed: {e}"))?;
            if status.as_u16() == 429 {
                self.mark_secret_rate_limited(&selected, retry_after);
                last_err = format!("Voyage rerank API error: {} - {}", status, text);
                if attempt < Self::MAX_ATTEMPTS {
                    continue;
                }
                return Err(last_err);
            }
            if status.as_u16() == 401 || status.as_u16() == 403 {
                self.mark_secret_auth_failed(
                    &selected,
                    Some(&format!("Voyage rerank auth failure {status}")),
                );
                return Err(format!("Voyage rerank API error: {} - {}", status, text));
            }
            if !status.is_success() {
                return Err(format!("Voyage rerank API error: {} - {}", status, text));
            }
            json = Some(
                serde_json::from_str(&text)
                    .map_err(|e| format!("Failed to parse Voyage rerank response: {}", e))?,
            );
            self.mark_secret_success(&selected);
            break;
        }
        let json = json.ok_or_else(|| {
            if last_err.is_empty() {
                "Voyage rerank API failed without a response".to_string()
            } else {
                last_err
            }
        })?;
        let data = json["data"]
            .as_array()
            .ok_or("Invalid Voyage rerank response: missing data array")?;

        let mut out = Vec::with_capacity(data.len());
        for item in data {
            let filtered_index = item["index"]
                .as_u64()
                .ok_or("Invalid Voyage rerank response: missing index")?
                as usize;
            let relevance = item["relevance_score"]
                .as_f64()
                .ok_or("Invalid Voyage rerank response: missing relevance_score")?;
            let orig_index = index_map
                .get(filtered_index)
                .copied()
                .unwrap_or(filtered_index);
            out.push((orig_index, relevance));
        }
        Ok(out)
    }

    pub async fn call_extract_llm(
        &self,
        system: &str,
        user: &str,
        model: Option<&str>,
        temperature: f32,
        max_tokens: u32,
    ) -> Result<String, String> {
        self.call_lane_llm(
            ChatLane::Extract,
            system,
            user,
            model,
            temperature,
            max_tokens,
        )
        .await
    }

    /// Foundry batch distill and single-group fallback when
    /// `FOUNDRY_DISTILL_BACKEND=raw_api`.
    pub async fn call_distill_llm(
        &self,
        system: &str,
        user: &str,
        model: Option<&str>,
        temperature: f32,
        max_tokens: u32,
    ) -> Result<String, String> {
        self.call_lane_llm(
            ChatLane::Distill,
            system,
            user,
            model,
            temperature,
            max_tokens,
        )
        .await
    }

    fn claude_cli_skip_at(&self, now: Instant) -> Option<ClaudeCliSkip> {
        let failure = self
            .claude_cli_failure
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone();
        let failure = failure?;
        let elapsed = now.saturating_duration_since(failure.failed_at);
        if elapsed >= CLAUDE_CLI_FAILURE_COOLDOWN {
            return None;
        }
        Some(ClaudeCliSkip {
            kind: failure.kind,
            remaining: CLAUDE_CLI_FAILURE_COOLDOWN.saturating_sub(elapsed),
        })
    }

    fn claude_cli_skip(&self) -> Option<ClaudeCliSkip> {
        self.claude_cli_skip_at(Instant::now())
    }

    fn record_claude_cli_success(&self) {
        self.claude_cli_failure
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .take();
    }

    fn record_claude_cli_failure_at(&self, error: &str, failed_at: Instant) {
        let Some(kind) = ClaudeCliFailureKind::from_error(error) else {
            return;
        };
        *self
            .claude_cli_failure
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) =
            Some(ClaudeCliFailure { kind, failed_at });
    }

    fn record_claude_cli_failure(&self, error: &str) {
        self.record_claude_cli_failure_at(error, Instant::now());
    }

    pub async fn call_reasoning_llm(
        &self,
        system: &str,
        user: &str,
        model: Option<&str>,
        temperature: f32,
        max_tokens: u32,
    ) -> Result<String, String> {
        if let Some(skip) = self.claude_cli_skip() {
            tracing::debug!(
                "skipping claude-cli reasoning after recent {} failure; retry in {}s",
                skip.kind.as_str(),
                skip.remaining.as_secs().max(1),
            );
        } else {
            // Try Claude Code CLI first for higher-quality reasoning.
            match Self::call_claude_cli(system, user).await {
                Ok(response) => {
                    self.record_claude_cli_success();
                    tracing::info!(
                        "reasoning via claude-cli succeeded ({} chars)",
                        response.len()
                    );
                    return Ok(response);
                }
                Err(e) => {
                    self.record_claude_cli_failure(&e);
                    tracing::warn!("claude-cli reasoning failed, falling back to lane LLM: {e}");
                }
            }
        }
        self.call_lane_llm(
            ChatLane::Reasoning,
            system,
            user,
            model,
            temperature,
            max_tokens,
        )
        .await
    }

    async fn call_claude_cli(system: &str, user: &str) -> Result<String, String> {
        use std::process::Stdio;
        use tokio::io::AsyncWriteExt;
        use tokio::process::Command;

        let prompt = format!("<system>\n{system}\n</system>\n\n{user}");
        let mut child = Command::new("claude")
            .arg("-p")
            .arg("--output-format")
            .arg("text")
            .arg("--max-turns")
            .arg("1")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .map_err(|e| format!("claude cli spawn failed: {e}"))?;

        if let Some(mut stdin) = child.stdin.take() {
            stdin
                .write_all(prompt.as_bytes())
                .await
                .map_err(|e| format!("claude cli stdin write failed: {e}"))?;
        } else {
            return Err("claude cli stdin unavailable".to_string());
        }

        // Add timeout protection (5 minutes) to prevent indefinite blocking
        let output = tokio::time::timeout(Duration::from_secs(300), child.wait_with_output())
            .await
            .map_err(|_| "claude cli timeout after 5 minutes".to_string())?
            .map_err(|e| format!("claude cli failed: {e}"))?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(format!(
                "claude cli exited {}: {}",
                output.status.code().unwrap_or(-1),
                stderr.chars().take(500).collect::<String>()
            ));
        }

        let text = String::from_utf8_lossy(&output.stdout).trim().to_string();
        if text.is_empty() {
            return Err("claude cli returned empty output".to_string());
        }
        Ok(text)
    }

    pub async fn call_summary_llm(
        &self,
        system: &str,
        user: &str,
        model: Option<&str>,
        temperature: f32,
        max_tokens: u32,
    ) -> Result<String, String> {
        self.call_lane_llm(
            ChatLane::Summary,
            system,
            user,
            model,
            temperature,
            max_tokens,
        )
        .await
    }

    async fn call_lane_llm(
        &self,
        lane: ChatLane,
        system: &str,
        user: &str,
        model_override: Option<&str>,
        temperature: f32,
        max_tokens: u32,
    ) -> Result<String, String> {
        let lane_cfg = self.lane(lane);
        let model = model_override.unwrap_or(&lane_cfg.model);

        let mut body = serde_json::json!({
            "model": model,
            "messages": [
                {"role": "system", "content": system},
                {"role": "user", "content": user}
            ],
            "temperature": temperature,
            "max_tokens": max_tokens
        });
        if Self::should_disable_thinking(&lane_cfg.base_url, model) {
            body["enable_thinking"] = Value::Bool(false);
        }

        let mut last_err = String::new();

        for attempt in 1..=Self::MAX_ATTEMPTS {
            let selected = self.required_selected_secret(&lane_cfg.api_key_envs)?;
            let resp = self
                .http
                .post(&lane_cfg.base_url)
                .header(CONTENT_TYPE, "application/json")
                .header(AUTHORIZATION, format!("Bearer {}", selected.value))
                .json(&body)
                .send()
                .await;

            let resp = match resp {
                Ok(r) => r,
                Err(e) => {
                    last_err = format!("HTTP request failed: {e}");
                    if attempt < Self::MAX_ATTEMPTS
                        && (e.is_timeout() || e.is_connect() || e.is_request())
                    {
                        eprintln!(
                            "[llm] transient error (attempt {}/{}): {e}; retrying",
                            attempt,
                            Self::MAX_ATTEMPTS
                        );
                        tokio::time::sleep(Self::retry_delay(attempt)).await;
                        continue;
                    }
                    return Err(last_err);
                }
            };

            let status = resp.status();
            let retry_after = resp
                .headers()
                .get("retry-after")
                .and_then(|v| v.to_str().ok())
                .and_then(|v| v.parse::<u64>().ok());
            let resp_text = resp
                .text()
                .await
                .unwrap_or_else(|e| format!("<read error: {e}>"));

            // Retry on 429 rate-limit or 5xx server errors
            if status.as_u16() == 429 {
                self.mark_secret_rate_limited(&selected, retry_after);
                last_err = format!("API error {status}: {resp_text}");
                if attempt < Self::MAX_ATTEMPTS {
                    continue;
                }
                return Err(last_err);
            }
            if status.as_u16() == 401 || status.as_u16() == 403 {
                self.mark_secret_auth_failed(
                    &selected,
                    Some(&format!("Chat auth failure {status}")),
                );
                return Err(format!("API error {status}: {resp_text}"));
            }
            if status.is_server_error() {
                last_err = format!("API error {status}: {resp_text}");
                if attempt < Self::MAX_ATTEMPTS {
                    let delay = if let Some(secs) = retry_after {
                        Duration::from_secs(secs)
                    } else {
                        Self::retry_delay(attempt)
                    };
                    eprintln!(
                        "[llm] API error {status} (attempt {}/{}); retrying after {}ms",
                        attempt,
                        Self::MAX_ATTEMPTS,
                        delay.as_millis()
                    );
                    tokio::time::sleep(delay).await;
                    continue;
                }
                return Err(last_err);
            }

            if !status.is_success() {
                return Err(format!("Chat API error {status}: {resp_text}"));
            }

            // Parse JSON response
            let json: Value = serde_json::from_str(&resp_text).map_err(|e| {
                format!("Failed to parse chat response JSON: {e} — raw: {resp_text}")
            })?;

            // Extract content from first choice
            let content = json["choices"].as_array().and_then(|choices| {
                choices.iter().find_map(|choice| {
                    choice["message"]["content"]
                        .as_str()
                        .map(str::trim)
                        .filter(|s| !s.is_empty())
                        .map(String::from)
                })
            });

            if let Some(text) = content {
                self.mark_secret_success(&selected);
                return Ok(text);
            }

            // Content was empty — build diagnostic info
            let finish_reason = json["choices"][0]["finish_reason"]
                .as_str()
                .unwrap_or("null");
            let usage = json
                .get("usage")
                .map(|u| u.to_string())
                .unwrap_or_else(|| "unknown".to_string());

            last_err = format!(
                "Empty assistant content (finish_reason={finish_reason}, usage={usage}, model={model})"
            );

            if attempt < Self::MAX_ATTEMPTS {
                eprintln!(
                    "[llm] empty content (attempt {}/{}): {last_err}; retrying",
                    attempt,
                    Self::MAX_ATTEMPTS
                );
                tokio::time::sleep(Self::retry_delay(attempt)).await;
                continue;
            }
        }

        Err(last_err)
    }

    /// Generate L0 summary using SUMMARY_PROMPT.
    ///
    /// On LLM error this falls back to a 100-char truncation of the input.
    /// This is intentional for *summary* (we always want some text), but is
    /// catastrophic for *distill* — the truncated input is never a real
    /// distillation. Distill callers MUST use `generate_distill` instead.
    pub async fn generate_summary(&self, text: &str) -> Result<String, String> {
        match self
            .call_summary_llm(crate::prompts::SUMMARY_PROMPT, text, None, 0.3, 100)
            .await
        {
            Ok(summary) => Ok(summary),
            Err(e) => {
                eprintln!("[llm] generate_summary fell back to truncation after error: {e}");
                // Fallback to truncation on error
                Ok(text.chars().take(100).collect())
            }
        }
    }

    /// Generate a distilled synthesis from concatenated source memories.
    ///
    /// Unlike `generate_summary`, this does NOT silently fall back to
    /// truncation on error. Callers (Foundry distill worker) want a hard
    /// failure so the job is marked failed/skipped rather than persisting
    /// a "frankenstein" memory whose text is just the prompt's input prefix.
    ///
    /// Historical bug: prior to this method, `generate_summary` was reused
    /// for distill and its silent fallback produced 15/23 (65%) garbage
    /// distill memories in the antigravity project DB after just two days.
    pub async fn generate_distill(&self, text: &str) -> Result<String, String> {
        let out = self
            .call_summary_llm(crate::prompts::SUMMARY_PROMPT, text, None, 0.4, 400)
            .await?;
        let trimmed = out.trim();
        if trimmed.is_empty() {
            return Err("LLM returned empty distill payload".to_string());
        }
        // Reject obvious echo-back of the input prefix (defensive double-check
        // in case a future LLM provider returns the prompt instead of an answer).
        let input_prefix: String = text.chars().take(60).collect();
        // Trim FIRST, then check non-empty: otherwise a whitespace-only prefix
        // produces an empty trimmed string and `starts_with("")` is always true,
        // rejecting every otherwise-valid LLM output. (Caught in PR #49 review.)
        let trimmed_prefix = input_prefix.trim();
        if !trimmed_prefix.is_empty() && trimmed.starts_with(trimmed_prefix) {
            return Err(
                "LLM distill output appears to echo the input prefix; rejecting".to_string(),
            );
        }
        Ok(trimmed.to_string())
    }

    /// Extract keywords + entities for search enrichment.
    pub async fn extract_metadata(&self, text: &str) -> Result<(Vec<String>, Vec<String>), String> {
        let response = self
            .call_extract_llm(
                crate::prompts::METADATA_EXTRACTION_PROMPT,
                text,
                None,
                0.2,
                400,
            )
            .await?;
        let json_str = Self::extract_json_payload(&response)?;
        let parsed: Value = serde_json::from_str(json_str).map_err(|e| {
            format!(
                "Failed to parse metadata JSON: {} - response was: {}",
                e, json_str
            )
        })?;
        let keywords = parsed
            .get("keywords")
            .and_then(Value::as_array)
            .map(|items| {
                items
                    .iter()
                    .filter_map(Value::as_str)
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                    .map(str::to_string)
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        let entities = parsed
            .get("entities")
            .and_then(Value::as_array)
            .map(|items| {
                items
                    .iter()
                    .filter_map(Value::as_str)
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                    .map(str::to_string)
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        Ok((keywords, entities))
    }

    /// Extract structured facts from text using EXTRACTION_PROMPT
    pub async fn extract_facts(&self, text: &str) -> Result<Vec<Value>, String> {
        let response = self
            .call_extract_llm(crate::prompts::EXTRACTION_PROMPT, text, None, 0.3, 2000)
            .await?;
        let json_str = Self::extract_json_payload(&response)?;

        if json_str.trim().is_empty() {
            return Err("LLM returned empty facts payload after stripping fences".to_string());
        }

        serde_json::from_str(json_str).map_err(|e| {
            format!(
                "Failed to parse facts JSON: {} - response was: {}",
                e, json_str
            )
        })
    }

    fn retry_delay(attempt: usize) -> Duration {
        let multiplier = 1u64 << attempt.saturating_sub(1).min(4);
        Duration::from_millis(Self::BASE_RETRY_DELAY_MS * multiplier)
    }

    /// Remove ```json markdown code fences from response
    pub fn strip_code_fence(text: &str) -> &str {
        let text = text.trim();
        let inner = if text.starts_with("```json") {
            text[7..].trim()
        } else if text.starts_with("```") {
            &text[3..]
        } else {
            return text;
        };

        if let Some(idx) = inner.rfind("```") {
            inner[..idx].trim()
        } else {
            inner
        }
    }

    /// Extract the first complete JSON object/array from an LLM response.
    ///
    /// Some reasoning models prepend hidden-thought text or other prose before
    /// the JSON even when the prompt asks for JSON-only. Keep strict JSON
    /// parsing, but feed the parser the first balanced JSON payload instead of
    /// the whole response.
    pub fn extract_json_payload(text: &str) -> Result<&str, String> {
        let text = Self::strip_code_fence(text).trim();
        let start = text
            .char_indices()
            .find_map(|(idx, ch)| matches!(ch, '{' | '[').then_some((idx, ch)))
            .ok_or_else(|| format!("No JSON object or array found in response: {text}"))?;
        let (start_idx, open) = start;
        let close = if open == '{' { '}' } else { ']' };
        let mut stack = vec![close];
        let mut in_string = false;
        let mut escaped = false;

        for (rel_idx, ch) in text[start_idx..].char_indices().skip(1) {
            if in_string {
                if escaped {
                    escaped = false;
                } else if ch == '\\' {
                    escaped = true;
                } else if ch == '"' {
                    in_string = false;
                }
                continue;
            }

            match ch {
                '"' => in_string = true,
                '{' => stack.push('}'),
                '[' => stack.push(']'),
                '}' | ']' => {
                    if stack.pop() != Some(ch) {
                        return Err(format!("Mismatched JSON delimiter in response: {text}"));
                    }
                    if stack.is_empty() {
                        let end_idx = start_idx + rel_idx + ch.len_utf8();
                        return Ok(&text[start_idx..end_idx]);
                    }
                }
                _ => {}
            }
        }

        Err(format!("Incomplete JSON payload in response: {text}"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    // NOTE: each test below MUST use a unique env-var name. Cargo runs
    // `#[test]` fns in parallel by default, so sharing a process-wide env var
    // causes ordering-dependent flakes (e.g. one test setting the var while
    // another asserts it's unset). See:
    //   crates/memory-server/src/tests.rs::home_test_lock for the pattern we
    //   use when an env var (HOME) genuinely cannot be uniquified.

    #[test]
    fn llm_client_initializes_without_provider_env() {
        // Unique key — guaranteed never set by any other test or by the host
        // shell, so this test is parallel-safe.
        const KEY: &str = "TACHI_TEST_ONLY_API_KEY_INIT_NO_ENV";
        std::env::remove_var(KEY);
        let client = LlmClient::new().expect("client should not require API keys at startup");

        assert!(client.provider_secret_for_tests(&[KEY]).is_none());
        assert!(client
            .required_secret(&[KEY])
            .expect_err("missing keys should fail at call time")
            .contains("Missing API key"));
    }

    #[test]
    fn claude_cli_failure_cache_skips_expensive_discovery_failures() {
        let client = LlmClient::new().expect("client should initialize");
        let failed_at = Instant::now();

        assert!(client.claude_cli_skip_at(failed_at).is_none());

        client.record_claude_cli_failure_at("claude cli spawn failed: not found", failed_at);
        let skip = client
            .claude_cli_skip_at(failed_at + Duration::from_secs(1))
            .expect("spawn failure should suppress immediate retries");
        assert_eq!(skip.kind, ClaudeCliFailureKind::SpawnFailed);
        assert!(skip.remaining <= CLAUDE_CLI_FAILURE_COOLDOWN);

        assert!(
            client
                .claude_cli_skip_at(
                    failed_at + CLAUDE_CLI_FAILURE_COOLDOWN + Duration::from_secs(1)
                )
                .is_none(),
            "failure cache should expire so Claude CLI can recover"
        );

        client.record_claude_cli_failure_at(
            "claude cli timeout after 5 minutes",
            failed_at + Duration::from_secs(5),
        );
        assert_eq!(
            client
                .claude_cli_skip_at(failed_at + Duration::from_secs(6))
                .expect("timeout should suppress immediate retries")
                .kind,
            ClaudeCliFailureKind::Timeout
        );

        client.record_claude_cli_success();
        assert!(client
            .claude_cli_skip_at(failed_at + Duration::from_secs(7))
            .is_none());
    }

    #[test]
    fn claude_cli_failure_cache_ignores_prompt_level_errors() {
        let client = LlmClient::new().expect("client should initialize");
        let now = Instant::now();

        client.record_claude_cli_failure_at("claude cli exited 1: bad prompt", now);
        assert!(
            client
                .claude_cli_skip_at(now + Duration::from_secs(1))
                .is_none(),
            "non-availability errors should not disable future CLI attempts"
        );
    }

    #[test]
    fn vault_provider_secret_overrides_env_value() {
        const KEY: &str = "TACHI_TEST_ONLY_API_KEY_VAULT_OVERRIDE";
        std::env::set_var(KEY, "env-value");
        let client = LlmClient::new().expect("client should initialize");

        client.set_provider_secret(KEY, "vault-value");

        assert_eq!(
            client.provider_secret_for_tests(&[KEY]).unwrap(),
            "vault-value"
        );
        std::env::remove_var(KEY);
    }

    #[test]
    fn provider_pool_status_reports_cooldown_without_secret_values() {
        const KEY: &str = "TACHI_TEST_ONLY_API_KEY_POOL_STATUS";
        let client = LlmClient::new().expect("client should initialize");
        client.set_provider_secret_pool(
            KEY,
            vec![
                ProviderSecret {
                    key_id: format!("{KEY}_1"),
                    value: "secret-one".to_string(),
                },
                ProviderSecret {
                    key_id: format!("{KEY}_2"),
                    value: "secret-two".to_string(),
                },
            ],
        );
        let first_key = format!("{KEY}_1");
        assert_eq!(
            client.provider_key_id_for_tests(&[KEY]).as_deref(),
            Some(first_key.as_str())
        );
        client.mark_provider_key_rate_limited_for_tests(&first_key, Some(60));

        let statuses = client.provider_pool_statuses();
        let status = statuses
            .iter()
            .find(|status| status.logical_name == KEY)
            .expect("pool status should include logical key");
        assert_eq!(status.total_keys, 2);
        assert_eq!(status.available_keys, 1);
        assert_eq!(status.rate_limited_keys[0].key_id, first_key);
        assert_eq!(status.current_index, 1);
        let raw = serde_json::to_string(&statuses).expect("serialize statuses");
        assert!(!raw.contains("secret-one"));
        assert!(!raw.contains("secret-two"));
    }

    #[test]
    fn env_fallback_skips_rate_limited_key() {
        const KEY: &str = "TACHI_TEST_ONLY_API_KEY_ENV_COOLDOWN";
        std::env::set_var(KEY, "env-secret");
        let client = LlmClient::new().expect("client should initialize");

        assert_eq!(
            client.provider_secret_for_tests(&[KEY]),
            Some("env-secret".to_string())
        );
        client.mark_provider_key_rate_limited_for_tests(KEY, Some(60));

        assert!(
            client.provider_secret_for_tests(&[KEY]).is_none(),
            "env fallback should not reuse a key while it is cooling down"
        );
        std::env::remove_var(KEY);
    }

    #[test]
    fn all_pool_keys_rate_limited_returns_none_if_all_blocked() {
        const KEY: &str = "TACHI_TEST_ONLY_API_KEY_ALL_COOLDOWN";
        let client = LlmClient::new().expect("client should initialize");
        client.set_provider_secret_pool(
            KEY,
            vec![
                ProviderSecret {
                    key_id: format!("{KEY}_1"),
                    value: "secret-one".to_string(),
                },
                ProviderSecret {
                    key_id: format!("{KEY}_2"),
                    value: "secret-two".to_string(),
                },
            ],
        );
        client.mark_provider_key_rate_limited_for_tests(&format!("{KEY}_1"), Some(60));
        client.mark_provider_key_rate_limited_for_tests(&format!("{KEY}_2"), Some(60));

        assert!(
            client.provider_key_id_for_tests(&[KEY]).is_none(),
            "pool selection should return None when all members are cooling down"
        );
    }

    #[test]
    fn all_pool_keys_auth_failed_returns_none() {
        const KEY: &str = "TACHI_TEST_ONLY_API_KEY_ALL_UNUSABLE";
        let client = LlmClient::new().expect("client should initialize");
        client.set_provider_secret_pool(
            KEY,
            vec![
                ProviderSecret {
                    key_id: format!("{KEY}_1"),
                    value: "secret-one".to_string(),
                },
                ProviderSecret {
                    key_id: format!("{KEY}_2"),
                    value: "secret-two".to_string(),
                },
            ],
        );
        client.mark_provider_key_auth_failed_for_tests(KEY, &format!("{KEY}_1"));
        client.mark_provider_key_auth_failed_for_tests(KEY, &format!("{KEY}_2"));
        assert_eq!(
            client.provider_key_id_for_tests(&[KEY]),
            None,
            "pool selection should not return blocked auth-failed members"
        );
    }

    #[tokio::test]
    async fn provider_key_health_persists_off_async_runtime_thread() {
        let temp = tempfile::tempdir().expect("temp vault db");
        let db_path = temp.path().join("vault.db");
        let client =
            LlmClient::new_with_vault_db(Some(&db_path)).expect("client should initialize");

        let health = client.record_provider_key_result(
            "TACHI_TEST_ONLY_API_KEY_ASYNC_PERSIST",
            "TACHI_TEST_ONLY_API_KEY_ASYNC_PERSIST_1",
            Some(429),
            None,
            Some(30),
            Some("provider throttled"),
        );
        assert_eq!(health.status, HEALTH_RATE_LIMITED);

        let mut persisted = None;
        for _ in 0..50 {
            if db_path.exists() {
                let store = memory_core::MemoryStore::open_read_only(db_path.to_str().unwrap())
                    .expect("open persisted vault db");
                persisted = store
                    .vault_get_key_health(
                        "TACHI_TEST_ONLY_API_KEY_ASYNC_PERSIST",
                        "TACHI_TEST_ONLY_API_KEY_ASYNC_PERSIST_1",
                    )
                    .expect("read persisted key health");
                if persisted.is_some() {
                    break;
                }
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }

        let persisted = persisted.expect("background key-health persist should finish");
        assert_eq!(persisted.status, HEALTH_RATE_LIMITED);
        assert_eq!(
            persisted.last_error.as_deref(),
            Some("rate limited; retry after 30s")
        );
    }

    #[test]
    fn expired_cooldown_reinstates_pool_key() {
        const KEY: &str = "TACHI_TEST_ONLY_API_KEY_EXPIRED_COOLDOWN";
        let client = LlmClient::new().expect("client should initialize");
        client.set_provider_secret_pool(
            KEY,
            vec![ProviderSecret {
                key_id: format!("{KEY}_1"),
                value: "secret-one".to_string(),
            }],
        );
        let key_id = format!("{KEY}_1");
        client.mark_provider_key_rate_limited_for_tests(&key_id, Some(1));
        std::thread::sleep(Duration::from_millis(1100));

        assert_eq!(
            client.provider_key_id_for_tests(&[KEY]).as_deref(),
            Some(key_id.as_str())
        );
        assert!(client
            .provider_pool_statuses()
            .into_iter()
            .find(|status| status.logical_name == KEY)
            .is_some_and(|status| status.rate_limited_keys.is_empty()));
    }

    #[tokio::test]
    #[allow(clippy::await_holding_lock)]
    async fn chat_lane_retries_with_next_pool_key_after_429() {
        use axum::{
            extract::State,
            http::{HeaderMap, StatusCode},
            response::IntoResponse,
            routing::post,
            Json, Router,
        };
        use std::sync::{Arc, Mutex};

        struct EnvRestore {
            key: &'static str,
            original: Option<std::ffi::OsString>,
        }

        impl EnvRestore {
            fn set(key: &'static str, value: impl AsRef<std::ffi::OsStr>) -> Self {
                let original = std::env::var_os(key);
                std::env::set_var(key, value);
                Self { key, original }
            }
        }

        impl Drop for EnvRestore {
            fn drop(&mut self) {
                if let Some(value) = self.original.as_ref() {
                    std::env::set_var(self.key, value);
                } else {
                    std::env::remove_var(self.key);
                }
            }
        }

        let _guard = crate::utils::global_test_lock()
            .lock()
            .unwrap_or_else(|e| e.into_inner());

        let seen_auth = Arc::new(Mutex::new(Vec::<String>::new()));
        let app = Router::new()
            .route(
                "/chat/completions",
                post(
                    |State(seen_auth): State<Arc<Mutex<Vec<String>>>>,
                     headers: HeaderMap,
                     Json(_body): Json<Value>| async move {
                        let auth = headers
                            .get(AUTHORIZATION)
                            .and_then(|value| value.to_str().ok())
                            .unwrap_or("")
                            .to_string();
                        seen_auth
                            .lock()
                            .unwrap_or_else(|e| e.into_inner())
                            .push(auth.clone());
                        if auth == "Bearer pool-secret-one" {
                            return (
                                StatusCode::TOO_MANY_REQUESTS,
                                [("retry-after", "120")],
                                "rate limited",
                            )
                                .into_response();
                        }
                        Json(serde_json::json!({
                            "choices": [
                                {
                                    "message": {
                                        "role": "assistant",
                                        "content": "ok from second key"
                                    },
                                    "finish_reason": "stop"
                                }
                            ],
                            "usage": {"prompt_tokens": 1, "completion_tokens": 1, "total_tokens": 2}
                        }))
                        .into_response()
                    },
                ),
            )
            .with_state(seen_auth.clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind mock provider");
        let port = listener.local_addr().expect("mock provider addr").port();
        let server_task = tokio::spawn(async move {
            axum::serve(listener, app).await.expect("mock provider");
        });

        let _base_guard = EnvRestore::set(
            "EXTRACT_BASE_URL",
            format!("http://127.0.0.1:{port}/chat/completions"),
        );
        let _model_guard = EnvRestore::set("EXTRACT_MODEL", "mock-model");
        let client = LlmClient::new().expect("client should initialize");
        client.set_provider_secret_pool(
            "EXTRACT_API_KEY",
            vec![
                ProviderSecret {
                    key_id: "EXTRACT_API_KEY_1".to_string(),
                    value: "pool-secret-one".to_string(),
                },
                ProviderSecret {
                    key_id: "EXTRACT_API_KEY_2".to_string(),
                    value: "pool-secret-two".to_string(),
                },
            ],
        );

        let out = client
            .call_extract_llm("system", "user", None, 0.0, 16)
            .await
            .expect("second pool key should succeed after first 429");
        assert_eq!(out, "ok from second key");

        let seen = seen_auth.lock().unwrap_or_else(|e| e.into_inner()).clone();
        assert_eq!(
            seen,
            vec![
                "Bearer pool-secret-one".to_string(),
                "Bearer pool-secret-two".to_string()
            ]
        );

        let status = client
            .provider_pool_statuses()
            .into_iter()
            .find(|status| status.logical_name == "EXTRACT_API_KEY")
            .expect("provider pool status should include extract key");
        assert_eq!(status.total_keys, 2);
        assert_eq!(status.available_keys, 1);
        assert_eq!(status.rate_limited_keys[0].key_id, "EXTRACT_API_KEY_1");
        assert!(status.rate_limited_keys[0].remaining_seconds >= 1);
        assert_eq!(
            client
                .provider_key_id_for_tests(&["EXTRACT_API_KEY"])
                .as_deref(),
            Some("EXTRACT_API_KEY_2")
        );

        let raw = serde_json::to_string(&status).expect("serialize status");
        assert!(!raw.contains("pool-secret-one"));
        assert!(!raw.contains("pool-secret-two"));

        server_task.abort();
    }

    #[test]
    fn rerank_document_filter_preserves_original_indices() {
        let docs = vec![
            "first".to_string(),
            "   ".to_string(),
            "second".to_string(),
            "".to_string(),
        ];

        let (filtered, index_map) = non_empty_rerank_documents(&docs);

        assert_eq!(filtered, vec![&docs[0], &docs[2]]);
        assert_eq!(index_map, vec![0, 2]);
    }

    #[test]
    fn reasoning_lane_declares_zhipu_key_aliases() {
        const KEY: &str = "TACHI_TEST_ONLY_ZAI_ALIAS_KEY";
        std::env::set_var(KEY, "zai-value");

        let client = LlmClient::new().expect("client should initialize");

        assert_eq!(
            client.provider_secret_for_tests(&[KEY]),
            Some("zai-value".to_string())
        );
        assert!(client.reasoning.api_key_envs.contains(&"ZAI_API_KEY"));
        assert!(client.reasoning.api_key_envs.contains(&"BIGMODEL_API_KEY"));
        assert!(client.distill.api_key_envs.contains(&"ZAI_API_KEY"));
        assert!(client.distill.api_key_envs.contains(&"BIGMODEL_API_KEY"));
        std::env::remove_var(KEY);
    }

    #[test]
    fn extract_json_payload_ignores_prefix_and_suffix() {
        let raw = "<think>ignore</think>\n{\"ok\": true}\nextra text";
        assert_eq!(
            LlmClient::extract_json_payload(raw).expect("json payload"),
            "{\"ok\": true}"
        );
    }
}
