use reqwest::header::{AUTHORIZATION, CONTENT_TYPE};
use serde_json::{self, Value};
use std::time::Duration;

use super::super::provider_health::ChatLane;

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
}
