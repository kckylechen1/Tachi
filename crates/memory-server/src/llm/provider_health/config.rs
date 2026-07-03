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
        let selected_provider_default = provider_default
            .filter(|default| selected_api_key == Some(default.api_key_env));
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
