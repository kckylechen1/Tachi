use super::*;

const DEFAULT_CHAT_BASE_URL: &str = "https://api.siliconflow.cn/v1/chat/completions";
const DEFAULT_DEEPSEEK_BASE_URL: &str = "https://api.deepseek.com/chat/completions";
const DEFAULT_EXTRACT_MODEL: &str = "Qwen/Qwen3.5-27B";
const DEFAULT_REASONING_MODEL: &str = "Qwen/Qwen3.5-27B";
const DEFAULT_DEEPSEEK_DISTILL_MODEL: &str = "deepseek-chat";
const DEFAULT_DEEPSEEK_REASONING_MODEL: &str = "deepseek-reasoner";

struct ProviderLaneDefault {
    api_key_env: &'static str,
    base_url_envs: &'static [&'static str],
    model_envs: &'static [&'static str],
    base_url: &'static str,
    model: &'static str,
}

const DEEPSEEK_DISTILL_DEFAULT: ProviderLaneDefault = ProviderLaneDefault {
    api_key_env: "DEEPSEEK_API_KEY",
    base_url_envs: &["DEEPSEEK_DISTILL_BASE_URL", "DEEPSEEK_BASE_URL"],
    model_envs: &["DEEPSEEK_DISTILL_MODEL", "DEEPSEEK_MODEL"],
    base_url: DEFAULT_DEEPSEEK_BASE_URL,
    model: DEFAULT_DEEPSEEK_DISTILL_MODEL,
};

const DEEPSEEK_REASONING_DEFAULT: ProviderLaneDefault = ProviderLaneDefault {
    api_key_env: "DEEPSEEK_API_KEY",
    base_url_envs: &["DEEPSEEK_REASONING_BASE_URL", "DEEPSEEK_BASE_URL"],
    model_envs: &["DEEPSEEK_REASONING_MODEL", "DEEPSEEK_MODEL"],
    base_url: DEFAULT_DEEPSEEK_BASE_URL,
    model: DEFAULT_DEEPSEEK_REASONING_MODEL,
};

/// Cross-provider fallback defaults (#1197). These mirror the concrete
/// example in the issue (`extract: siliconflow -> deepseek`) and its inverse
/// for the foundry lanes (which prefer DeepSeek as *primary* when
/// `DEEPSEEK_API_KEY` is set, so their natural fallback is the SiliconFlow
/// front-line provider instead).
const DEEPSEEK_FALLBACK_DEFAULT: ProviderLaneDefault = ProviderLaneDefault {
    api_key_env: "DEEPSEEK_API_KEY",
    base_url_envs: &["DEEPSEEK_BASE_URL"],
    model_envs: &["DEEPSEEK_MODEL"],
    base_url: DEFAULT_DEEPSEEK_BASE_URL,
    model: DEFAULT_DEEPSEEK_DISTILL_MODEL,
};

const SILICONFLOW_FALLBACK_DEFAULT: ProviderLaneDefault = ProviderLaneDefault {
    api_key_env: "SILICONFLOW_API_KEY",
    base_url_envs: &["SILICONFLOW_BASE_URL"],
    model_envs: &["SILICONFLOW_MODEL"],
    base_url: DEFAULT_CHAT_BASE_URL,
    model: DEFAULT_EXTRACT_MODEL,
};

/// Construction-time provider/rerank configuration, separable from env reads.
///
/// `from_env()` reproduces the exact env-fallback chain that
/// `LlmClient::new_with_vault_db` previously ran inline. Callers that want
/// to bypass env (tests, programmatic config) can build this struct from
/// literals and pass it to [`LlmClient::new_with_config`].
///
/// Precedent: `ClaudePool::new_in_app_home` accepts already-resolved values
/// and does not re-read env at construction.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderRuntimeConfig {
    pub extract: ChatLaneConfig,
    pub summary: ChatLaneConfig,
    pub reasoning: ChatLaneConfig,
    pub distill: ChatLaneConfig,
    pub rerank: super::super::RerankConfig,
}

