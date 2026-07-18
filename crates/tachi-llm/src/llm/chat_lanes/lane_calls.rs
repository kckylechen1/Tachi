use memcore::store::llm_usage::LlmUsageEvent;
use reqwest::{
    header::{AUTHORIZATION, CONTENT_TYPE},
    Url,
};
use serde_json::{self, Value};
use std::time::{Duration, Instant};

use super::super::provider_health::{ChatLane, ChatLaneConfig, SelectedProviderSecret};

#[derive(Clone, Copy)]
struct ChatUsageTokens {
    prompt_tokens: Option<i64>,
    completion_tokens: Option<i64>,
    total_tokens: Option<i64>,
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
        self.call_lane_llm(
            ChatLane::Extract,
            system,
            user,
            model,
            temperature,
            max_tokens,
        )
        .await
        .map(|(text, _truncated)| text)
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
        .map(|(text, _truncated)| text)
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
        .map(|(text, _truncated)| text)
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
        .map(|(text, _truncated)| text)
    }

    /// Returns `(text, truncated)` — `truncated` is `true` when the
    /// provider's `finish_reason` is `"length"` on an otherwise-successful,
    /// non-empty response (#1071 fix-round checkpoint 6: truncated synthesis
    /// must never be silently reported as a clean `completed` answer).
    ///
    /// #1197: walks a per-lane provider chain (primary, then a configured
    /// cross-provider fallback if any) instead of stopping at the primary's
    /// exhaustion. Each tier gets its own circuit breaker key, so a lane
    /// whose primary provider is degraded (breaker open, or its whole key
    /// pool exhausted/auth-failed) fast-escalates to the fallback instead of
    /// silently stalling every background caller. Only when *every*
    /// configured tier fails do we return the loud, typed "lane outage"
    /// error and bump the lane-outage counter (`provider_health_status()
    /// .lane_outages`) — this must never be a silent hang.
    pub(in crate::llm::chat_lanes) async fn call_lane_llm(
        &self,
        lane: ChatLane,
        system: &str,
        user: &str,
        model_override: Option<&str>,
        temperature: f32,
        max_tokens: u32,
    ) -> Result<(String, bool), String> {
        let primary_cfg = self.lane(lane).clone();
        let primary_breaker_key = format!("chat:{}", lane.as_str());

        let mut tiers: Vec<(ChatLaneConfig, String)> = vec![(primary_cfg.clone(), primary_breaker_key)];
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
                    system,
                    user,
                    model_override,
                    temperature,
                    max_tokens,
                )
                .await
            {
                Ok(result) => {
                    self.lane_outage.record_chain_success(lane.as_str());
                    return Ok(result);
                }
                Err(e) => last_err = e,
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
        system: &str,
        user: &str,
        model_override: Option<&str>,
        temperature: f32,
        max_tokens: u32,
    ) -> Result<(String, bool), String> {
        let model = model_override.unwrap_or(&cfg.model);

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

        for attempt in 1..=Self::MAX_ATTEMPTS {
            let Some(selected) = self
                .required_selected_secret_or_wait(&cfg.api_key_envs, attempt, "chat lane")
                .await?
            else {
                continue;
            };
            let attempt_started = Instant::now();
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
                    self.circuit_breakers.record_failure(breaker_key);
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
                    let is_auth_status = status.as_u16() == 401 || status.as_u16() == 403;
                    if status.as_u16() == 429 {
                        self.mark_secret_rate_limited(&selected, retry_after);
                    } else if is_auth_status {
                        self.mark_secret_auth_failed(
                            &selected,
                            Some(&format!("Chat auth failure {status}")),
                        );
                    }
                    last_err = format!("Chat response body read failed after HTTP {status}: {e}");
                    // #1197 BUG-1 (codex review): a single bad key must not
                    // fail the whole tier while the primary pool still has
                    // another usable key — same fix as the main 401/403
                    // branch below, applied here for the (rarer) case where
                    // the body read itself also failed.
                    let pool_has_another_key =
                        is_auth_status && self.has_usable_secret_readonly(&cfg.api_key_envs);
                    if attempt < Self::MAX_ATTEMPTS
                        && (status.is_server_error() || pool_has_another_key)
                    {
                        tokio::time::sleep(Self::retry_delay(attempt)).await;
                        continue;
                    }
                    if status.as_u16() == 429 || status.is_server_error() || is_auth_status {
                        self.circuit_breakers.record_failure(breaker_key);
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
                self.circuit_breakers.record_failure(breaker_key);
                return Err(last_err);
            }
            if status.as_u16() == 401 || status.as_u16() == 403 {
                if status.as_u16() == 403 && is_retriable_billing_failure(&resp_text) {
                    self.mark_secret_exhausted(
                        &selected,
                        Some(&chat_auth_failure_reason(status.as_u16(), &resp_text)),
                    );
                } else {
                    self.mark_secret_auth_failed(
                        &selected,
                        Some(&chat_auth_failure_reason(status.as_u16(), &resp_text)),
                    );
                }
                last_err = format!("API error {status}: {resp_text}");
                // #1197 BUG-1 fix (codex review): marking *this* key
                // auth-failed/exhausted must not, by itself, fail the whole
                // tier — the contract is "fallback fires when the PRIMARY
                // POOL is exhausted", not on one bad key. Re-check the pool
                // (post-mark, so the just-failed key is now excluded) for
                // another usable key before giving up on this tier; only a
                // genuinely exhausted pool falls through to the loud error
                // that the caller's tier-chain treats as "this tier is
                // down".
                if attempt < Self::MAX_ATTEMPTS && self.has_usable_secret_readonly(&cfg.api_key_envs) {
                    eprintln!(
                        "[llm] auth/exhausted error {status} (attempt {}/{}); pool has another key, retrying",
                        attempt,
                        Self::MAX_ATTEMPTS
                    );
                    continue;
                }
                self.circuit_breakers.record_failure(breaker_key);
                return Err(last_err);
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
                self.circuit_breakers.record_failure(breaker_key);
                return Err(last_err);
            }

            if !status.is_success() {
                return Err(format!("Chat API error {status}: {resp_text}"));
            }

            // Parse JSON response
            let json: Value = serde_json::from_str(&resp_text).map_err(|e| {
                format!("Failed to parse chat response JSON: {e} — raw: {resp_text}")
            })?;

            // #1071 fix-round checkpoint 6: read `finish_reason` regardless
            // of whether content came back, so a non-empty-but-cut-off
            // response (`finish_reason == "length"`) is distinguishable from
            // a clean stop, not just used as empty-content diagnostics.
            let finish_reason = json["choices"][0]["finish_reason"]
                .as_str()
                .unwrap_or("null")
                .to_string();

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
                self.circuit_breakers.record_success(breaker_key);
                self.record_successful_llm_usage(
                    lane,
                    model,
                    &cfg.base_url,
                    &selected,
                    parse_usage_tokens(json.get("usage")),
                    max_tokens,
                    user.chars().count(),
                    text.chars().count(),
                    attempt_started.elapsed(),
                );
                return Ok((text, finish_reason == "length"));
            }

            // Content was empty — build diagnostic info
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

        self.circuit_breakers.record_failure(breaker_key);
        Err(last_err)
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

        if let Ok(handle) = tokio::runtime::Handle::try_current() {
            handle.spawn(async move {
                if let Err(err) =
                    tokio::task::spawn_blocking(move || persist_llm_usage_blocking(db_path, record))
                        .await
                        .map_err(|err| format!("persist llm usage join failed: {err}"))
                        .and_then(|inner| inner)
                {
                    tracing::warn!("[llm] {err}");
                }
            });
        } else if let Err(err) = persist_llm_usage_blocking(db_path, record) {
            tracing::warn!("[llm] {err}");
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
    let snippet: String = resp_text.chars().take(300).collect();
    if snippet.trim().is_empty() {
        format!("Chat auth failure {status}")
    } else {
        format!("Chat auth failure {status}: {snippet}")
    }
}

fn persist_llm_usage_blocking(
    db_path: std::path::PathBuf,
    record: LlmUsageEvent,
) -> Result<(), String> {
    let db_path = db_path
        .to_str()
        .ok_or_else(|| "persist llm usage: invalid db path".to_string())?;
    let store = memcore::MemoryStore::open(db_path)
        .map_err(|err| format!("persist llm usage open db: {err}"))?;
    store
        .record_llm_usage(&record)
        .map_err(|err| format!("persist llm usage insert: {err}"))
}
