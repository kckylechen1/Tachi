use super::*;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChatLaneConfig {
    pub base_url: String,
    pub model: String,
    pub api_key_envs: Vec<&'static str>,
}

#[derive(Clone)]
pub struct ProviderSecret {
    pub key_id: String,
    pub value: String,
}

/// Provider families with an owner-verified, non-generating authentication
/// probe. Z.AI/BigModel is named explicitly so callers can distinguish the
/// documented absence of a safe probe from malformed configuration.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ProviderAuthProbeFamily {
    DeepSeek,
    SiliconFlow,
    ZaiBigModel,
    Unsupported,
}

/// Public-safe outcome classes for the one-request, no-content auth probe.
/// No variant carries an error message or provider response body.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ProviderAuthProbeClass {
    AuthOk,
    AuthFailed,
    ProviderExhausted,
    RateLimited,
    Transient,
    RedirectRefused,
    MalformedResponse,
    MalformedConfiguration,
    UnexpectedStatus,
    CredentialUnavailable,
    UnsupportedNoDocumentedProbe,
}

impl ProviderAuthProbeClass {
    pub fn is_auth_ok(self) -> bool {
        self == Self::AuthOk
    }
}

/// Safe, observation-only receipt for a no-content provider auth probe. The
/// selected model is configuration, while provider model IDs and all response
/// bodies remain deliberately absent.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ProviderAuthProbeResult {
    pub provider_family: ProviderAuthProbeFamily,
    pub provider_host: String,
    pub effective_model: String,
    pub auth_class: ProviderAuthProbeClass,
    pub selected_model_present: Option<bool>,
    pub model_count: Option<usize>,
    pub latency_ms: u64,
}

impl ProviderAuthProbeResult {
    pub fn clears_configured_model(&self) -> bool {
        self.auth_class.is_auth_ok() && self.selected_model_present == Some(true)
    }
}

/// Internal receipt for one successful provider HTTP completion. It names the
/// endpoint and provider-reported identity, but deliberately never carries a
/// secret value, request body, or response body. Persisted provenance must use
/// [`PersistedModelInvocationReceiptV1`] instead: that type has a deliberately
/// closed serialization allowlist.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderInvocationReceipt {
    pub effective_provider: String,
    pub effective_model: Option<String>,
    pub effective_version: Option<String>,
    pub fallback_chain: Vec<String>,
    pub degraded: bool,
    pub prompt_tokens: Option<i64>,
    pub completion_tokens: Option<i64>,
    pub total_tokens: Option<i64>,
    pub latency_ms: u128,
}

/// Stable schema tag for model-execution provenance written by downstream
/// artifact producers.
pub const MODEL_INVOCATION_SCHEMA_V1: &str = "model-invocation-v1";

/// Typed failure returned when a provider marks a completion as truncated.
///
/// The receipt still identifies that invocation, but callers must not parse or
/// persist its output as a clean model-derived artifact.
pub const LLM_OUTPUT_TRUNCATED: &str = "llm_output_truncated";

/// Chat lane recorded in durable model-invocation provenance.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ModelInvocationLaneV1 {
    Extract,
    Distill,
    Reasoning,
    Summary,
}

/// Execution engine recorded in durable model-invocation provenance.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ModelEngineKindV1 {
    ProviderHttp,
    ClaudeCli,
}

/// Whether the serving engine exposed a trustworthy completion signal.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CompletionStatusV1 {
    Complete,
    Truncated,
    Unknown,
}

/// Public-safe, schema-versioned model invocation receipt for durable
/// provenance.
///
/// This is intentionally *not* a serialization derive on
/// [`ProviderInvocationReceipt`]. Its fields are an explicit allowlist: no
/// prompt/output content, request/response body, endpoint, credential, key
/// identifier, raw provider detail, or vault identity can enter persistence by
/// adding a field to an internal transport receipt.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PersistedModelInvocationReceiptV1 {
    pub schema: &'static str,
    pub lane: ModelInvocationLaneV1,
    pub engine_kind: ModelEngineKindV1,
    pub effective_provider: Option<String>,
    pub effective_model: Option<String>,
    pub effective_version: Option<String>,
    pub fallback_chain: Vec<String>,
    pub degraded: bool,
    pub completion_status: CompletionStatusV1,
    pub prompt_tokens: Option<i64>,
    pub completion_tokens: Option<i64>,
    pub total_tokens: Option<i64>,
    pub latency_ms: Option<u64>,
}