impl ProviderRuntimeConfig {
    /// Resolve provider and rerank config from env, reproducing the exact
    /// `load_lane`×4 + `RerankConfig::from_env` chain previously inline in
    /// `LlmClient::new_with_vault_db`. Byte-for-byte equivalent env reads;
    /// no semantic changes.
    pub fn from_env() -> Result<Self, String> {
        // ── Front-line LLM layer (Extract + Summary) ──
        // Extract: EXTRACT_* → SILICONFLOW_*
        let extract = super::super::LlmClient::load_lane(
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
            None,
        )?;

        // Summary: SUMMARY_* → EXTRACT_* → SILICONFLOW_*  (front-line default)
        let summary = super::super::LlmClient::load_lane(
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
            None,
        )?;

        // ── Foundry LLM layer (Distill + Reasoning) ──
        // DeepSeek is preferred when configured; otherwise foundry lanes still
        // fall back through the legacy reasoning/ZAI/extract/SiliconFlow chain.
        // Dedicated DISTILL_* env vars remain the explicit distill override.
        let reasoning = super::super::LlmClient::load_lane(
            "reasoning",
            &[
                "DEEPSEEK_API_KEY",
                "REASONING_API_KEY",
                "ZAI_API_KEY",
                "BIGMODEL_API_KEY",
                "DISTILL_API_KEY",
                "EXTRACT_API_KEY",
                "SILICONFLOW_API_KEY",
            ],
            &[
                "REASONING_BASE_URL",
                "DEEPSEEK_REASONING_BASE_URL",
                "DEEPSEEK_BASE_URL",
                "DISTILL_BASE_URL",
                "EXTRACT_BASE_URL",
                "SILICONFLOW_BASE_URL",
            ],
            &[
                "REASONING_MODEL",
                "DEEPSEEK_REASONING_MODEL",
                "DEEPSEEK_MODEL",
                "DISTILL_MODEL",
                "EXTRACT_MODEL",
                "SILICONFLOW_MODEL",
            ],
            "TACHI_BACKEND_REASONING_TIER",
            DEFAULT_REASONING_MODEL,
            Some(&DEEPSEEK_REASONING_DEFAULT),
        )?;

        let distill = super::super::LlmClient::load_lane(
            "distill",
            &[
                "DISTILL_API_KEY",
                "DEEPSEEK_API_KEY",
                "REASONING_API_KEY",
                "ZAI_API_KEY",
                "BIGMODEL_API_KEY",
                "EXTRACT_API_KEY",
                "SILICONFLOW_API_KEY",
            ],
            &[
                "DISTILL_BASE_URL",
                "DEEPSEEK_DISTILL_BASE_URL",
                "DEEPSEEK_BASE_URL",
                "REASONING_BASE_URL",
                "EXTRACT_BASE_URL",
                "SILICONFLOW_BASE_URL",
            ],
            &[
                "DISTILL_MODEL",
                "DEEPSEEK_DISTILL_MODEL",
                "DEEPSEEK_MODEL",
                "REASONING_MODEL",
                "EXTRACT_MODEL",
                "SILICONFLOW_MODEL",
            ],
            "TACHI_BACKEND_DISTILL_TIER",
            &reasoning.model,
            Some(&DEEPSEEK_DISTILL_DEFAULT),
        )?;

        // Eager rerank-config validation: unknown provider / local without
        // endpoint fail at construction, never mid-search as a silent hybrid
        // fallback (R2 review: config errors must not be swallowed).
        // Under cfg(test), take the process-wide test lock so we don't race
        // with embedding_rerank tests that temporarily set invalid providers.
        #[cfg(test)]
        let _test_lock = crate::test_support::global_test_lock().lock();
        let rerank = super::super::RerankConfig::from_env()?;

        Ok(Self {
            extract,
            summary,
            reasoning,
            distill,
            rerank,
        })
    }
}

