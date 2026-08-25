use memcore::store::llm_usage::LlmUsageEvent;
use reqwest::{
    header::{AUTHORIZATION, CONTENT_TYPE},
    Url,
};
use serde_json::{self, Value};
use std::sync::Arc;
use std::time::{Duration, Instant};

use super::super::catalog_import::DeploymentAttribution;
use super::super::provider_health::{
    ChatLane, ChatLaneConfig, CompletionStatusV1, Generated, ModelInvocationLaneV1,
    ProviderInvocationFailure, ProviderInvocationFailureClass, ProviderInvocationOutcome,
    ProviderInvocationReceipt, SelectedProviderSecret,
};

/// Maximum retained characters from a caller-supplied model override.
const MAX_REFERENCE_CHARS: usize = 64;
const LLM_USAGE_PERSIST_SQLITE_BUSY_TIMEOUT: Duration = Duration::from_secs(2);

/// Bound and scrub a caller-supplied model override before it reaches a
/// durable usage row or retry log. Routing still uses the original string.
fn bounded_reference(raw: &str) -> String {
    let mut bounded: String = raw
        .chars()
        .take(MAX_REFERENCE_CHARS)
        .map(|character| {
            if character.is_control() {
                '\u{fffd}'
            } else {
                character
            }
        })
        .collect();
    if raw.chars().nth(MAX_REFERENCE_CHARS).is_some() {
        bounded.push('…');
    }
    bounded
}

#[derive(Clone, Copy)]
struct ChatUsageTokens {
    prompt_tokens: Option<i64>,
    completion_tokens: Option<i64>,
    total_tokens: Option<i64>,
}

struct ProviderTierFailure {
    class: ProviderInvocationFailureClass,
    provider_attempts: usize,
    latency_ms: u128,
    safe_detail: String,
}

impl ProviderTierFailure {
    fn public_receipt(&self) -> ProviderInvocationFailure {
        ProviderInvocationFailure {
            class: self.class,
            provider_attempts: self.provider_attempts,
            latency_ms: self.latency_ms,
        }
    }
}

impl super::super::LlmClient {
    pub(super) fn should_disable_thinking(base_url: &str, model: &str) -> bool {
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
        self.call_extract_llm_with_receipt(system, user, model, temperature, max_tokens)
            .await
            .map(|generated| generated.value)
    }

    /// Extract-lane completion paired with its explicit, persisted-safe model
    /// invocation receipt. The text-only sibling above remains a compatibility
    /// adapter for callers that do not own durable provenance.
    pub async fn call_extract_llm_with_receipt(
        &self,
        system: &str,
        user: &str,
        model: Option<&str>,
        temperature: f32,
        max_tokens: u32,
    ) -> Result<Generated<String>, String> {
        self.call_lane_llm(
            ChatLane::Extract,
            system,
            user,
            model,
            temperature,
            max_tokens,
        )
        .await
        .map(|outcome| outcome.into_generated(ModelInvocationLaneV1::Extract))
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
        self.call_distill_llm_with_receipt(system, user, model, temperature, max_tokens)
            .await
            .map(|generated| generated.value)
    }

    /// Distill-lane completion paired with its persisted-safe invocation
    /// receipt. This does not change lane/model selection.
    pub async fn call_distill_llm_with_receipt(
        &self,
        system: &str,
        user: &str,
        model: Option<&str>,
        temperature: f32,
        max_tokens: u32,
    ) -> Result<Generated<String>, String> {
        self.call_lane_llm(
            ChatLane::Distill,
            system,
            user,
            model,
            temperature,
            max_tokens,
        )
        .await
        .map(|outcome| outcome.into_generated(ModelInvocationLaneV1::Distill))
    }

    /// Reasoning-lane HTTP call with **no** Claude-CLI-first behavior — a
    /// pure provider round-trip. `call_reasoning_llm`/
    /// `call_reasoning_llm_with_receipt` try a raw `claude` CLI subprocess
    /// before falling back to this same lane (see `chat_lanes/claude_cli.rs`);
    /// that is exactly the implicit Claude-CLI dependency #1087 retires, so
    /// a provider-rollout call site (e.g. the hub security scan's strong-tier
    /// two-vote check) must go through this method instead of
    /// `call_reasoning_llm`, or it would silently reintroduce a CLI
    /// subprocess dependency into a path meant to be CLI-free.
    pub async fn call_reasoning_llm_provider_only(
        &self,
        system: &str,
        user: &str,
        model: Option<&str>,
        temperature: f32,
        max_tokens: u32,
    ) -> Result<String, String> {
        self.call_lane_llm(
            ChatLane::Reasoning,
            system,
            user,
            model,
            temperature,
            max_tokens,
        )
        .await
        .map(|outcome| outcome.text)
    }

