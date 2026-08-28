use super::*;
use crate::llm::auth_probe::{
    provider_descriptor_for_base_url, provider_descriptor_for_logical_key, ProviderProbeDescriptor,
    DEEPSEEK_AUTH_PROBE, SILICONFLOW_AUTH_PROBE, ZAI_AUTH_PROBE, ZAI_BIGMODEL_AUTH_PROBE,
};

const DEFAULT_CHAT_BASE_URL: &str = "https://api.siliconflow.cn/v1/chat/completions";
const DEFAULT_EXTRACT_MODEL: &str = "Qwen/Qwen3.5-27B";
const DEFAULT_REASONING_MODEL: &str = "Qwen/Qwen3.5-27B";

/// Construction-time authority for the two lane fields that provider
/// materialization is allowed to fill in. The bits are frozen on the client;
/// request paths never re-read env to decide whether a field was explicit.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct LaneAuthority(u8);

impl LaneAuthority {
    const BASE_URL_EXPLICIT: u8 = 1 << 0;
    const MODEL_EXPLICIT: u8 = 1 << 1;

    pub(super) const fn all_explicit() -> Self {
        Self(Self::BASE_URL_EXPLICIT | Self::MODEL_EXPLICIT)
    }

    pub(super) const fn has_explicit_base_url(self) -> bool {
        self.0 & Self::BASE_URL_EXPLICIT != 0
    }

    pub(super) const fn has_explicit_model(self) -> bool {
        self.0 & Self::MODEL_EXPLICIT != 0
    }

    fn from_fields(base_url_explicit: bool, model_explicit: bool) -> Self {
        let mut bits = 0;
        if base_url_explicit {
            bits |= Self::BASE_URL_EXPLICIT;
        }
        if model_explicit {
            bits |= Self::MODEL_EXPLICIT;
        }
        Self(bits)
    }
}

#[derive(Clone, Copy)]
struct ProviderLaneDefault {
    descriptor: &'static ProviderProbeDescriptor,
    base_url_envs: &'static [&'static str],
    model_envs: &'static [&'static str],
}

const DEEPSEEK_DISTILL_DEFAULT: ProviderLaneDefault = ProviderLaneDefault {
    descriptor: &DEEPSEEK_AUTH_PROBE,
    base_url_envs: &["DEEPSEEK_DISTILL_BASE_URL", "DEEPSEEK_BASE_URL"],
    model_envs: &["DEEPSEEK_DISTILL_MODEL", "DEEPSEEK_MODEL"],
};

const DEEPSEEK_REASONING_DEFAULT: ProviderLaneDefault = ProviderLaneDefault {
    descriptor: &DEEPSEEK_AUTH_PROBE,
    base_url_envs: &["DEEPSEEK_REASONING_BASE_URL", "DEEPSEEK_BASE_URL"],
    model_envs: &["DEEPSEEK_REASONING_MODEL", "DEEPSEEK_MODEL"],
};

/// Cross-provider fallback defaults (#1197). These mirror the concrete
/// example in the issue (`extract: siliconflow -> deepseek`) and its inverse
/// for the foundry lanes (which prefer DeepSeek as *primary* when
/// `DEEPSEEK_API_KEY` is set, so their natural fallback is the SiliconFlow
/// front-line provider instead).
const DEEPSEEK_FALLBACK_DEFAULT: ProviderLaneDefault = ProviderLaneDefault {
    descriptor: &DEEPSEEK_AUTH_PROBE,
    base_url_envs: &["DEEPSEEK_BASE_URL"],
    model_envs: &["DEEPSEEK_MODEL"],
};

const SILICONFLOW_FALLBACK_DEFAULT: ProviderLaneDefault = ProviderLaneDefault {
    descriptor: &SILICONFLOW_AUTH_PROBE,
    base_url_envs: &["SILICONFLOW_BASE_URL"],
    model_envs: &["SILICONFLOW_MODEL"],
};

const ZAI_LANE_DEFAULT: ProviderLaneDefault = ProviderLaneDefault {
    descriptor: &ZAI_AUTH_PROBE,
    base_url_envs: &["ZAI_BASE_URL"],
    model_envs: &["ZAI_MODEL"],
};

const BIGMODEL_LANE_DEFAULT: ProviderLaneDefault = ProviderLaneDefault {
    descriptor: &ZAI_BIGMODEL_AUTH_PROBE,
    base_url_envs: &["BIGMODEL_BASE_URL"],
    model_envs: &["BIGMODEL_MODEL"],
};