/// Declarative per-lane cross-provider fallback (#1197): a *secondary*
/// provider's `ChatLaneConfig`, tried when the primary lane's whole key pool
/// is exhausted/auth-failed or its circuit breaker is open, instead of the
/// call stalling background pipelines silently.
///
/// A lane with `None` here behaves exactly as it did before #1197
/// (primary-only) — this is additive, never a behavior change for
/// deployments that don't configure a distinct fallback provider.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LaneFallbackConfig {
    pub extract: Option<ChatLaneConfig>,
    pub summary: Option<ChatLaneConfig>,
    pub reasoning: Option<ChatLaneConfig>,
    pub distill: Option<ChatLaneConfig>,
}

impl LaneFallbackConfig {
    /// Resolve fallback config from env. Each lane's fallback activates only
    /// if a distinct fallback api key resolves (either an explicit
    /// `{LANE}_FALLBACK_API_KEY` override or the lane's cross-provider
    /// convenience default) — never invents a fallback out of thin air.
    pub fn from_env() -> Self {
        let extract = Self::load_fallback(
            &["EXTRACT_FALLBACK_API_KEY", "DEEPSEEK_API_KEY"],
            &["EXTRACT_FALLBACK_BASE_URL", "DEEPSEEK_BASE_URL"],
            &["EXTRACT_FALLBACK_MODEL", "DEEPSEEK_MODEL"],
            &DEEPSEEK_FALLBACK_DEFAULT,
        );
        let summary = Self::load_fallback(
            &[
                "SUMMARY_FALLBACK_API_KEY",
                "EXTRACT_FALLBACK_API_KEY",
                "DEEPSEEK_API_KEY",
            ],
            &[
                "SUMMARY_FALLBACK_BASE_URL",
                "EXTRACT_FALLBACK_BASE_URL",
                "DEEPSEEK_BASE_URL",
            ],
            &[
                "SUMMARY_FALLBACK_MODEL",
                "EXTRACT_FALLBACK_MODEL",
                "DEEPSEEK_MODEL",
            ],
            &DEEPSEEK_FALLBACK_DEFAULT,
        );
        let reasoning = Self::load_fallback(
            &["REASONING_FALLBACK_API_KEY", "SILICONFLOW_API_KEY"],
            &["REASONING_FALLBACK_BASE_URL", "SILICONFLOW_BASE_URL"],
            &["REASONING_FALLBACK_MODEL", "SILICONFLOW_MODEL"],
            &SILICONFLOW_FALLBACK_DEFAULT,
        );
        let distill = Self::load_fallback(
            &["DISTILL_FALLBACK_API_KEY", "SILICONFLOW_API_KEY"],
            &["DISTILL_FALLBACK_BASE_URL", "SILICONFLOW_BASE_URL"],
            &["DISTILL_FALLBACK_MODEL", "SILICONFLOW_MODEL"],
            &SILICONFLOW_FALLBACK_DEFAULT,
        );
        Self {
            extract,
            summary,
            reasoning,
            distill,
        }
    }

    /// Returns `None` when no fallback api key resolves — the lane has no
    /// configured secondary provider and falls through to primary-only
    /// behavior identical to pre-#1197.
    fn load_fallback(
        api_key_envs: &[&'static str],
        base_url_envs: &[&str],
        model_envs: &[&str],
        default: &ProviderLaneDefault,
    ) -> Option<ChatLaneConfig> {
        super::super::LlmClient::first_env_key(api_key_envs)?;
        let base_url = super::super::LlmClient::first_env(base_url_envs)
            .unwrap_or_else(|| default.base_url.to_string());
        let model = super::super::LlmClient::first_env(model_envs)
            .unwrap_or_else(|| default.model.to_string());
        Some(ChatLaneConfig {
            base_url,
            model,
            api_key_envs: api_key_envs.to_vec(),
        })
    }
}

impl super::super::LlmClient {
    pub(in crate::llm) const MAX_ATTEMPTS: usize = 3;
    pub(in crate::llm) const BASE_RETRY_DELAY_MS: u64 = 500;
    pub(in crate::llm) const RETRY_JITTER_PERCENT: u64 = 20;
    pub(in crate::llm) const KEY_HEALTH_RELOAD_TTL: Duration = Duration::from_secs(30);