    /// Spend-aware provider-only reasoning call with a public-safe receipt.
    /// It makes at most one HTTP request against the primary reasoning tier:
    /// no key-pool retry, same-case retry, fallback provider, or Claude CLI.
    /// Failures retain only a typed class, request count, and latency.
    pub async fn call_reasoning_llm_provider_only_with_receipt(
        &self,
        system: &str,
        user: &str,
        model: Option<&str>,
        temperature: f32,
        max_tokens: u32,
    ) -> Result<ProviderInvocationOutcome, ProviderInvocationFailure> {
        let lane = ChatLane::Reasoning;
        let cfg = self.lane(lane).clone();

        let breaker_key = format!("chat:{}", lane.as_str());
        if !self.circuit_breakers.allow(&breaker_key) {
            return Err(ProviderInvocationFailure {
                class: ProviderInvocationFailureClass::LaneOutage,
                provider_attempts: 0,
                latency_ms: 0,
            });
        }
        match self
            .call_provider_tier(
                lane,
                &cfg,
                &breaker_key,
                1,
                system,
                user,
                model,
                temperature,
                max_tokens,
            )
            .await
        {
            Ok(result) => {
                self.lane_outage.record_chain_success(lane.as_str());
                Ok(result)
            }
            Err(failure) => {
                let now_utc =
                    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true);
                self.lane_outage.record_chain_exhausted(
                    lane.as_str(),
                    now_utc,
                    format!("class={}", failure.class.as_str()),
                );
                Err(failure.public_receipt())
            }
        }
    }

    pub async fn call_summary_llm(
        &self,
        system: &str,
        user: &str,
        model: Option<&str>,
        temperature: f32,
        max_tokens: u32,
    ) -> Result<String, String> {
        self.call_summary_llm_with_receipt(system, user, model, temperature, max_tokens)
            .await
            .map(|generated| generated.value)
    }

    /// Summary-lane completion paired with its persisted-safe invocation
    /// receipt. Text/error compatibility remains in `call_summary_llm`.
    pub async fn call_summary_llm_with_receipt(
        &self,
        system: &str,
        user: &str,
        model: Option<&str>,
        temperature: f32,
        max_tokens: u32,
    ) -> Result<Generated<String>, String> {
        self.call_lane_llm(
            ChatLane::Summary,
            system,
            user,
            model,
            temperature,
            max_tokens,
        )
        .await
        .map(|outcome| outcome.into_generated(ModelInvocationLaneV1::Summary))
    }

    /// Returns `(text, truncated)` — `truncated` is `true` when the
    /// provider's `finish_reason` is `"length"` on an otherwise-successful,
    /// non-empty response (#1071 fix-round checkpoint 6: truncated synthesis
    /// must never be silently reported as a clean `completed` answer).
    ///
    /// #1197: walks a per-lane provider chain (primary, then a configured
    /// cross-provider fallback if any) instead of stopping at the primary's
    /// exhaustion. Each tier gets its own circuit breaker key, so a lane
    /// whose primary provider is degraded (breaker open, or its key pool
    /// exhausted/auth-failed within the shared `MAX_ATTEMPTS` retry budget —
    /// retries up to `MAX_ATTEMPTS` distinct keys, then fails over; this
    /// does not exhaust an arbitrarily large pool before failover)
    /// fast-escalates to the fallback instead of silently stalling every
    /// background caller. Only when *every* configured tier fails do we
    /// return the loud, typed "lane outage" error and bump the lane-outage
    /// counter (`provider_health_status().lane_outages`) — this must never
    /// be a silent hang.
    pub(in crate::llm::chat_lanes) async fn call_lane_llm(
        &self,
        lane: ChatLane,
        system: &str,
        user: &str,
        model_override: Option<&str>,
        temperature: f32,
        max_tokens: u32,
    ) -> Result<ProviderInvocationOutcome, String> {
        let primary_cfg = self.lane(lane).clone();
        let primary_breaker_key = format!("chat:{}", lane.as_str());

        let mut tiers: Vec<(ChatLaneConfig, String)> =
            vec![(primary_cfg.clone(), primary_breaker_key)];
        if let Some(fallback_cfg) = self.fallback_lane(lane) {
            // A fallback that resolves to the exact same provider config as
            // primary (e.g. no `*_FALLBACK_*` env configured and the
            // convenience default happens to match) carries no resilience
            // value — skip it rather than retrying the same dead endpoint
            // twice under a different breaker key.
            if fallback_cfg != primary_cfg {
                let fallback_breaker_key = format!("chat:{}:fallback", lane.as_str());
                tiers.push((fallback_cfg, fallback_breaker_key));
            }
        }
        let tier_count = tiers.len();

        let mut last_err = String::new();
        for (tier_index, (cfg, breaker_key)) in tiers.into_iter().enumerate() {
            if !self.circuit_breakers.allow(&breaker_key) {
                last_err = format!(
                    "Circuit breaker open for {} (lane {}, tier {tier_index}/{tier_count}) — provider is failing, fast-rejecting. Retry in ~30s.",
                    breaker_key,
                    lane.as_str()
                );
                continue;
            }

            match self
                .call_provider_tier(
                    lane,
                    &cfg,
                    &breaker_key,
                    Self::MAX_ATTEMPTS,
                    system,
                    user,
                    model_override,
                    temperature,
                    max_tokens,
                )
                .await
            {
                Ok(mut result) => {
                    self.lane_outage.record_chain_success(lane.as_str());
                    if tier_index > 0 {
                        result.receipt.degraded = true;
                        result.receipt.fallback_chain.push(format!(
                            "{} lane used provider fallback tier {tier_index}",
                            lane.as_str()
                        ));
                    }
                    return Ok(result);
                }
                Err(e) => last_err = e.safe_detail,
            }
        }

        // Every configured tier (primary + fallback, if any) failed. This is
        // a full lane outage, not an ordinary within-tier retry — fail loud
        // (拒必有声) and record it on the queryable outage surface instead of
        // letting background callers see nothing but a swallowed Err.
        let now_utc = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true);
        self.lane_outage
            .record_chain_exhausted(lane.as_str(), now_utc, last_err.clone());
        let outage_msg = format!(
            "LANE OUTAGE [{}]: all {tier_count} configured provider tier(s) exhausted — {last_err}",
            lane.as_str()
        );
        tracing::error!("[llm] {outage_msg}");
        Err(outage_msg)
    }

    /// One provider tier's full attempt loop (retries within `Self::MAX_ATTEMPTS`,
    /// key-pool rotation, breaker bookkeeping). Factored out of `call_lane_llm`
    /// so the same logic runs against either the primary or a fallback
    /// `ChatLaneConfig` (#1197) — behavior is byte-for-byte identical to the
    /// pre-#1197 single-tier loop when there is no fallback tier.
    #[allow(clippy::too_many_arguments)]
    async fn call_provider_tier(
        &self,
        lane: ChatLane,
        cfg: &ChatLaneConfig,
        breaker_key: &str,
        max_attempts: usize,
        system: &str,
        user: &str,
        model_override: Option<&str>,
        temperature: f32,
        max_tokens: u32,
    ) -> Result<ProviderInvocationOutcome, ProviderTierFailure> {
        debug_assert!(max_attempts > 0);
        let tier_started = Instant::now();
        let model = model_override.unwrap_or(&cfg.model);
        // Which catalog deployment this tier's requests are attributable to
        // (#1681 D4, PR-C). Identity only — the store checks the endpoint and
        // model against the stored row, so a #1197 fallback tier or a
        // `model_override` lands as a counted skip rather than as health for a
        // deployment that never served this request.
        let attribution = DeploymentAttribution::EnvLane {
            lane: lane.as_str(),
            endpoint: &cfg.base_url,
            model,
        };

        let mut body = serde_json::json!({
            "model": model,
            "messages": [
                {"role": "system", "content": system},
                {"role": "user", "content": user}
            ],
            "temperature": temperature,
            "max_tokens": max_tokens
        });
        if Self::should_disable_thinking(&cfg.base_url, model) {
            body["enable_thinking"] = Value::Bool(false);
        }

        let mut last_err = String::new();
        let mut last_class = ProviderInvocationFailureClass::LaneOutage;
        let mut provider_attempts = 0;

        for attempt in 1..=max_attempts {
            let Some(selected) = self
                .required_selected_secret_or_wait(&cfg.api_key_envs, attempt, "chat lane")
                .await
                .map_err(|safe_detail| ProviderTierFailure {
                    class: ProviderInvocationFailureClass::LaneOutage,
                    provider_attempts,
                    latency_ms: tier_started.elapsed().as_millis(),
                    safe_detail,
                })?
            else {
                continue;
            };
            let attempt_started = Instant::now();
            provider_attempts += 1;
            let resp = self
                .http_client()
                .post(&cfg.base_url)
                .header(CONTENT_TYPE, "application/json")
                .header(AUTHORIZATION, format!("Bearer {}", selected.value))
                .json(&body)
                .send()
                .await;

            let resp = match resp {
                Ok(r) => r,
                Err(e) => {
                    // Deployment-only evidence (#1681 D4): no status line ever
                    // arrived, so there is no credential fact here at all. One
                    // record per attempt, because each attempt is its own
                    // observation of the deployment.
                    self.note_deployment_transport_failure(attribution);
                    last_err = format!("HTTP request failed: {e}");
                    last_class = ProviderInvocationFailureClass::Transient;
                    if attempt < max_attempts
                        && (e.is_timeout() || e.is_connect() || e.is_request())
                    {
                        eprintln!(
                            "[llm] transient error (attempt {}/{}): {e}; retrying",
                            attempt, max_attempts
                        );
                        tokio::time::sleep(Self::retry_delay(attempt)).await;
                        continue;
                    }
                    self.circuit_breakers.record_failure(breaker_key);
                    return Err(ProviderTierFailure {
                        class: last_class,
                        provider_attempts,
                        latency_ms: tier_started.elapsed().as_millis(),
                        safe_detail: last_err,
                    });
                }
            };

            let status = resp.status();
            let retry_after = resp
                .headers()
                .get("retry-after")
                .and_then(|v| v.to_str().ok())
                .and_then(|v| v.parse::<u64>().ok());
            // The same header, unparsed, for the deployment authority alone
            // (#1681 D4). Read beside `retry_after` rather than replacing it:
            // that one is a delta-seconds count feeding the retry sleep, and
            // widening it to HTTP-dates would change how long a retry waits.
            // This copy goes through `RetryAfter::parse`, so all three RFC 9110
            // date formats reach a health row without touching lane timing.
            let retry_after_header = resp
                .headers()
                .get("retry-after")
                .and_then(|v| v.to_str().ok())
                .map(str::to_string);
            let resp_text = match resp.text().await {
                Ok(text) => text,
                Err(e) => {
                    let is_auth_status = status.as_u16() == 401 || status.as_u16() == 403;
                    if status.as_u16() == 429 {
                        self.mark_secret_rate_limited(&selected, retry_after, attribution);
                    } else if is_auth_status {
                        self.mark_secret_auth_failed(
                            &selected,
                            Some(&format!("Chat auth failure {status}")),
                            attribution,
                        );
                    } else if status.is_success() {
                        // Headers said 2xx and then the body never finished
                        // arriving: nothing was served, whatever the status
                        // line promised.
                        self.note_deployment_unusable_body(attribution);
                    } else {
                        // 5xx, 402 and the rest still carry their status: the
                        // deployment answered, it just answered badly. (429 and
                        // the auth statuses are handled above — 429 through the
                        // dual record, auth never at all.)
                        self.note_deployment_http_status(
                            attribution,
                            status.as_u16(),
                            retry_after_header.as_deref(),
                        );
                    }
                    last_err = format!("Chat response body read failed after HTTP {status}: {e}");
                    last_class = failure_class_for_status(status.as_u16(), "");
                    // #1197 BUG-1 (codex review): a single bad key must not
                    // fail the whole tier while the primary pool still has
                    // another usable key — same fix as the main 401/403
                    // branch below, applied here for the (rarer) case where
                    // the body read itself also failed.
                    let pool_has_another_key =
                        is_auth_status && self.has_usable_secret_readonly(&cfg.api_key_envs);
                    if attempt < max_attempts && (status.is_server_error() || pool_has_another_key)
                    {
                        tokio::time::sleep(Self::retry_delay(attempt)).await;
                        continue;
                    }
                    if status.as_u16() == 429 || status.is_server_error() || is_auth_status {
                        self.circuit_breakers.record_failure(breaker_key);
                    }
                    return Err(ProviderTierFailure {
                        class: last_class,
                        provider_attempts,
                        latency_ms: tier_started.elapsed().as_millis(),
                        safe_detail: last_err,
                    });
                }
            };

            // Retry on 429 rate-limit or 5xx server errors
            if status.as_u16() == 429 {
                last_class = ProviderInvocationFailureClass::ProviderExhausted;
                self.mark_secret_rate_limited(&selected, retry_after, attribution);
                last_err = format!(
                    "API error {status}: {}",
                    redact_provider_response(&resp_text)
                );
                if attempt < max_attempts {
                    continue;
                }
                self.circuit_breakers.record_failure(breaker_key);
                return Err(ProviderTierFailure {
                    class: last_class,
                    provider_attempts,
                    latency_ms: tier_started.elapsed().as_millis(),
                    safe_detail: last_err,
                });
            }
            if status.as_u16() == 401 || status.as_u16() == 403 {
                let failure_class = chat_auth_failure_class(&resp_text);
                last_class = failure_class_for_status(status.as_u16(), &resp_text);
                if status.as_u16() == 403 && failure_class == "billing_or_quota" {
                    self.mark_secret_exhausted(
                        &selected,
                        Some(&chat_auth_failure_reason(status.as_u16(), &resp_text)),
                        attribution,
                    );
                } else {
                    self.mark_secret_auth_failed(
                        &selected,
                        Some(&chat_auth_failure_reason(status.as_u16(), &resp_text)),
                        attribution,
                    );
                }
                last_err = format!(
                    "API error {status}: class={failure_class}; {}",
                    redact_provider_response(&resp_text)
                );
                // #1197 BUG-1 fix (codex review): marking *this* key
                // auth-failed/exhausted must not, by itself, fail the whole
                // tier — the contract is "fallback fires when the PRIMARY
                // POOL is exhausted", not on one bad key. Re-check the pool
                // (post-mark, so the just-failed key is now excluded) for
                // another usable key before giving up on this tier.
                //
                // This retries up to `MAX_ATTEMPTS` (3) distinct keys, then
                // fails over — it does not (and is not required to)
                // exhaust an arbitrarily large pool before failover;
                // leader-ruled acceptable (codex round-2): retry budget is
                // a fixed, small cap shared with the 429/5xx retry paths
                // above, not "try every key in the pool no matter how
                // many". A larger retry budget, if ever wanted, is a
                // config knob for a future PR, not this one.
                if attempt < max_attempts && self.has_usable_secret_readonly(&cfg.api_key_envs) {
                    eprintln!(
                        "[llm] auth/exhausted error {status} (attempt {}/{}); pool has another key, retrying",
                        attempt,
                        max_attempts
                    );
                    continue;
                }
                self.circuit_breakers.record_failure(breaker_key);
                return Err(ProviderTierFailure {
                    class: last_class,
                    provider_attempts,
                    latency_ms: tier_started.elapsed().as_millis(),
                    safe_detail: last_err,
                });
            }
            if status.is_server_error() {
                // #1681 D4: a 5xx is the deployment's own failure and nobody
                // else's — deployment-only, and it cools the row down only if
                // the provider named a `Retry-After`.
                self.note_deployment_http_status(
                    attribution,
                    status.as_u16(),
                    retry_after_header.as_deref(),
                );
                last_class = ProviderInvocationFailureClass::Transient;
                last_err = format!(
                    "API error {status}: {}",
                    redact_provider_response(&resp_text)
                );
                if attempt < max_attempts {
                    let delay = if let Some(secs) = retry_after {
                        Duration::from_secs(secs)
                    } else {
                        Self::retry_delay(attempt)
                    };
                    eprintln!(
                        "[llm] API error {status} (attempt {}/{}); retrying after {}ms",
                        attempt,
                        max_attempts,
                        delay.as_millis()
                    );
                    tokio::time::sleep(delay).await;
                    continue;
                }
                self.circuit_breakers.record_failure(breaker_key);
                return Err(ProviderTierFailure {
                    class: last_class,
                    provider_attempts,
                    latency_ms: tier_started.elapsed().as_millis(),
                    safe_detail: last_err,
                });
            }

            if !status.is_success() {
                // Everything left over: a `402` (quota spent behind this
                // deployment — throttling, per #1681 D4's 429/quota rule) and
                // the plain refusals, `400`/`404`/`422`. The lane still fails
                // exactly as it did; what changed is that the observation is
                // no longer thrown away.
                self.note_deployment_http_status(
                    attribution,
                    status.as_u16(),
                    retry_after_header.as_deref(),
                );
                return Err(ProviderTierFailure {
                    class: ProviderInvocationFailureClass::LaneOutage,
                    provider_attempts,
                    latency_ms: tier_started.elapsed().as_millis(),
                    safe_detail: format!(
                        "Chat API error {status}: {}",
                        redact_provider_response(&resp_text)
                    ),
                });
            }

            // Parse JSON response
            let json: Value = match serde_json::from_str(&resp_text) {
                Ok(json) => json,
                Err(e) => {
                    // A protocol failure: 2xx, and the body is not the
                    // protocol it claimed. Deployment-only (#1681 D4).
                    self.note_deployment_unusable_body(attribution);
                    return Err(ProviderTierFailure {
                        class: ProviderInvocationFailureClass::LaneOutage,
                        provider_attempts,
                        latency_ms: tier_started.elapsed().as_millis(),
                        safe_detail: format!(
                            "Failed to parse chat response JSON: {e} — {}",
                            redact_provider_response(&resp_text)
                        ),
                    });
                }
            };

            // #1071 fix-round checkpoint 6: read `finish_reason` regardless
            // of whether content came back, so a non-empty-but-cut-off
            // response (`finish_reason == "length"`) is distinguishable from
            // a clean stop, not just used as empty-content diagnostics.
            let finish_reason = json["choices"][0]["finish_reason"].as_str();
            let completion_status = completion_status_from_finish_reason(finish_reason);
            let finish_reason_label = finish_reason.unwrap_or("null");

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
                self.mark_secret_success(&selected, attribution);
                self.circuit_breakers.record_success(breaker_key);
                let usage = parse_usage_tokens(json.get("usage"));
                // `model` may be a caller-controlled override. Bound it before
                // the durable `llm_usage.model` write; routing already happened
                // above with the raw value, so this is a write-sink bound only.
                self.record_successful_llm_usage(
                    lane,
                    &bounded_reference(model),
                    &cfg.base_url,
                    &selected,
                    usage,
                    max_tokens,
                    user.chars().count(),
                    text.chars().count(),
                    attempt_started.elapsed(),
                );
                return Ok(ProviderInvocationOutcome {
                    text,
                    truncated: completion_status == CompletionStatusV1::Truncated,
                    completion_status,
                    receipt: ProviderInvocationReceipt {
                        effective_provider: provider_host(&cfg.base_url),
                        effective_model: json
                            .get("model")
                            .and_then(Value::as_str)
                            .map(str::trim)
                            .filter(|value| !value.is_empty())
                            .map(str::to_string),
                        effective_version: json
                            .get("system_fingerprint")
                            .and_then(Value::as_str)
                            .map(str::trim)
                            .filter(|value| !value.is_empty())
                            .map(str::to_string),
                        fallback_chain: Vec::new(),
                        degraded: false,
                        prompt_tokens: usage.prompt_tokens,
                        completion_tokens: usage.completion_tokens,
                        total_tokens: usage.total_tokens,
                        latency_ms: attempt_started.elapsed().as_millis(),
                    },
                });
            }

            // An empty completion is one of the three shapes
            // `DeploymentOutcome::UnusableResponse` names: the deployment
            // answered, and the answer was of no use. Deployment-only — the
            // credential worked fine.
            self.note_deployment_unusable_body(attribution);

            // Content was empty — build diagnostic info
            let usage = json
                .get("usage")
                .map(|u| u.to_string())
                .unwrap_or_else(|| "unknown".to_string());

            // Same bound as the successful-write sink above: `model` is
            // caller-controlled and this string reaches `eprintln!` below via
            // `last_err`, so it must not be able to forge a second log line.
            let bounded_model = bounded_reference(model);
            last_err = format!(
                "Empty assistant content (finish_reason={finish_reason_label}, usage={usage}, model={bounded_model})"
            );
            last_class = ProviderInvocationFailureClass::LaneOutage;

            if attempt < max_attempts {
                eprintln!(
                    "[llm] empty content (attempt {}/{}): {last_err}; retrying",
                    attempt, max_attempts
                );
                tokio::time::sleep(Self::retry_delay(attempt)).await;
                continue;
            }
        }

        self.circuit_breakers.record_failure(breaker_key);
        Err(ProviderTierFailure {
            class: last_class,
            provider_attempts,
            latency_ms: tier_started.elapsed().as_millis(),
            safe_detail: last_err,
        })
    }

    // TODO(#1197 issue-ask #3, deferred — needs a `memcore` schema change,
    // out of this packet's tachi-llm-only edit boundary): `LlmUsageEvent`
    // only gets recorded on the success path below. Failed attempts (429,
    // 401/403 exhaustion, 5xx, timeouts, and the full-chain "LANE OUTAGE"
    // case) never persist a row, so outage history isn't queryable from the
    // usage table the way successes are — only from the in-memory
    // `lane_outage` streak (`provider_health_status().lane_outages`), which
    // resets on process restart. Adding a failure-class `LlmUsageEvent`
    // variant (lane/model/provider_host/error_class) is the follow-up.
    #[allow(clippy::too_many_arguments)]
    fn record_successful_llm_usage(
        &self,
        lane: ChatLane,
        model: &str,
        base_url: &str,
        selected: &SelectedProviderSecret,
        usage: ChatUsageTokens,
        max_tokens: u32,
        request_chars: usize,
        response_chars: usize,
        duration: Duration,
    ) {
        let Some(db_path) = self.vault_db_path.clone() else {
            return;
        };
        let record = LlmUsageEvent {
            timestamp: chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
            lane: lane.as_str().to_string(),
            model: model.to_string(),
            provider_host: provider_host(base_url),
            provider_logical_name: selected.logical_name.clone(),
            provider_key_id: selected.key_id.clone(),
            prompt_tokens: usage.prompt_tokens,
            completion_tokens: usage.completion_tokens,
            total_tokens: usage.total_tokens,
            max_tokens: i64::from(max_tokens),
            request_chars: request_chars.min(i64::MAX as usize) as i64,
            response_chars: response_chars.min(i64::MAX as usize) as i64,
            duration_ms: duration.as_millis().min(i64::MAX as u128) as i64,
        };
        let migration = self.vault_db_migration.clone();
        let tracker = self
            .llm_usage_persist
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .tracker();
        let background_persist_lock = Arc::clone(&self.background_persist_lock);
        let completion = tracker.track();

        if let Ok(handle) = tokio::runtime::Handle::try_current() {
            handle.spawn(async move {
                let _completion = completion;
                let result = {
                    let _persist_guard = background_persist_lock.lock().await;
                    tokio::task::spawn_blocking(move || {
                        persist_llm_usage_blocking(db_path, migration, record)
                    })
                    .await
                    .map_err(|err| format!("persist llm usage join failed: {err}"))
                    .and_then(|inner| inner)
                };
                if let Err(err) = result {
                    tracker.record_error(err.clone());
                    tracing::warn!("[llm] {err}");
                }
            });
        } else {
            if let Err(err) = persist_llm_usage_blocking(db_path, migration, record) {
                tracker.record_error(err.clone());
                tracing::warn!("[llm] {err}");
            }
        }
    }
}

