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
        self.http
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
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
        let vault_db_path = vault_db_path.map(|path| path.to_path_buf());

        // Ensure a rustls crypto provider is installed before any HTTPS client
        // is built. reqwest uses rustls-no-provider, so this is required.
        crate::install_tls_provider();

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
            None,
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
            None,
        )?;

        // ── Foundry LLM layer (Distill + Reasoning) ──
        // DeepSeek is preferred when configured; otherwise foundry lanes still
        // fall back through the legacy reasoning/ZAI/extract/SiliconFlow chain.
        // Dedicated DISTILL_* env vars remain the explicit distill override.
        let reasoning = Self::load_lane(
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

        let distill = Self::load_lane(
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

        let http = Self::build_http_client()?;

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
            http: Arc::new(RwLock::new(http)),
            http_timeout_streak: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
            extract,
            distill,
            reasoning,
            summary,
            vault_db_path,
            provider_state: Arc::new(RwLock::new(ProviderState::with_health(provider_health))),
            provider_health_reload: Arc::new(RwLock::new(provider_health_reload)),
            provider_health_persist: Arc::new(RwLock::new(ProviderHealthPersistState::default())),
            claude_cli_failure: Arc::new(RwLock::new(None)),
            circuit_breakers: super::super::CircuitBreakerRegistry::new(),
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
}