impl PersistedModelInvocationReceiptV1 {
    const MAX_FALLBACK_CHAIN_ENTRIES: usize = 4;

    /// Convert the internal provider receipt at the durable boundary. Existing
    /// fallback labels are intentionally collapsed to a fixed routing marker:
    /// producer/provider error text must never become durable provenance.
    pub fn from_provider_http(
        lane: ModelInvocationLaneV1,
        receipt: ProviderInvocationReceipt,
        truncated: bool,
    ) -> Self {
        let fallback_chain = receipt
            .fallback_chain
            .iter()
            .take(Self::MAX_FALLBACK_CHAIN_ENTRIES)
            .map(|_| "provider_http_fallback".to_string())
            .collect();
        Self {
            schema: MODEL_INVOCATION_SCHEMA_V1,
            lane,
            engine_kind: ModelEngineKindV1::ProviderHttp,
            effective_provider: non_empty(receipt.effective_provider),
            effective_model: receipt.effective_model.and_then(non_empty),
            effective_version: receipt.effective_version.and_then(non_empty),
            fallback_chain,
            degraded: receipt.degraded,
            completion_status: if truncated {
                CompletionStatusV1::Truncated
            } else {
                CompletionStatusV1::Complete
            },
            prompt_tokens: receipt.prompt_tokens,
            completion_tokens: receipt.completion_tokens,
            total_tokens: receipt.total_tokens,
            latency_ms: Some(receipt.latency_ms.min(u128::from(u64::MAX)) as u64),
        }
    }

    /// Claude CLI has no authoritative model, token, version, or completion
    /// fields in its text protocol. Preserve that uncertainty rather than
    /// projecting configured values into the durable receipt.
    pub fn claude_cli_reasoning(latency_ms: u128) -> Self {
        Self {
            schema: MODEL_INVOCATION_SCHEMA_V1,
            lane: ModelInvocationLaneV1::Reasoning,
            engine_kind: ModelEngineKindV1::ClaudeCli,
            effective_provider: Some("anthropic_cli".to_string()),
            effective_model: None,
            effective_version: None,
            fallback_chain: Vec::new(),
            degraded: false,
            completion_status: CompletionStatusV1::Unknown,
            prompt_tokens: None,
            completion_tokens: None,
            total_tokens: None,
            latency_ms: Some(latency_ms.min(u128::from(u64::MAX)) as u64),
        }
    }

    /// The CLI failure/skip detail stays transient. Durable provenance records
    /// only the fixed fact that provider HTTP served a CLI-first request.
    pub fn mark_claude_cli_to_provider_http_fallback(&mut self) {
        self.degraded = true;
        if self.fallback_chain.len() < Self::MAX_FALLBACK_CHAIN_ENTRIES {
            self.fallback_chain
                .push("claude_cli_to_provider_http".to_string());
        }
    }
}

fn non_empty(value: String) -> Option<String> {
    let trimmed = value.trim();
    (!trimmed.is_empty()).then(|| trimmed.to_string())
}

/// A parsed or text model result paired with the one persisted receipt for the
/// invocation that produced it. It is the receipt-preserving API boundary for
/// downstream producers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Generated<T> {
    pub value: T,
    pub invocation: PersistedModelInvocationReceiptV1,
}

/// Completion text plus its public-safe execution receipt. Consumers that
/// persist a report must deliberately retain only the receipt when the text is
/// not itself an approved output surface.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderInvocationOutcome {
    pub text: String,
    pub truncated: bool,
    pub receipt: ProviderInvocationReceipt,
}

impl ProviderInvocationOutcome {
    /// Convert an internal transport outcome into the explicitly allowlisted
    /// durable receipt shape. This consumes the raw transport receipt so a
    /// producer cannot accidentally serialize it instead.
    pub fn into_generated(self, lane: ModelInvocationLaneV1) -> Generated<String> {
        Generated {
            invocation: PersistedModelInvocationReceiptV1::from_provider_http(
                lane,
                self.receipt,
                self.truncated,
            ),
            value: self.text,
        }
    }
}

/// Public-safe failure classes for spend-aware provider calls. These values
/// deliberately contain no provider response, prompt, source text, endpoint,
/// key identifier, or credential material.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProviderInvocationFailureClass {
    AuthFailed,
    ProviderExhausted,
    Transient,
    LaneOutage,
}

impl ProviderInvocationFailureClass {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::AuthFailed => "auth_failed",
            Self::ProviderExhausted => "provider_exhausted",
            Self::Transient => "transient",
            Self::LaneOutage => "lane_outage",
        }
    }
}