fn parse_usage_tokens(usage: Option<&Value>) -> ChatUsageTokens {
    let token = |key: &str| {
        usage
            .and_then(|value| value.get(key))
            .and_then(Value::as_i64)
    };
    ChatUsageTokens {
        prompt_tokens: token("prompt_tokens"),
        completion_tokens: token("completion_tokens"),
        total_tokens: token("total_tokens"),
    }
}

fn completion_status_from_finish_reason(finish_reason: Option<&str>) -> CompletionStatusV1 {
    match finish_reason {
        Some("stop") => CompletionStatusV1::Complete,
        Some("length") => CompletionStatusV1::Truncated,
        _ => CompletionStatusV1::Unknown,
    }
}

#[cfg(test)]
mod receipt_tests {
    use super::*;

    #[test]
    fn only_explicit_stop_is_complete_and_only_length_is_truncated() {
        assert_eq!(
            completion_status_from_finish_reason(Some("stop")),
            CompletionStatusV1::Complete
        );
        assert_eq!(
            completion_status_from_finish_reason(Some("length")),
            CompletionStatusV1::Truncated
        );
        for unknown in [None, Some(""), Some("null"), Some("content_filter")] {
            assert_eq!(
                completion_status_from_finish_reason(unknown),
                CompletionStatusV1::Unknown,
                "non-authoritative finish reason {unknown:?} must stay unknown"
            );
        }
    }
}