    // ── Recall-path fail-safe bounds (#926) ─────────────────────────────────
    // A blackholed embedding provider (dead proxy fake-IP with a live TCP
    // accept) once froze every embed-requiring recall for minutes. These caps
    // bound the recall path only (embed + rerank); distill/extract/chat keep
    // the client-wide 60s budget so long reasoning calls are not truncated.
    /// TCP connect timeout applied to the shared client — safe globally because
    /// no lane legitimately spends minutes *connecting*.
    pub(in crate::llm) const RECALL_CONNECT_TIMEOUT_SECS: u64 = 3;
    /// Per-request read/response deadline for embed & rerank. Overridable via
    /// `TACHI_RECALL_PROVIDER_TIMEOUT_SECS` (see `recall_request_timeout`).
    pub(in crate::llm) const RECALL_PROVIDER_TIMEOUT_SECS: u64 = 10;
    /// Attempt cap for embed & rerank (vs. the global `MAX_ATTEMPTS = 3`) so
    /// worst-case ≈ attempts × timeout stays bounded. Overridable via
    /// `TACHI_RECALL_PROVIDER_ATTEMPTS` (see `recall_max_attempts`).
    pub(in crate::llm) const RECALL_PROVIDER_ATTEMPTS: usize = 2;
    /// Consecutive recall-path timeouts that trigger a pooled-client rebuild.
    pub(in crate::llm) const POOL_TIMEOUT_REBUILD_THRESHOLD: usize = 3;

    /// Build the shared pooled HTTP client. Factored so the recall-path pool
    /// hygiene (#926) can rebuild an identically-configured client after a run
    /// of timeouts poisons the connection pool.
    pub(in crate::llm) fn build_http_client() -> Result<reqwest::Client, String> {
        reqwest::Client::builder()
            .connect_timeout(Duration::from_secs(Self::RECALL_CONNECT_TIMEOUT_SECS))
            .timeout(Duration::from_secs(60))
            .build()
            .map_err(|e| format!("Failed to build HTTP client: {e}"))
    }

    /// Clone the current pooled client out from behind the swap lock. reqwest
    /// clients are cheap to clone (internally `Arc`), and releasing the read
    /// lock before `.await` keeps a rebuild from being blocked by in-flight
    /// requests.
    pub(in crate::llm) fn http_client(&self) -> reqwest::Client {
        self.http.read().unwrap_or_else(|e| e.into_inner()).clone()
    }

    /// Per-request deadline for embed & rerank, honouring the same env-override
    /// idiom as `CLAUDE_POOL_TIMEOUT_SECS`.
    pub(in crate::llm) fn recall_request_timeout() -> Duration {
        let secs = std::env::var("TACHI_RECALL_PROVIDER_TIMEOUT_SECS")
            .ok()
            .and_then(|v| v.trim().parse::<u64>().ok())
            .filter(|s| *s > 0)
            .unwrap_or(Self::RECALL_PROVIDER_TIMEOUT_SECS);
        Duration::from_secs(secs)
    }

    /// Attempt cap for embed & rerank, honouring `TACHI_RECALL_PROVIDER_ATTEMPTS`.
    pub(in crate::llm) fn recall_max_attempts() -> usize {
        std::env::var("TACHI_RECALL_PROVIDER_ATTEMPTS")
            .ok()
            .and_then(|v| v.trim().parse::<usize>().ok())
            .filter(|a| *a >= 1)
            .unwrap_or(Self::RECALL_PROVIDER_ATTEMPTS)
    }