/// Failure receipt for a bounded provider-only call. `provider_attempts`
/// counts HTTP requests actually started; a resolver, selection, or open
/// circuit failure therefore records zero rather than inventing spend.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProviderInvocationFailure {
    pub class: ProviderInvocationFailureClass,
    pub provider_attempts: usize,
    pub latency_ms: u128,
}

#[derive(Debug, Clone, Serialize)]
pub struct ProviderKeyCooldownStatus {
    pub key_id: String,
    pub remaining_seconds: u64,
}

#[derive(Debug, Clone, Serialize)]
pub struct ProviderPoolStatus {
    pub logical_name: String,
    pub total_keys: usize,
    pub available_keys: usize,
    pub rate_limited_keys: Vec<ProviderKeyCooldownStatus>,
    pub current_index: usize,
    pub strategy: &'static str,
}

#[derive(Debug, Clone, Serialize)]
pub struct ProviderHealthStatus {
    pub source_of_truth: &'static str,
    pub reload_ttl_secs: u64,
    pub last_attempt_at: Option<String>,
    pub last_success_at: Option<String>,
    pub last_success_age_secs: Option<u64>,
    pub last_error: Option<String>,
    pub persist_last_attempt_at: Option<String>,
    pub persist_last_success_at: Option<String>,
    pub persist_last_success_age_secs: Option<u64>,
    pub persist_last_error: Option<String>,
    /// Per-lane breaker/outage surface (#1197): a lane whose circuit breaker
    /// is open, or that has exhausted its full fallback chain recently,
    /// shows up here instead of only as a silent stall to background
    /// callers. Recorded unconditionally inside `LlmClient::call_lane_llm`
    /// before the `Err` ever reaches a caller — so this is populated
    /// regardless of whether a given background caller (e.g. the research
    /// digest / wiki ingest / session capture lanes in `tachi-server`, all
    /// intentionally-degrading paths that fall back to a deterministic
    /// result rather than fail their parent operation) propagates or
    /// swallows that `Err` for its own control flow.
    ///
    /// TODO(#1197 codex-review BUG-2, deferred — crosses into
    /// `tachi-server`, out of this packet's tachi-llm-only edit boundary):
    /// the default (non-verbose, no-error) `tachi_status` view slims this
    /// away — see `crates/tachi-server/src/status_ops/runtime.rs`'s
    /// `slim_provider_health_value`, which replaces the whole
    /// provider_health value with `{status, last_success_age_secs,
    /// source_of_truth}` whenever `last_error`/`persist_last_error` are both
    /// null, dropping `lane_outages` even when it's nonempty. That function
    /// needs to also check `lane_outages` for any nonzero
    /// `consecutive_chain_failures` before slimming, and threshold-crossing
    /// → active alert-event routing (the issue's ask #2) still needs a home
    /// in that same status/alert surface.
    pub lane_outages: Vec<LaneOutageStatus>,
}

/// Snapshot of one chat lane's resilience state: breaker position, whether a
/// fallback provider is configured for it, and the running full-chain-outage
/// streak (#1197). `consecutive_chain_failures` only increments when *every*
/// configured tier (primary + fallback) failed on a call — an ordinary
/// within-tier retry or a fallback-served success never counts against it.
#[derive(Debug, Clone, Serialize)]
pub struct LaneOutageStatus {
    pub lane: String,
    pub breaker_state: &'static str,
    pub fallback_configured: bool,
    pub consecutive_chain_failures: u32,
    pub last_outage_at: Option<String>,
    pub last_error: Option<String>,
}

#[derive(Clone)]
pub(in crate::llm) struct SelectedProviderSecret {
    pub(in crate::llm) logical_name: String,
    pub(in crate::llm) key_id: String,
    pub(in crate::llm) value: String,
}

#[derive(Clone, Copy)]
pub(in crate::llm) enum ChatLane {
    Extract,
    Distill,
    Reasoning,
    Summary,
}