fn provider_host(base_url: &str) -> String {
    Url::parse(base_url)
        .ok()
        .and_then(|url| url.host_str().map(str::to_string))
        .unwrap_or_default()
}

fn is_retriable_billing_failure(resp_text: &str) -> bool {
    let lower = resp_text.to_ascii_lowercase();
    (lower.contains("balance") && lower.contains("insufficient"))
        || lower.contains("insufficient balance")
        || lower.contains("billing")
        || lower.contains("quota exceeded")
        || lower.contains("余额不足")
}

fn chat_auth_failure_reason(status: u16, resp_text: &str) -> String {
    let class = chat_auth_failure_class(resp_text);
    format!(
        "Chat auth failure {status}: class={class}; {}",
        redact_provider_response(resp_text)
    )
}

fn chat_auth_failure_class(resp_text: &str) -> &'static str {
    if is_retriable_billing_failure(resp_text) {
        "billing_or_quota"
    } else {
        "authentication_or_authorization"
    }
}

fn failure_class_for_status(status: u16, resp_text: &str) -> ProviderInvocationFailureClass {
    match status {
        401 => ProviderInvocationFailureClass::AuthFailed,
        403 if is_retriable_billing_failure(resp_text) => {
            ProviderInvocationFailureClass::ProviderExhausted
        }
        403 => ProviderInvocationFailureClass::AuthFailed,
        429 => ProviderInvocationFailureClass::ProviderExhausted,
        500..=599 => ProviderInvocationFailureClass::Transient,
        _ => ProviderInvocationFailureClass::LaneOutage,
    }
}

