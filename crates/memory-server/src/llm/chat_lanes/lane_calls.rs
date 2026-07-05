use reqwest::{
    header::{AUTHORIZATION, CONTENT_TYPE},
    Url,
};
use rusqlite::params;
use serde_json::{self, Value};
use std::time::{Duration, Instant};

use super::super::provider_health::{ChatLane, SelectedProviderSecret};

#[derive(Clone, Copy)]
struct ChatUsageTokens {
    prompt_tokens: Option<i64>,
    completion_tokens: Option<i64>,
    total_tokens: Option<i64>,
}

struct LlmUsageRecord {
    timestamp: String,
    lane: &'static str,
    model: String,
    provider_host: String,
    provider_logical_name: String,
    provider_key_id: String,
    prompt_tokens: Option<i64>,
    completion_tokens: Option<i64>,
    total_tokens: Option<i64>,
    max_tokens: i64,
    request_chars: i64,
    response_chars: i64,
    duration_ms: i64,
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

    pub(in crate::llm::chat_lanes) async fn call_lane_llm(
        &self,
        lane: ChatLane,
        system: &str,
        user: &str,
        model_override: Option<&str>,
        temperature: f32,
        max_tokens: u32,
    ) -> Result<String, String> {
        let breaker_key = format!("chat:{}", lane.as_str());
        if !self.circuit_breakers.allow(&breaker_key) {
            return Err(format!(
                "Circuit breaker open for {} lane — provider is failing, fast-rejecting. Retry in ~30s.",
                lane.as_str()
            ));
        }

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
            let attempt_started = Instant::now();
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
                self.circuit_breakers.record_failure(&breaker_key);
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
                self.circuit_breakers.record_failure(&breaker_key);
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
                self.circuit_breakers.record_success(&breaker_key);
                self.record_successful_llm_usage(
                    lane,
                    model,
                    &lane_cfg.base_url,
                    &selected,
                    parse_usage_tokens(json.get("usage")),
                    max_tokens,
                    user.chars().count(),
                    text.chars().count(),
                    attempt_started.elapsed(),
                );
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

        self.circuit_breakers.record_failure(&breaker_key);
        Err(last_err)
    }

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
        let record = LlmUsageRecord {
            timestamp: chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
            lane: lane.as_str(),
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
    record: LlmUsageRecord,
) -> Result<(), String> {
    let db_path = db_path
        .to_str()
        .ok_or_else(|| "persist llm usage: invalid db path".to_string())?;
    let store = memory_core::MemoryStore::open(db_path)
        .map_err(|err| format!("persist llm usage open db: {err}"))?;
    store
        .connection()
        .execute(
            "INSERT INTO llm_usage (
                timestamp, lane, model, provider_host, provider_logical_name,
                provider_key_id, prompt_tokens, completion_tokens, total_tokens,
                max_tokens, request_chars, response_chars, duration_ms, success,
                created_at
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, 1, ?1)",
            params![
                record.timestamp,
                record.lane,
                record.model,
                record.provider_host,
                record.provider_logical_name,
                record.provider_key_id,
                record.prompt_tokens,
                record.completion_tokens,
                record.total_tokens,
                record.max_tokens,
                record.request_chars,
                record.response_chars,
                record.duration_ms,
            ],
        )
        .map_err(|err| format!("persist llm usage insert: {err}"))?;
    Ok(())
}