    /// Record the outcome of a recall-path provider request for pool hygiene.
    /// A timeout grows the consecutive-timeout streak; once it reaches
    /// `POOL_TIMEOUT_REBUILD_THRESHOLD` the pooled client is rebuilt (dropping
    /// the poisoned connection + forcing fresh DNS) and the streak resets. Any
    /// success resets the streak.
    pub(in crate::llm) fn note_recall_provider_outcome(&self, timed_out: bool) {
        use std::sync::atomic::Ordering;
        if !timed_out {
            self.http_timeout_streak.store(0, Ordering::Relaxed);
            return;
        }
        let streak = self.http_timeout_streak.fetch_add(1, Ordering::Relaxed) + 1;
        if streak >= Self::POOL_TIMEOUT_REBUILD_THRESHOLD {
            match Self::build_http_client() {
                Ok(fresh) => {
                    if let Ok(mut guard) = self.http.write() {
                        *guard = fresh;
                    }
                    self.http_timeout_streak.store(0, Ordering::Relaxed);
                    tracing::warn!(
                        "[provider] rebuilt pooled HTTP client after {streak} consecutive recall-path timeouts (#926)"
                    );
                }
                Err(e) => {
                    tracing::warn!("[provider] pooled HTTP client rebuild failed: {e}");
                }
            }
        }
    }

    /// Test-only accessor for the recall-path consecutive-timeout streak.
    /// Used by the #926-review regression coverage to assert the streak only
    /// grows on a genuine timeout-class outcome (including a body-read
    /// timeout on a provider that already sent headers), not on
    /// headers-received alone.
    #[cfg(any(test, feature = "test-support"))]
    #[doc(hidden)]
    pub fn recall_timeout_streak_for_tests(&self) -> usize {
        use std::sync::atomic::Ordering;
        self.http_timeout_streak.load(Ordering::Relaxed)
    }

    pub fn new() -> Result<Self, String> {
        Self::new_with_vault_db(None)
    }

    pub fn new_with_vault_db(vault_db_path: Option<&Path>) -> Result<Self, String> {
        Self::new_with_config_and_fallbacks(
            ProviderRuntimeConfig::from_env()?,
            LaneFallbackConfig::from_env(),
            vault_db_path,
        )
    }

    /// Build an `LlmClient` from an already-resolved [`ProviderRuntimeConfig`],
    /// skipping all env reads, with **no** cross-provider fallback configured
    /// (equivalent to pre-#1197 behavior). This is the construction seam:
    /// callers that want programmatic config (tests, future config-file
    /// loaders) pass a literal struct; `new_with_vault_db` goes through
    /// `new_with_config_and_fallbacks` instead so production construction
    /// also resolves `LaneFallbackConfig::from_env()`.
    pub fn new_with_config(
        config: ProviderRuntimeConfig,
        vault_db_path: Option<&Path>,
    ) -> Result<Self, String> {
        Self::new_with_config_and_fallbacks(config, LaneFallbackConfig::default(), vault_db_path)
    }

    /// Build an `LlmClient` from an already-resolved [`ProviderRuntimeConfig`]
    /// and [`LaneFallbackConfig`] (#1197), skipping all env reads. The
    /// fallback-aware sibling of `new_with_config` — tests that want to
    /// exercise cross-provider fallback deterministically (no env, no real
    /// network) should use this instead of setting `*_FALLBACK_*` env vars.
    pub fn new_with_config_and_fallbacks(
        config: ProviderRuntimeConfig,
        fallbacks: LaneFallbackConfig,
        vault_db_path: Option<&Path>,
    ) -> Result<Self, String> {
        let vault_db_path = vault_db_path.map(|path| path.to_path_buf());

        // Fail-closed rerank validation: injected `Local` without a non-empty
        // endpoint must error here, not defer to a runtime belt-and-suspenders
        // check. Shares the same `validate()` as the env path (#1096 R2).
        config.rerank.validate()?;

        // Ensure a rustls crypto provider is installed before any HTTPS client
        // is built. reqwest uses rustls-no-provider, so this is required.
        crate::install_tls_provider();

        let http = Self::build_http_client()?;

        let (provider_health, provider_health_reload) =
            Self::initial_key_health_from_db(vault_db_path.as_deref());

        // Warn when foundry lanes collapse to the same model/endpoint as extract.
        // This is expected when dedicated DISTILL_*/REASONING_* env vars are unset,
        // but the user should know so they can configure separation if needed.
        if config.distill.base_url == config.extract.base_url
            && config.distill.model == config.extract.model
        {
            tracing::info!(
                "LLM distill lane collapsed to extract endpoint ({}/{}). \
                 Set DISTILL_API_KEY / DISTILL_BASE_URL to separate.",
                config.extract.base_url,
                config.extract.model,
            );
        }

        Ok(Self {
            http: Arc::new(RwLock::new(http)),
            http_timeout_streak: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
            extract: config.extract,
            distill: config.distill,
            reasoning: config.reasoning,
            summary: config.summary,
            extract_fallback: fallbacks.extract,
            distill_fallback: fallbacks.distill,
            reasoning_fallback: fallbacks.reasoning,
            summary_fallback: fallbacks.summary,
            rerank_config: config.rerank,
            vault_db_path,
            provider_state: Arc::new(RwLock::new(ProviderState::with_health(provider_health))),
            provider_health_reload: Arc::new(RwLock::new(provider_health_reload)),
            provider_health_persist: Arc::new(RwLock::new(ProviderHealthPersistState::default())),
            claude_cli_failure: Arc::new(RwLock::new(None)),
            circuit_breakers: super::super::CircuitBreakerRegistry::new(),
            lane_outage: super::super::LaneOutageTracker::new(),
            #[cfg(test)]
            last_rerank_dispatch: Arc::new(std::sync::Mutex::new(None)),
        })
    }