impl ChatLane {
    pub(in crate::llm) fn as_str(self) -> &'static str {
        match self {
            Self::Extract => "extract",
            Self::Distill => "distill",
            Self::Reasoning => "reasoning",
            Self::Summary => "summary",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::llm) enum KeyAvailability {
    Available,
    Cooldown,
    AuthFailed,
    Disabled,
    Exhausted,
}

impl KeyAvailability {
    pub(in crate::llm) fn label(self) -> &'static str {
        match self {
            Self::Available => "available",
            Self::Cooldown => "cooldown",
            Self::AuthFailed => "auth_failed",
            Self::Disabled => "disabled",
            Self::Exhausted => "exhausted",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn persisted_model_invocation_receipts_are_schema_stable_and_secret_negative() {
        let provider = PersistedModelInvocationReceiptV1::from_provider_http(
            ModelInvocationLaneV1::Extract,
            ProviderInvocationReceipt {
                effective_provider: "provider.fixture.test".to_string(),
                effective_model: Some("served-model-v1".to_string()),
                effective_version: Some("served-version-v1".to_string()),
                fallback_chain: vec![
                    "raw provider error Bearer fixture-secret-value".to_string(),
                    "https://credential.example/v1/chat/completions".to_string(),
                    "api-key-id=fixture-key-id".to_string(),
                    "response body fixture-response-body".to_string(),
                    "must be bounded away".to_string(),
                ],
                degraded: true,
                prompt_tokens: Some(7),
                completion_tokens: Some(3),
                total_tokens: Some(10),
                latency_ms: u128::from(u64::MAX) + 1,
            },
            false,
        );
        let cli = PersistedModelInvocationReceiptV1::claude_cli_reasoning(41);
        let mut cli_to_http = provider.clone();
        cli_to_http.mark_claude_cli_to_provider_http_fallback();

        let expected_keys = [
            "schema",
            "lane",
            "engine_kind",
            "effective_provider",
            "effective_model",
            "effective_version",
            "fallback_chain",
            "degraded",
            "completion_status",
            "prompt_tokens",
            "completion_tokens",
            "total_tokens",
            "latency_ms",
        ];
        for receipt in [provider.clone(), cli.clone(), cli_to_http] {
            let value = serde_json::to_value(&receipt).expect("receipt must serialize");
            let object = value.as_object().expect("receipt serializes as object");
            assert_eq!(
                object.len(),
                expected_keys.len(),
                "closed allowlist changed"
            );
            for key in expected_keys {
                assert!(object.contains_key(key), "missing allowlisted key {key}");
            }
            let serialized = serde_json::to_string(&receipt).expect("receipt JSON");
            for forbidden in [
                "fixture-secret-value",
                "fixture-key-id",
                "fixture-response-body",
                "https://credential.example",
                "raw provider error",
                "api-key-id",
                "prompt.md",
                "Authorization",
            ] {
                assert!(
                    !serialized.contains(forbidden),
                    "persisted receipt leaked forbidden marker {forbidden}: {serialized}"
                );
            }
            assert_eq!(receipt.schema, MODEL_INVOCATION_SCHEMA_V1);
        }

        assert_eq!(
            provider.fallback_chain,
            vec![
                "provider_http_fallback".to_string(),
                "provider_http_fallback".to_string(),
                "provider_http_fallback".to_string(),
                "provider_http_fallback".to_string(),
            ],
            "raw fallback detail must collapse to bounded predefined labels"
        );
        assert_eq!(provider.latency_ms, Some(u64::MAX));
        assert_eq!(cli.engine_kind, ModelEngineKindV1::ClaudeCli);
        assert_eq!(cli.completion_status, CompletionStatusV1::Unknown);
        assert!(cli.effective_model.is_none());
        assert!(cli.completion_tokens.is_none());
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::llm) enum KeyRetryStatus {
    Available,
    RetryAfter(Duration),
    Unavailable,
}

#[derive(Debug, Clone)]
pub(in crate::llm) struct ClaudeCliFailure {
    pub(in crate::llm) kind: ClaudeCliFailureKind,
    pub(in crate::llm) failed_at: Instant,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::llm) enum ClaudeCliFailureKind {
    SpawnFailed,
    Timeout,
}

impl ClaudeCliFailureKind {
    pub(in crate::llm) fn from_error(error: &str) -> Option<Self> {
        let error = error.trim_start();
        if error.starts_with("claude cli spawn failed:") {
            Some(Self::SpawnFailed)
        } else if error.starts_with("claude cli timeout after ") {
            Some(Self::Timeout)
        } else {
            None
        }
    }

    pub(in crate::llm) fn as_str(self) -> &'static str {
        match self {
            Self::SpawnFailed => "spawn_failed",
            Self::Timeout => "timeout",
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub(in crate::llm) struct ClaudeCliSkip {
    pub(in crate::llm) kind: ClaudeCliFailureKind,
    pub(in crate::llm) remaining: Duration,
}