const NO_PROVIDER_DEFAULTS: &[&ProviderLaneDefault] = &[];
const FOUNDRY_REASONING_DEFAULTS: &[&ProviderLaneDefault] = &[
    &DEEPSEEK_REASONING_DEFAULT,
    &ZAI_LANE_DEFAULT,
    &BIGMODEL_LANE_DEFAULT,
    &SILICONFLOW_FALLBACK_DEFAULT,
];
const FOUNDRY_DISTILL_DEFAULTS: &[&ProviderLaneDefault] = &[
    &DEEPSEEK_DISTILL_DEFAULT,
    &ZAI_LANE_DEFAULT,
    &BIGMODEL_LANE_DEFAULT,
    &SILICONFLOW_FALLBACK_DEFAULT,
];

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
    /// `LlmClient::new_with_vault_db`. The public projection keeps the
    /// resolved values; the production constructor also retains the private
    /// field-authority bits needed for later Vault materialization.
    pub fn from_env() -> Result<Self, String> {
        let (config, _) = Self::from_env_with_authority()?;
        Ok(config)
    }

    pub(super) fn from_env_with_authority() -> Result<(Self, [LaneAuthority; 4]), String> {
        // ── Front-line LLM layer (Extract + Summary) ──
        // Extract: EXTRACT_* → SILICONFLOW_*
        let (extract, extract_authority) = super::super::LlmClient::load_lane(
            ChatLane::Extract,
            &["EXTRACT_API_KEY", "SILICONFLOW_API_KEY"],
            &[
                "EXTRACT_BASE_URL",
                "SILICONFLOW_BASE_URL",
                "EXTRACTOR_BASE_URL",
            ],
            &["EXTRACT_MODEL", "SILICONFLOW_MODEL", "EXTRACTOR_MODEL"],
            "TACHI_BACKEND_EXTRACT_TIER",
            DEFAULT_EXTRACT_MODEL,
            NO_PROVIDER_DEFAULTS,
        )?;

        // Summary: SUMMARY_* → EXTRACT_* → SILICONFLOW_*  (front-line default)
        let (summary, summary_authority) = super::super::LlmClient::load_lane(
            ChatLane::Summary,
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
            NO_PROVIDER_DEFAULTS,
        )?;

        // ── Foundry LLM layer (Distill + Reasoning) ──
        // DeepSeek is preferred when configured; otherwise foundry lanes still
        // fall back through the legacy reasoning/ZAI/extract/SiliconFlow chain.
        // Dedicated DISTILL_* env vars remain the explicit distill override.
        let (reasoning, reasoning_authority) = super::super::LlmClient::load_lane(
            ChatLane::Reasoning,
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
            FOUNDRY_REASONING_DEFAULTS,
        )?;

        let (distill, distill_authority) = super::super::LlmClient::load_lane(
            ChatLane::Distill,
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
            FOUNDRY_DISTILL_DEFAULTS,
        )?;

        // Eager rerank-config validation: unknown provider / local without
        // endpoint fail at construction, never mid-search as a silent hybrid
        // fallback (R2 review: config errors must not be swallowed).
        // Under cfg(test), take the process-wide test lock so we don't race
        // with embedding_rerank tests that temporarily set invalid providers.
        #[cfg(test)]
        let _test_lock = crate::test_support::global_test_lock().lock();
        let rerank = super::super::RerankConfig::from_env()?;

        Ok((
            Self {
                extract,
                summary,
                reasoning,
                distill,
                rerank,
            },
            // Keep the array order in lockstep with `ChatLane::index`:
            // Extract, Distill, Reasoning, Summary.
            [
                extract_authority,
                distill_authority,
                reasoning_authority,
                summary_authority,
            ],
        ))
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

#[derive(Clone, Copy)]
pub(super) struct LaneAuthoritySet {
    pub(super) primary: [LaneAuthority; 4],
    pub(super) fallback: [Option<LaneAuthority>; 4],
}

impl LaneAuthoritySet {
    pub(super) fn for_injected_config(fallbacks: &LaneFallbackConfig) -> Self {
        let explicit = LaneAuthority::all_explicit();
        Self {
            primary: [explicit; 4],
            fallback: [
                fallbacks.extract.as_ref().map(|_| explicit),
                fallbacks.distill.as_ref().map(|_| explicit),
                fallbacks.reasoning.as_ref().map(|_| explicit),
                fallbacks.summary.as_ref().map(|_| explicit),
            ],
        }
    }
}

impl LaneFallbackConfig {
    /// Resolve fallback config from env. Each lane's fallback activates only
    /// if a distinct fallback api key resolves (either an explicit
    /// `{LANE}_FALLBACK_API_KEY` override or the lane's cross-provider
    /// convenience default) — never invents a fallback out of thin air.
    pub fn from_env() -> Self {
        let (config, _) = Self::from_env_with_authority();
        config
    }

    pub(super) fn from_env_with_authority() -> (Self, [Option<LaneAuthority>; 4]) {
        let (extract, extract_authority) = Self::load_fallback(
            ChatLane::Extract,
            &["EXTRACT_FALLBACK_API_KEY", "DEEPSEEK_API_KEY"],
            &["EXTRACT_FALLBACK_BASE_URL", "DEEPSEEK_BASE_URL"],
            &["EXTRACT_FALLBACK_MODEL", "DEEPSEEK_MODEL"],
            &DEEPSEEK_FALLBACK_DEFAULT,
        );
        let (summary, summary_authority) = Self::load_fallback(
            ChatLane::Summary,
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
        let (reasoning, reasoning_authority) = Self::load_fallback(
            ChatLane::Reasoning,
            &["REASONING_FALLBACK_API_KEY", "SILICONFLOW_API_KEY"],
            &["REASONING_FALLBACK_BASE_URL", "SILICONFLOW_BASE_URL"],
            &["REASONING_FALLBACK_MODEL", "SILICONFLOW_MODEL"],
            &SILICONFLOW_FALLBACK_DEFAULT,
        );
        let (distill, distill_authority) = Self::load_fallback(
            ChatLane::Distill,
            &["DISTILL_FALLBACK_API_KEY", "SILICONFLOW_API_KEY"],
            &["DISTILL_FALLBACK_BASE_URL", "SILICONFLOW_BASE_URL"],
            &["DISTILL_FALLBACK_MODEL", "SILICONFLOW_MODEL"],
            &SILICONFLOW_FALLBACK_DEFAULT,
        );
        (
            Self {
                extract,
                summary,
                reasoning,
                distill,
            },
            [
                extract_authority,
                distill_authority,
                reasoning_authority,
                summary_authority,
            ],
        )
    }

    /// Returns `None` when no fallback api key resolves — the lane has no
    /// configured secondary provider and falls through to primary-only
    /// behavior identical to pre-#1197.
    fn load_fallback(
        lane: ChatLane,
        api_key_envs: &[&'static str],
        base_url_envs: &[&str],
        model_envs: &[&str],
        default: &ProviderLaneDefault,
    ) -> (Option<ChatLaneConfig>, Option<LaneAuthority>) {
        if super::super::LlmClient::first_env_key(api_key_envs).is_none() {
            return (None, None);
        }
        let authority = LaneAuthority::from_fields(
            base_url_envs.first().is_some_and(|key| {
                super::super::LlmClient::first_env(std::slice::from_ref(key)).is_some()
            }),
            model_envs.first().is_some_and(|key| {
                super::super::LlmClient::first_env(std::slice::from_ref(key)).is_some()
            }),
        );
        let base_url = super::super::LlmClient::first_env(base_url_envs)
            .unwrap_or_else(|| default.descriptor.chat.base_url.to_string());
        let model = super::super::LlmClient::first_env(model_envs)
            .unwrap_or_else(|| default.descriptor.chat_model_for_lane(lane).to_string());
        (
            Some(ChatLaneConfig {
                base_url,
                model,
                api_key_envs: api_key_envs.to_vec(),
            }),
            Some(authority),
        )
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
            // SECURITY (#1621): every request this pooled client carries is
            // credential-bearing — chat lanes attach `Bearer {api_key}` (see
            // `llm/chat_lanes/lane_calls.rs`), as do embed and rerank. This is
            // the same class of request the auth probe already refuses to send
            // through a proxy at `llm/auth_probe.rs`: reqwest's `system-proxy`
            // feature (`Cargo.toml`) otherwise lets HTTP_PROXY, HTTPS_PROXY,
            // ALL_PROXY or OS proxy configuration receive the request and own
            // DNS/TCP. The two builders in this crate now agree: documented
            // provider endpoints are always contacted directly.
            //
            // This is also what makes this crate's own test binary sound.
            // reqwest 0.13 has no implicit loopback bypass, so a
            // `http://127.0.0.1:PORT` mock request is genuinely handed to the
            // proxy, and reqwest snapshots proxy configuration at `.build()`.
            // `tests/auth_probe.rs`'s
            // `ambient_proxy_cannot_receive_probe_bearer_or_own_transport`
            // sets those vars process-globally under `EnvRestore` while ~20
            // `chat_lanes` `#[tokio::test]`s — which do NOT take
            // `global_test_lock` — are building clients, which produced the
            // "502 Bad Gateway then connection errors" flake under
            // `cargo test` (libtest shares one process; nextest's
            // process-per-test hid it).
            .no_proxy()
            .connect_timeout(Duration::from_secs(Self::RECALL_CONNECT_TIMEOUT_SECS))
            .timeout(Duration::from_secs(60))
            .build()
            .map_err(|e| format!("Failed to build HTTP client: {e}"))
    }

    /// Swap the pooled client so tests can pin a documented provider host to a
    /// loopback mock without sending credentials to the real network.
    #[cfg(any(test, feature = "test-support"))]
    #[doc(hidden)]
    pub fn replace_http_client_for_tests(&self, client: reqwest::Client) {
        *self.http.write().unwrap_or_else(|e| e.into_inner()) = client;
    }

    /// Same pooled-client shape as production, plus a single DNS override.
    #[cfg(any(test, feature = "test-support"))]
    #[doc(hidden)]
    pub fn http_client_with_host_resolved_for_tests(
        host: &str,
        addr: std::net::SocketAddr,
    ) -> Result<reqwest::Client, String> {
        crate::install_tls_provider();
        reqwest::Client::builder()
            .no_proxy()
            .resolve(host, addr)
            .connect_timeout(Duration::from_secs(Self::RECALL_CONNECT_TIMEOUT_SECS))
            .timeout(Duration::from_secs(60))
            .build()
            .map_err(|e| format!("Failed to build test HTTP client: {e}"))
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
        Self::new_with_vault_db_and_migration_authority(
            vault_db_path,
            memcore::MigrationAuthority::Deny,
        )
    }

    pub fn new_with_vault_db_and_migration_authority(
        vault_db_path: Option<&Path>,
        vault_db_migration: memcore::MigrationAuthority,
    ) -> Result<Self, String> {
        let (config, primary_authority) = ProviderRuntimeConfig::from_env_with_authority()?;
        let (fallbacks, fallback_authority) = LaneFallbackConfig::from_env_with_authority();
        Self::new_with_config_fallbacks_and_migration_authority(
            config,
            fallbacks,
            vault_db_path,
            vault_db_migration,
            true,
            LaneAuthoritySet {
                primary: primary_authority,
                fallback: fallback_authority,
            },
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
        let authorities = LaneAuthoritySet::for_injected_config(&fallbacks);
        Self::new_with_config_fallbacks_and_migration_authority(
            config,
            fallbacks,
            vault_db_path,
            memcore::MigrationAuthority::Deny,
            false,
            authorities,
        )
    }

    fn new_with_config_fallbacks_and_migration_authority(
        config: ProviderRuntimeConfig,
        fallbacks: LaneFallbackConfig,
        vault_db_path: Option<&Path>,
        vault_db_migration: memcore::MigrationAuthority,
        rebind_selected_provider: bool,
        authorities: LaneAuthoritySet,
    ) -> Result<Self, String> {
        let vault_db_path = vault_db_path.map(|path| path.to_path_buf());

        // Fail-closed rerank validation: injected `Local` without a non-empty
        // endpoint must error here, not defer to a runtime belt-and-suspenders
        // check. Shares the same `validate()` as the env path (#1096 R2).
        config.rerank.validate()?;

        // Every configured chat endpoint eventually carries an Authorization
        // header. Refuse credential-bearing URLs at the common fallible
        // constructor, before an HTTP client exists, so env-derived and
        // directly injected primary/fallback configs share memcore's single
        // canonical rule.
        for (lane, endpoint) in [
            ("extract", config.extract.base_url.as_str()),
            ("summary", config.summary.base_url.as_str()),
            ("reasoning", config.reasoning.base_url.as_str()),
            ("distill", config.distill.base_url.as_str()),
        ] {
            if let Some(leak) = memcore::catalog::endpoint::endpoint_credential_leak(endpoint) {
                return Err(format!("chat lane '{lane}' endpoint refused: {leak}"));
            }
        }
        for (lane, lane_config) in [
            ("extract fallback", fallbacks.extract.as_ref()),
            ("summary fallback", fallbacks.summary.as_ref()),
            ("reasoning fallback", fallbacks.reasoning.as_ref()),
            ("distill fallback", fallbacks.distill.as_ref()),
        ] {
            if let Some(leak) = lane_config.and_then(|lane_config| {
                memcore::catalog::endpoint::endpoint_credential_leak(&lane_config.base_url)
            }) {
                return Err(format!("chat lane '{lane}' endpoint refused: {leak}"));
            }
        }

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
            vault_db_migration,
            provider_state: Arc::new(RwLock::new(ProviderState::with_health(provider_health))),
            provider_materialization_lock: Arc::new(std::sync::Mutex::new(())),
            provider_health_reload: Arc::new(RwLock::new(provider_health_reload)),
            provider_health_persist: Arc::new(RwLock::new(ProviderHealthPersistState::default())),
            background_persist_lock: Arc::new(tokio::sync::Mutex::new(())),
            llm_usage_persist: Arc::new(RwLock::new(ProviderHealthPersistState::default())),
            deployment_health: Arc::new(DeploymentHealthCounters::default()),
            claude_cli_failure: Arc::new(RwLock::new(None)),
            circuit_breakers: super::super::CircuitBreakerRegistry::new(),
            lane_outage: super::super::LaneOutageTracker::new(),
            rebind_selected_provider,
            lane_authority: authorities.primary,
            fallback_authority: authorities.fallback,
            #[cfg(test)]
            last_rerank_dispatch: Arc::new(std::sync::Mutex::new(None)),
        })
    }

    fn load_lane(
        lane: ChatLane,
        api_key_envs: &[&'static str],
        base_url_envs: &[&str],
        model_envs: &[&str],
        tier_env: &str,
        default_model: &str,
        provider_defaults: &[&ProviderLaneDefault],
    ) -> Result<(ChatLaneConfig, LaneAuthority), String> {
        let selected_api_key = Self::first_env_key(api_key_envs);
        let selected_descriptor = selected_api_key.and_then(provider_descriptor_for_logical_key);
        let selected_provider_default = selected_descriptor.and_then(|descriptor| {
            provider_defaults
                .iter()
                .copied()
                .find(|default| default.descriptor.host == descriptor.host)
        });
        let explicit_base_url = base_url_envs.first().and_then(|key| Self::env_value(key));
        let explicit_model = model_envs.first().and_then(|key| Self::env_value(key));
        let base_url = if let Some(base_url) = explicit_base_url.as_ref() {
            base_url.clone()
        } else if let Some(descriptor) = selected_descriptor {
            let provider_base_url_envs = selected_provider_default
                .map(|default| default.base_url_envs)
                .unwrap_or(&[]);
            if selected_provider_default.is_some() {
                Self::first_compatible_provider_env(provider_base_url_envs, descriptor)
                    .unwrap_or_else(|| descriptor.chat.base_url.to_string())
            } else {
                Self::first_compatible_provider_env(base_url_envs, descriptor)
                    .unwrap_or_else(|| descriptor.chat.base_url.to_string())
            }
        } else {
            Self::first_non_deepseek_env(base_url_envs)
                .unwrap_or_else(|| DEFAULT_CHAT_BASE_URL.to_string())
        };
        let provider_model =
            selected_provider_default.and_then(|default| Self::first_env(default.model_envs));
        let model_source = explicit_model.clone().or(provider_model);
        let fallback = if explicit_model.is_none()
            && selected_provider_default.is_none()
            && model_envs.len() > 1
        {
            Self::first_non_deepseek_env(&model_envs[1..])
        } else {
            None
        };
        let default_model = selected_descriptor
            .map(|descriptor| descriptor.chat_model_for_lane(lane))
            .unwrap_or(default_model);
        let model = crate::backend_tier::resolve_lane_model(
            lane.as_str(),
            tier_env,
            model_source,
            fallback,
            default_model,
        );

        let authority =
            LaneAuthority::from_fields(explicit_base_url.is_some(), explicit_model.is_some());

        let config = ChatLaneConfig {
            base_url,
            model,
            api_key_envs: api_key_envs.to_vec(),
        };
        match selected_api_key {
            Some(logical_name) => {
                bind_lane_config_to_selected_key(lane, &config, authority, logical_name, true)
                    .map(|bound| (bound, authority))
            }
            None => Ok((config, authority)),
        }
    }

    fn first_compatible_provider_env(
        keys: &[&str],
        selected: &ProviderProbeDescriptor,
    ) -> Option<String> {
        keys.iter().find_map(|key| {
            let value = Self::env_value(key)?;
            let url = reqwest::Url::parse(&value).ok()?;
            url.host_str()?;
            match provider_descriptor_for_base_url(&value) {
                Some(candidate) if candidate.host != selected.host => None,
                _ => Some(value),
            }
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

    /// A provider key and its endpoint/model must come from the same provider
    /// family. DeepSeek variables are valid for a DeepSeek-selected lane only;
    /// otherwise a stale `DEEPSEEK_*` value could outrank the selected
    /// provider's URL/model while its key remains in the lane's key chain.
    fn first_non_deepseek_env(keys: &[&str]) -> Option<String> {
        keys.iter()
            .filter(|key| !key.starts_with("DEEPSEEK_"))
            .find_map(|key| Self::env_value(key))
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

    pub(in crate::llm) fn lane_authority(&self, lane: ChatLane) -> LaneAuthority {
        self.lane_authority[lane.index()]
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

    pub(in crate::llm) fn fallback_lane_with_authority(
        &self,
        lane: ChatLane,
    ) -> Option<(ChatLaneConfig, LaneAuthority)> {
        let config = match lane {
            ChatLane::Extract => self.extract_fallback.clone(),
            ChatLane::Distill => self.distill_fallback.clone(),
            ChatLane::Reasoning => self.reasoning_fallback.clone(),
            ChatLane::Summary => self.summary_fallback.clone(),
        }?;
        Some((config, self.fallback_authority[lane.index()]?))
    }
}

/// Bind a lane's endpoint and default model to the logical provider key that
/// was actually selected from env or Vault.
///
/// Env resolution happens before Vault materialization, so the construction
/// lane may still carry SiliconFlow defaults when a later selected key belongs
/// to DeepSeek, Z.AI, or BigModel. Known provider hosts are a closed set: a
/// mismatched known host is rebound only when the construction-time endpoint
/// was a non-explicit default; explicit endpoints are refused. A known
/// provider-branded credential is also refused on an unknown host. The four
/// lane-scoped aliases remain valid for caller-owned custom endpoints; their
/// descriptor only supplies a default when no endpoint was configured.
pub(crate) fn bind_lane_config_to_selected_key(
    lane: ChatLane,
    cfg: &ChatLaneConfig,
    authority: LaneAuthority,
    logical_name: &str,
    allow_rebind: bool,
) -> Result<ChatLaneConfig, String> {
    let Some(selected) = provider_descriptor_for_logical_key(logical_name) else {
        return Ok(cfg.clone());
    };

    let current = reqwest::Url::parse(&cfg.base_url).map_err(|_| {
        format!(
            "lane '{}' has a malformed endpoint; refusing credential-bearing request",
            lane.as_str()
        )
    })?;
    if current.host_str().is_none() {
        return Err(format!(
            "lane '{}' endpoint has no host; refusing credential-bearing request",
            lane.as_str()
        ));
    }

    let Some(current_provider) = provider_descriptor_for_base_url(&cfg.base_url) else {
        if matches!(
            logical_name,
            "EXTRACT_API_KEY" | "SUMMARY_API_KEY" | "DISTILL_API_KEY" | "REASONING_API_KEY"
        ) {
            return Ok(cfg.clone());
        }
        return Err(format!(
            "logical provider key '{}' targets known provider '{}', but lane '{}' endpoint is not a recognized provider host; refusing credential-bearing request",
            logical_name,
            selected.host,
            lane.as_str(),
        ));
    };
    if current_provider.host == selected.host {
        return Ok(cfg.clone());
    }
    if authority.has_explicit_base_url() || !allow_rebind {
        return Err(format!(
            "logical provider key '{}' targets known provider '{}', but lane '{}' is configured for known provider '{}'; refusing credential-bearing request",
            logical_name,
            selected.host,
            lane.as_str(),
            current_provider.host,
        ));
    }

    Ok(ChatLaneConfig {
        base_url: selected.chat.base_url.to_string(),
        model: if authority.has_explicit_model() {
            cfg.model.clone()
        } else {
            selected.chat_model_for_lane(lane).to_string()
        },
        api_key_envs: cfg.api_key_envs.clone(),
    })
}