    fn load_lane(
        lane: &str,
        api_key_envs: &[&'static str],
        base_url_envs: &[&str],
        model_envs: &[&str],
        tier_env: &str,
        default_model: &str,
        provider_default: Option<&ProviderLaneDefault>,
    ) -> Result<ChatLaneConfig, String> {
        let selected_api_key = Self::first_env_key(api_key_envs);
        let selected_provider_default =
            provider_default.filter(|default| selected_api_key == Some(default.api_key_env));
        let base_url = if let Some(default) = selected_provider_default {
            Self::first_env(default.base_url_envs).unwrap_or_else(|| default.base_url.to_string())
        } else {
            Self::first_env(base_url_envs).unwrap_or_else(|| DEFAULT_CHAT_BASE_URL.to_string())
        };
        let explicit = if let Some(default) = selected_provider_default {
            Self::first_env(default.model_envs)
        } else {
            model_envs.first().and_then(|&key| Self::env_value(key))
        };
        let fallback = if selected_provider_default.is_none() && model_envs.len() > 1 {
            Self::first_env(&model_envs[1..])
        } else {
            None
        };
        let default_model = selected_provider_default
            .map(|default| default.model)
            .unwrap_or(default_model);
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

    fn first_env_key(keys: &[&'static str]) -> Option<&'static str> {
        keys.iter()
            .copied()
            .find(|key| Self::env_value(key).is_some())
    }

    fn env_value(key: &str) -> Option<String> {
        std::env::var(key)
            .ok()
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty())
    }

    pub(super) fn first_env(keys: &[&str]) -> Option<String> {
        keys.iter().find_map(|key| {
            std::env::var(key)
                .ok()
                .map(|value| value.trim().to_string())
                .filter(|value| !value.is_empty())
        })
    }

    pub(in crate::llm) fn lane(&self, lane: ChatLane) -> &ChatLaneConfig {
        match lane {
            ChatLane::Extract => &self.extract,
            ChatLane::Distill => &self.distill,
            ChatLane::Reasoning => &self.reasoning,
            ChatLane::Summary => &self.summary,
        }
    }

    /// The configured cross-provider fallback for `lane`, if any (#1197).
    /// `None` means the lane is primary-only — either no fallback env/default
    /// resolved, or (via `new_with_config`, the env-free test seam) none was
    /// injected.
    pub(in crate::llm) fn fallback_lane(&self, lane: ChatLane) -> Option<ChatLaneConfig> {
        match lane {
            ChatLane::Extract => self.extract_fallback.clone(),
            ChatLane::Distill => self.distill_fallback.clone(),
            ChatLane::Reasoning => self.reasoning_fallback.clone(),
            ChatLane::Summary => self.summary_fallback.clone(),
        }
    }
}