/// First boundary for an untrusted provider response body. Callers may inspect
/// the body in memory to classify a retry, but errors, health state, outage
/// aggregation, and tracing receive only this bounded marker.
fn redact_provider_response(resp_text: &str) -> String {
    format!("provider response redacted ({} bytes)", resp_text.len())
}

fn persist_llm_usage_blocking(
    db_path: std::path::PathBuf,
    migration: memcore::MigrationAuthority,
    record: LlmUsageEvent,
) -> Result<(), String> {
    let db_path = db_path
        .to_str()
        .ok_or_else(|| "persist llm usage: invalid db path".to_string())?;
    let open_context = memcore::DbOpenContext {
        intent: memcore::OpenIntent::OpenExisting,
        migration,
        required_profile: memcore::StoreProfile::TachiFull,
    };
    let store = memcore::MemoryStore::open_with_context_and_busy_timeout(
        db_path,
        &open_context,
        LLM_USAGE_PERSIST_SQLITE_BUSY_TIMEOUT,
    )
    .map_err(|err| format!("persist llm usage open db: {err}"))?;
    store
        .record_llm_usage(&record)
        .map_err(|err| format!("persist llm usage insert: {err}"))
}

#[cfg(test)]
mod failure_class_tests {
    use super::*;

    #[test]
    fn spend_aware_failure_classes_are_stable_and_body_free() {
        assert_eq!(
            failure_class_for_status(401, "echoed secret and prompt"),
            ProviderInvocationFailureClass::AuthFailed
        );
        assert_eq!(
            failure_class_for_status(403, "account balance is insufficient"),
            ProviderInvocationFailureClass::ProviderExhausted
        );
        assert_eq!(
            failure_class_for_status(429, "quota body"),
            ProviderInvocationFailureClass::ProviderExhausted
        );
        assert_eq!(
            failure_class_for_status(503, "provider outage body"),
            ProviderInvocationFailureClass::Transient
        );
        assert_eq!(
            failure_class_for_status(400, "unexpected body"),
            ProviderInvocationFailureClass::LaneOutage
        );
        for class in [
            ProviderInvocationFailureClass::AuthFailed,
            ProviderInvocationFailureClass::ProviderExhausted,
            ProviderInvocationFailureClass::Transient,
            ProviderInvocationFailureClass::LaneOutage,
        ] {
            assert!(!class.as_str().contains("body"));
            assert!(!class.as_str().contains("secret"));
            assert!(!class.as_str().contains("prompt"));
        }
    }
}
