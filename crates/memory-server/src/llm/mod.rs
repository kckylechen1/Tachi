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

mod embedding;
mod helpers;

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

#[derive(Debug, Clone, Serialize)]
pub(crate) struct ProviderHealthStatus {
    pub(crate) source_of_truth: &'static str,
    pub(crate) reload_ttl_secs: u64,
    pub(crate) last_attempt_at: Option<String>,
    pub(crate) last_success_at: Option<String>,
    pub(crate) last_success_age_secs: Option<u64>,
    pub(crate) last_error: Option<String>,
    pub(crate) persist_last_attempt_at: Option<String>,
    pub(crate) persist_last_success_at: Option<String>,
    pub(crate) persist_last_success_age_secs: Option<u64>,
    pub(crate) persist_last_error: Option<String>,
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

impl KeyAvailability {
    fn label(self) -> &'static str {
        match self {
            Self::Available => "available",
            Self::Cooldown => "cooldown",
            Self::AuthFailed => "auth_failed",
            Self::Disabled => "disabled",
            Self::Exhausted => "exhausted",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum KeyRetryStatus {
    Available,
    RetryAfter(Duration),
    Unavailable,
}

#[derive(Debug, Clone)]
struct ProviderHealthSnapshot {
    availability: KeyAvailability,
    cooldown_until: Option<DateTime<Utc>>,
    updated_at: Option<DateTime<Utc>>,
}

impl ProviderHealthSnapshot {
    fn from_health(health: &VaultKeyHealth) -> Self {
        Self::from_health_parts(
            health,
            LlmClient::parse_timestamp(&health.updated_at),
            health
                .cooldown_until
                .as_deref()
                .and_then(LlmClient::parse_timestamp),
        )
    }

    fn from_health_parts(
        health: &VaultKeyHealth,
        updated_at: Option<DateTime<Utc>>,
        cooldown_until: Option<DateTime<Utc>>,
    ) -> Self {
        let mut snapshot = Self {
            availability: KeyAvailability::Available,
            cooldown_until: None,
            updated_at,
        };

        if health.disabled {
            snapshot.availability = KeyAvailability::Disabled;
            return snapshot;
        }

        if health.auth_failed {
            snapshot.availability = KeyAvailability::AuthFailed;
            return snapshot;
        }

        match LlmClient::status_from_key_health_status(health.status.as_str()) {
            HEALTH_RATE_LIMITED => {
                snapshot.cooldown_until = cooldown_until;
                snapshot.availability = if snapshot.cooldown_until.is_some() {
                    KeyAvailability::Cooldown
                } else {
                    KeyAvailability::Available
                };
            }
            HEALTH_EXHAUSTED => {
                snapshot.availability = KeyAvailability::Exhausted;
            }
            HEALTH_AUTH_FAILED => {
                snapshot.availability = KeyAvailability::AuthFailed;
            }
            HEALTH_DISABLED => {
                snapshot.availability = KeyAvailability::Disabled;
            }
            _ => {}
        }

        snapshot
    }

    fn availability_at(&self, now: DateTime<Utc>) -> (KeyAvailability, Option<i64>) {
        if self.availability == KeyAvailability::Cooldown {
            if let Some(until) = self.cooldown_until {
                let remaining_seconds = (until - now).num_seconds().max(0);
                if until > now {
                    return (KeyAvailability::Cooldown, Some(remaining_seconds));
                }
            }
            return (KeyAvailability::Available, None);
        }

        (self.availability, None)
    }
}

#[derive(Default)]
struct ProviderState {
    secrets: HashMap<String, Vec<ProviderSecret>>,
    cooldowns: HashMap<String, Instant>,
    indices: HashMap<String, usize>,
    health: HashMap<String, HashMap<String, VaultKeyHealth>>,
    health_snapshots: HashMap<String, HashMap<String, ProviderHealthSnapshot>>,
}

#[derive(Debug, Clone)]
struct ProviderHealthReloadState {
    source_of_truth: &'static str,
    last_attempt: Option<Instant>,
    last_attempt_at: Option<String>,
    last_success: Option<Instant>,
    last_success_at: Option<String>,
    last_error: Option<String>,
}

#[derive(Debug, Clone, Default)]
struct ProviderHealthPersistState {
    last_attempt_at: Option<String>,
    last_success: Option<Instant>,
    last_success_at: Option<String>,
    last_error: Option<String>,
}

impl ProviderHealthReloadState {
    fn memory_only() -> Self {
        Self {
            source_of_truth: "memory_only",
            last_attempt: None,
            last_attempt_at: None,
            last_success: None,
            last_success_at: None,
            last_error: None,
        }
    }

    fn vault_db_attempt(now: Instant, now_utc: String) -> Self {
        Self {
            source_of_truth: "vault_db",
            last_attempt: Some(now),
            last_attempt_at: Some(now_utc),
            last_success: None,
            last_success_at: None,
            last_error: None,
        }
    }

    fn mark_success(&mut self, now: Instant, now_utc: String) {
        self.last_attempt = Some(now);
        self.last_attempt_at = Some(now_utc.clone());
        self.last_success = Some(now);
        self.last_success_at = Some(now_utc);
        self.last_error = None;
    }

    fn mark_error(&mut self, now: Instant, now_utc: String, error: String) {
        self.last_attempt = Some(now);
        self.last_attempt_at = Some(now_utc);
        self.last_error = Some(error);
    }
}

impl ProviderHealthPersistState {
    fn mark_success(&mut self, now: Instant, now_utc: String) {
        self.last_attempt_at = Some(now_utc.clone());
        self.last_success = Some(now);
        self.last_success_at = Some(now_utc);
        self.last_error = None;
    }

    fn mark_error(&mut self, now_utc: String, error: String) {
        self.last_attempt_at = Some(now_utc);
        self.last_error = Some(error);
    }
}

impl ProviderState {
    fn with_health(health: HashMap<String, HashMap<String, VaultKeyHealth>>) -> Self {
        let mut state = Self::default();
        for (logical_name, members) in health {
            for (key_id, health) in members {
                state.set_health_entry(logical_name.clone(), key_id, health);
            }
        }
        state
    }

    fn get_or_insert_health(&mut self, logical_name: &str, key_id: &str) -> &mut VaultKeyHealth {
        self.health
            .entry(logical_name.to_string())
            .or_default()
            .entry(key_id.to_string())
            .or_insert_with(|| VaultKeyHealth {
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
                updated_at: Utc::now().to_rfc3339(),
            })
    }

    fn set_health_entry(&mut self, logical_name: String, key_id: String, health: VaultKeyHealth) {
        let snapshot = ProviderHealthSnapshot::from_health(&health);
        self.set_health_entry_with_snapshot(logical_name, key_id, health, snapshot);
    }

    fn set_health_entry_with_snapshot(
        &mut self,
        logical_name: String,
        key_id: String,
        health: VaultKeyHealth,
        snapshot: ProviderHealthSnapshot,
    ) {
        self.health
            .entry(logical_name.clone())
            .or_default()
            .insert(key_id.clone(), health);
        self.health_snapshots
            .entry(logical_name)
            .or_default()
            .insert(key_id, snapshot);
    }

    fn set_health_snapshot(
        &mut self,
        logical_name: &str,
        key_id: &str,
        snapshot: ProviderHealthSnapshot,
    ) {
        self.health_snapshots
            .entry(logical_name.to_string())
            .or_default()
            .insert(key_id.to_string(), snapshot);
    }

    fn prune_expired_cooldowns(&mut self, now: Instant) {
        self.cooldowns.retain(|_, until| *until > now);
    }
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
        let error = error.trim_start();
        if error.starts_with("claude cli spawn failed:") {
            Some(Self::SpawnFailed)
        } else if error.starts_with("claude cli timeout after ") {
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
    provider_state: Arc<RwLock<ProviderState>>,
    provider_health_reload: Arc<RwLock<ProviderHealthReloadState>>,
    provider_health_persist: Arc<RwLock<ProviderHealthPersistState>>,
    claude_cli_failure: Arc<RwLock<Option<ClaudeCliFailure>>>,
}

impl LlmClient {
    const MAX_ATTEMPTS: usize = 3;
    const BASE_RETRY_DELAY_MS: u64 = 500;
    const RETRY_JITTER_PERCENT: u64 = 20;
    const KEY_HEALTH_RELOAD_TTL: Duration = Duration::from_secs(30);

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
    fn required_secret(&self, keys: &[&str]) -> Result<String, String> {
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

    fn selected_secret_retry_delay(&self, keys: &[&str]) -> Option<Duration> {
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

    async fn required_selected_secret_or_wait(
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

    fn mark_secret_rate_limited(
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
            let Some(selected) = self
                .required_selected_secret_or_wait(&lane_cfg.api_key_envs, attempt, "chat lane")
                .await?
            else {
                continue;
            };
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
            let resp_text = match resp.text().await {
                Ok(text) => text,
                Err(e) => {
                    if status.as_u16() == 429 {
                        self.mark_secret_rate_limited(&selected, retry_after);
                    } else if status.as_u16() == 401 || status.as_u16() == 403 {
                        self.mark_secret_auth_failed(
                            &selected,
                            Some(&format!("Chat auth failure {status}")),
                        );
                    }
                    last_err = format!("Chat response body read failed after HTTP {status}: {e}");
                    if attempt < Self::MAX_ATTEMPTS && status.is_server_error() {
                        tokio::time::sleep(Self::retry_delay(attempt)).await;
                        continue;
                    }
                    return Err(last_err);
                }
            };

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
    /// LLM failures are returned to callers so enrichment/backfill can record a
    /// real failure instead of storing a truncated input as if it were a summary.
    pub async fn generate_summary(&self, text: &str) -> Result<String, String> {
        self.call_summary_llm(crate::prompts::SUMMARY_PROMPT, text, None, 0.3, 100)
            .await
    }

    /// Generate a distilled synthesis from concatenated source memories.
    ///
    /// Distill callers (Foundry distill worker) want a hard failure so the job
    /// is marked failed/skipped rather than persisting a "frankenstein" memory
    /// whose text is just the prompt's input prefix.
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
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    // NOTE: each test below MUST use a unique env-var name. Cargo runs
    // `#[test]` fns in parallel by default, so sharing a process-wide env var
    // causes ordering-dependent flakes (e.g. one test setting the var while
    // another asserts it's unset). See:
    //   crates/memory-server/src/tests.rs::home_test_lock for the pattern we
    //   use when an env var (HOME) genuinely cannot be uniquified.

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

        client.record_claude_cli_failure_at(
            "claude cli exited 1: model output mentioned timeout",
            now,
        );
        assert!(
            client
                .claude_cli_skip_at(now + Duration::from_secs(1))
                .is_none(),
            "stderr content should not look like a process timeout"
        );

        client.record_claude_cli_failure_at("claude cli exited 1: prompt said spawn failed", now);
        assert!(
            client
                .claude_cli_skip_at(now + Duration::from_secs(1))
                .is_none(),
            "stderr content should not look like a spawn failure"
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
    fn provider_runtime_maps_share_one_state_lock() {
        const KEY: &str = "TACHI_TEST_ONLY_API_KEY_PROVIDER_STATE";
        let client = LlmClient::new().expect("client should initialize");
        let key_id = format!("{KEY}_1");
        client.set_provider_secret_pool(
            KEY,
            vec![ProviderSecret {
                key_id: key_id.clone(),
                value: "secret-one".to_string(),
            }],
        );

        assert_eq!(
            client.provider_key_id_for_tests(&[KEY]).as_deref(),
            Some(key_id.as_str())
        );
        client.mark_provider_key_rate_limited_for_tests(&key_id, Some(60));

        let state = client
            .provider_state
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        assert!(state.secrets.contains_key(KEY));
        assert_eq!(state.indices.get(KEY), Some(&0));
        assert!(state.cooldowns.contains_key(&key_id));
        assert_eq!(
            state
                .health
                .get(KEY)
                .and_then(|members| members.get(&key_id))
                .map(|health| health.status.as_str()),
            Some(HEALTH_RATE_LIMITED)
        );
        assert_eq!(
            state
                .health_snapshots
                .get(KEY)
                .and_then(|members| members.get(&key_id))
                .map(|snapshot| snapshot.availability),
            Some(KeyAvailability::Cooldown)
        );
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

        let err = client
            .required_secret(&[KEY])
            .expect_err("cooling pool should not be reported as a missing key");
        assert!(
            err.contains("temporarily unavailable"),
            "expected cooldown-specific error, got: {err}"
        );
        assert!(
            err.contains("retry after"),
            "expected retry guidance, got: {err}"
        );
    }

    #[test]
    fn retry_delay_adds_bounded_jitter_to_exponential_backoff() {
        let first = LlmClient::retry_delay_with_jitter(1, 0);
        assert!(first >= Duration::from_millis(LlmClient::BASE_RETRY_DELAY_MS));
        assert!(
            first
                <= Duration::from_millis(
                    LlmClient::BASE_RETRY_DELAY_MS
                        + (LlmClient::BASE_RETRY_DELAY_MS * LlmClient::RETRY_JITTER_PERCENT / 100)
                )
        );

        let later = LlmClient::retry_delay_with_jitter(3, 0);
        let later_base = LlmClient::BASE_RETRY_DELAY_MS * 4;
        assert!(later >= Duration::from_millis(later_base));
        assert!(
            later
                <= Duration::from_millis(
                    later_base + (later_base * LlmClient::RETRY_JITTER_PERCENT / 100)
                )
        );
    }

    #[test]
    fn retry_delay_jitter_varies_by_seed() {
        let first = LlmClient::retry_delay_with_jitter(2, 1);
        let second = LlmClient::retry_delay_with_jitter(2, 2);
        assert_ne!(first, second);
    }

    fn embedding_values(seed: f64) -> Vec<f64> {
        (0..1024).map(|idx| seed + idx as f64).collect()
    }

    #[test]
    fn voyage_batch_embeddings_accept_matching_response_indexes() {
        let data = vec![
            json!({"index": 0, "embedding": embedding_values(0.0)}),
            json!({"index": 1, "embedding": embedding_values(1000.0)}),
        ];

        let embeddings =
            parse_voyage_batch_embeddings(&data, 2).expect("matching indexes should parse");

        assert_eq!(embeddings.len(), 2);
        assert_eq!(embeddings[0][0], 0.0);
        assert_eq!(embeddings[1][0], 1000.0);
    }

    #[test]
    fn voyage_batch_embeddings_reject_mismatched_response_index() {
        let data = vec![
            json!({"index": 1, "embedding": embedding_values(1000.0)}),
            json!({"index": 0, "embedding": embedding_values(0.0)}),
        ];

        let err = parse_voyage_batch_embeddings(&data, 2)
            .expect_err("out-of-order response indexes should fail");

        assert!(err.contains("index mismatch"));
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
        let err = client
            .required_secret(&[KEY])
            .expect_err("auth-failed pool should not be reported as a missing key");
        assert!(
            err.contains("unusable") && err.contains("auth_failed"),
            "expected auth-failed reason, got: {err}"
        );
    }

    #[tokio::test]
    #[allow(clippy::await_holding_lock)]
    async fn generate_summary_propagates_llm_failures() {
        let _guard = crate::utils::global_test_lock()
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let _persist_guard = EnvRestore::set("TACHI_TEST_DISABLE_PROVIDER_KEY_HEALTH_PERSIST", "1");
        std::env::remove_var("SUMMARY_API_KEY");
        std::env::remove_var("SILICONFLOW_API_KEY");
        std::env::remove_var("EXTRACT_API_KEY");
        std::env::remove_var("REASONING_API_KEY");
        std::env::remove_var("ZAI_API_KEY");
        std::env::remove_var("BIGMODEL_API_KEY");

        let client = LlmClient::new().expect("client should initialize");
        let err = client
            .generate_summary("this text used to be silently truncated")
            .await
            .expect_err("summary should surface provider/key failures");

        assert!(
            err.contains("Missing API key") || err.contains("API key unavailable"),
            "expected provider error, got: {err}"
        );
        assert!(
            !err.contains("this text used to be silently truncated"),
            "summary errors must not return truncated input as success"
        );
    }

    #[tokio::test]
    #[allow(clippy::await_holding_lock)]
    async fn chat_lane_reports_response_body_read_errors() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        let _guard = crate::utils::global_test_lock()
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let _persist_guard = EnvRestore::set("TACHI_TEST_DISABLE_PROVIDER_KEY_HEALTH_PERSIST", "1");

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind broken provider");
        let port = listener.local_addr().expect("provider addr").port();
        let server_task = tokio::spawn(async move {
            if let Ok((mut socket, _)) = listener.accept().await {
                let mut buf = [0u8; 2048];
                let _ = socket.read(&mut buf).await;
                let response =
                    b"HTTP/1.1 200 OK\r\ncontent-length: 64\r\ncontent-type: application/json\r\n\r\n{\"choices\"";
                let _ = socket.write_all(response).await;
                let _ = socket.shutdown().await;
            }
        });

        let _base_guard = EnvRestore::set(
            "EXTRACT_BASE_URL",
            format!("http://127.0.0.1:{port}/chat/completions"),
        );
        let _model_guard = EnvRestore::set("EXTRACT_MODEL", "mock-model");
        let _key_guard = EnvRestore::set("EXTRACT_API_KEY", "test-key");

        let client = LlmClient::new().expect("client should initialize");
        let err = client
            .call_extract_llm("system", "user", None, 0.0, 16)
            .await
            .expect_err("truncated provider body should be a body read error");

        assert!(
            err.contains("Chat response body read failed after HTTP 200 OK"),
            "expected body read error, got: {err}"
        );
        assert!(
            !err.contains("<read error:"),
            "body read failures must not be converted into synthetic body text"
        );

        server_task.abort();
    }

    #[tokio::test]
    #[allow(clippy::await_holding_lock)]
    async fn provider_key_health_blocking_persist_honors_test_disable_env() {
        let _guard = crate::utils::global_test_lock()
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let _persist_guard = EnvRestore::set("TACHI_TEST_DISABLE_PROVIDER_KEY_HEALTH_PERSIST", "1");
        let temp = tempfile::tempdir().expect("temp vault db");
        let db_path = temp.path().join("vault.db");
        let client =
            LlmClient::new_with_vault_db(Some(&db_path)).expect("client should initialize");

        let health = client.record_provider_key_result_blocking(
            "TACHI_TEST_ONLY_API_KEY_DISABLED_PERSIST",
            "TACHI_TEST_ONLY_API_KEY_DISABLED_PERSIST_1",
            Some(429),
            None,
            Some(30),
            Some("provider throttled"),
        );

        assert_eq!(health.status, HEALTH_RATE_LIMITED);
        assert!(
            !db_path.exists(),
            "blocking persist should not create a DB when test persistence is disabled"
        );
        let status = client.provider_health_status();
        assert!(status.persist_last_attempt_at.is_none());
        assert!(status.persist_last_error.is_none());
    }

    #[allow(clippy::await_holding_lock)]
    #[tokio::test]
    async fn provider_key_health_persists_off_async_runtime_thread() {
        let _lock = crate::utils::global_test_lock()
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let _persist_guard = EnvRestore::set("TACHI_TEST_DISABLE_PROVIDER_KEY_HEALTH_PERSIST", "0");
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
                if let Ok(store) = memory_core::MemoryStore::open(db_path.to_str().unwrap()) {
                    persisted = store
                        .vault_get_key_health(
                            "TACHI_TEST_ONLY_API_KEY_ASYNC_PERSIST",
                            "TACHI_TEST_ONLY_API_KEY_ASYNC_PERSIST_1",
                        )
                        .ok()
                        .flatten();
                    if persisted.is_some() {
                        break;
                    }
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

        let mut status = client.provider_health_status();
        for _ in 0..50 {
            if status.persist_last_success_at.is_some() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
            status = client.provider_health_status();
        }
        assert!(status.persist_last_attempt_at.is_some());
        assert!(status.persist_last_success_at.is_some());
        assert!(status.persist_last_error.is_none());
    }

    #[allow(clippy::await_holding_lock)]
    #[tokio::test]
    async fn provider_key_health_persist_errors_are_visible_in_status() {
        let _lock = crate::utils::global_test_lock()
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let _persist_guard = EnvRestore::set("TACHI_TEST_DISABLE_PROVIDER_KEY_HEALTH_PERSIST", "0");
        let temp = tempfile::tempdir().expect("temp vault db");
        let db_path = temp.path().join("missing-parent").join("vault.db");
        let client =
            LlmClient::new_with_vault_db(Some(&db_path)).expect("client should initialize");

        let health = client.record_provider_key_result(
            "TACHI_TEST_ONLY_API_KEY_PERSIST_ERROR",
            "TACHI_TEST_ONLY_API_KEY_PERSIST_ERROR_1",
            Some(429),
            None,
            Some(30),
            Some("provider throttled"),
        );
        assert_eq!(health.status, HEALTH_RATE_LIMITED);

        let mut status = client.provider_health_status();
        for _ in 0..50 {
            if status.persist_last_error.is_some() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
            status = client.provider_health_status();
        }

        let error = status
            .persist_last_error
            .as_deref()
            .expect("persist error should be visible");
        assert!(status.persist_last_attempt_at.is_some());
        assert!(status.persist_last_success_at.is_none());
        assert!(
            error.contains("persist vault key health for TACHI_TEST_ONLY_API_KEY_PERSIST_ERROR"),
            "unexpected persist error: {error}"
        );
    }

    #[tokio::test]
    async fn provider_key_health_reloads_external_db_cooldowns_before_selection() {
        const KEY: &str = "TACHI_TEST_ONLY_API_KEY_RELOAD_COOLDOWN";
        let temp = tempfile::tempdir().expect("temp vault db");
        let db_path = temp.path().join("vault.db");
        let store = memory_core::MemoryStore::open(db_path.to_str().unwrap()).expect("open db");
        drop(store);

        let client =
            LlmClient::new_with_vault_db(Some(&db_path)).expect("client should initialize");
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

        let now = Utc::now();
        let store = memory_core::MemoryStore::open(db_path.to_str().unwrap()).expect("open db");
        store
            .vault_upsert_key_health(&VaultKeyHealth {
                logical_name: KEY.to_string(),
                key_id: format!("{KEY}_1"),
                status: HEALTH_RATE_LIMITED.to_string(),
                cooldown_until: Some((now + chrono::Duration::seconds(60)).to_rfc3339()),
                last_attempt: Some(now.to_rfc3339()),
                last_error: Some("manual CLI cooldown".to_string()),
                updated_at: now.to_rfc3339(),
                ..VaultKeyHealth::default()
            })
            .expect("write external key health");
        drop(store);

        client.force_provider_health_reload_due_for_tests();
        let selected = client
            .required_selected_secret_or_wait(&[KEY], 1, "test reload")
            .await
            .expect("selection should not fail")
            .expect("second key should be selected");

        assert_eq!(selected.key_id, format!("{KEY}_2"));
        let status = client.provider_health_status();
        assert_eq!(status.source_of_truth, "vault_db");
        assert!(status.last_success_at.is_some());
        assert!(status.last_error.is_none());
    }

    #[tokio::test]
    async fn provider_key_health_reload_clears_local_cooldown_on_external_success() {
        const KEY: &str = "TACHI_TEST_ONLY_API_KEY_RELOAD_SUCCESS";
        let temp = tempfile::tempdir().expect("temp vault db");
        let db_path = temp.path().join("vault.db");
        let client =
            LlmClient::new_with_vault_db(Some(&db_path)).expect("client should initialize");
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
        client.mark_provider_key_rate_limited_for_tests(&format!("{KEY}_1"), Some(300));

        let now = Utc::now() + chrono::Duration::seconds(1);
        let store = memory_core::MemoryStore::open(db_path.to_str().unwrap()).expect("open db");
        store
            .vault_upsert_key_health(&VaultKeyHealth {
                logical_name: KEY.to_string(),
                key_id: format!("{KEY}_1"),
                status: HEALTH_OK.to_string(),
                last_success: Some(now.to_rfc3339()),
                updated_at: now.to_rfc3339(),
                ..VaultKeyHealth::default()
            })
            .expect("write external success health");
        drop(store);

        client.force_provider_health_reload_due_for_tests();
        let selected = client
            .required_selected_secret_or_wait(&[KEY], 1, "test reload")
            .await
            .expect("selection should not fail")
            .expect("first key should be reinstated");

        assert_eq!(selected.key_id, format!("{KEY}_1"));
        assert!(client
            .provider_pool_statuses()
            .into_iter()
            .find(|status| status.logical_name == KEY)
            .is_some_and(|status| status.available_keys == 2));
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

    #[test]
    fn cooldown_retry_ignores_permanently_failed_pool_members() {
        const KEY: &str = "TACHI_TEST_ONLY_API_KEY_MIXED_HEALTH";
        let client = LlmClient::new().expect("client should initialize");
        client.set_provider_secret_pool(
            KEY,
            vec![
                ProviderSecret {
                    key_id: format!("{KEY}_1"),
                    value: "bad-secret".to_string(),
                },
                ProviderSecret {
                    key_id: format!("{KEY}_2"),
                    value: "cooling-secret".to_string(),
                },
            ],
        );
        client.mark_provider_key_auth_failed_for_tests(KEY, &format!("{KEY}_1"));
        client.mark_provider_key_rate_limited_for_tests(&format!("{KEY}_2"), Some(30));

        let delay = client
            .selected_secret_retry_delay(&[KEY])
            .expect("cooling key should still drive retry timing");
        assert!(delay > Duration::ZERO);
        assert!(delay <= Duration::from_secs(30));
    }

    #[tokio::test]
    #[allow(clippy::await_holding_lock)]
    async fn chat_lane_waits_for_temporarily_unavailable_pool_key() {
        use axum::{extract::State, routing::post, Json, Router};
        use std::sync::{Arc, Mutex};

        let _guard = crate::utils::global_test_lock()
            .lock()
            .unwrap_or_else(|e| e.into_inner());

        let seen = Arc::new(Mutex::new(0usize));
        let app = Router::new()
            .route(
                "/chat/completions",
                post(|State(seen): State<Arc<Mutex<usize>>>| async move {
                    *seen.lock().unwrap_or_else(|e| e.into_inner()) += 1;
                    Json(serde_json::json!({
                        "choices": [
                            {
                                "message": {
                                    "role": "assistant",
                                    "content": "ok after cooldown"
                                },
                                "finish_reason": "stop"
                            }
                        ],
                        "usage": {"prompt_tokens": 1, "completion_tokens": 1, "total_tokens": 2}
                    }))
                }),
            )
            .with_state(seen.clone());
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
            vec![ProviderSecret {
                key_id: "EXTRACT_API_KEY_1".to_string(),
                value: "pool-secret-one".to_string(),
            }],
        );
        client.mark_provider_key_rate_limited_for_tests("EXTRACT_API_KEY_1", Some(1));

        let out = client
            .call_extract_llm("system", "user", None, 0.0, 16)
            .await
            .expect("cooldown retry should eventually use the pool key");
        assert_eq!(out, "ok after cooldown");
        assert_eq!(*seen.lock().unwrap_or_else(|e| e.into_inner()), 1);

        server_task.abort();
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
