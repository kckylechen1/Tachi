// llm.rs — LLM & Embedding client for memory server
//
// Uses raw reqwest for OpenAI-compatible chat completions.
// SiliconFlow/Qwen still gets `enable_thinking: false` to avoid empty content.

use reqwest::header::{AUTHORIZATION, CONTENT_TYPE};
use serde_json::Value;
use std::collections::HashMap;
use std::sync::{Arc, RwLock};
use std::time::Duration;

const DEFAULT_CHAT_BASE_URL: &str = "https://api.siliconflow.cn/v1/chat/completions";
const DEFAULT_EXTRACT_MODEL: &str = "Qwen/Qwen3.5-27B";
const DEFAULT_REASONING_MODEL: &str = "Qwen/Qwen3.5-27B";

#[derive(Clone)]
struct ChatLaneConfig {
    base_url: String,
    model: String,
    api_key_envs: Vec<&'static str>,
}

#[derive(Clone, Copy)]
enum ChatLane {
    Extract,
    Distill,
    Reasoning,
    Summary,
}

fn non_empty_rerank_documents(documents: &[String]) -> (Vec<&String>, Vec<usize>) {
    documents
        .iter()
        .enumerate()
        .filter(|(_, doc)| !doc.trim().is_empty())
        .map(|(idx, doc)| (doc, idx))
        .unzip()
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
    provider_secrets: Arc<RwLock<HashMap<String, String>>>,
}

impl LlmClient {
    const MAX_ATTEMPTS: usize = 3;
    const BASE_RETRY_DELAY_MS: u64 = 500;

    pub fn new() -> Result<Self, String> {
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
            provider_secrets: Arc::new(RwLock::new(HashMap::new())),
        })
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

    pub fn set_provider_secret(&self, name: &str, value: &str) -> bool {
        let name = name.trim();
        let value = value.trim();
        if name.is_empty() || value.is_empty() {
            return false;
        }

        let mut secrets = self
            .provider_secrets
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        secrets.insert(name.to_string(), value.to_string());
        true
    }

    pub fn set_provider_secrets<I, K, V>(&self, secrets: I) -> usize
    where
        I: IntoIterator<Item = (K, V)>,
        K: AsRef<str>,
        V: AsRef<str>,
    {
        secrets
            .into_iter()
            .filter(|(name, value)| self.set_provider_secret(name.as_ref(), value.as_ref()))
            .count()
    }

    pub fn clear_provider_secrets(&self) {
        self.provider_secrets
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clear();
    }

    pub fn provider_secret_count(&self) -> usize {
        self.provider_secrets
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .len()
    }

    fn first_secret(&self, keys: &[&str]) -> Option<String> {
        let vault_value = {
            let secrets = self
                .provider_secrets
                .read()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            keys.iter().find_map(|key| {
                secrets
                    .get(*key)
                    .map(|value| value.trim().to_string())
                    .filter(|value| !value.is_empty())
            })
        };

        vault_value.or_else(|| {
            Self::first_env(keys).filter(|value| !crate::provider_config::is_vault_alias(value))
        })
    }

    fn required_secret(&self, keys: &[&str]) -> Result<String, String> {
        self.first_secret(keys).ok_or_else(|| {
            "Missing API key. Add one to Tachi Vault or set the appropriate env var.".to_string()
        })
    }

    #[cfg(test)]
    pub(crate) fn provider_secret_for_tests(&self, keys: &[&str]) -> Option<String> {
        self.first_secret(keys)
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

    /// Call Voyage-4 embedding API and return 1024-dim f32 vector.
    /// Convenience wrapper around embed_voyage_batch for single-item use.
    pub async fn embed_voyage(&self, text: &str, input_type: &str) -> Result<Vec<f32>, String> {
        let results = self
            .embed_voyage_batch(&[text.to_string()], input_type)
            .await?;
        results
            .into_iter()
            .next()
            .ok_or_else(|| "Empty batch result".to_string())
    }

    /// Batch call Voyage-4 embedding API. Returns one 1024-dim f32 vector per input text.
    /// Voyage supports up to 128 inputs per request; this method handles chunking internally.
    pub async fn embed_voyage_batch(
        &self,
        texts: &[String],
        input_type: &str,
    ) -> Result<Vec<Vec<f32>>, String> {
        if texts.is_empty() {
            return Ok(vec![]);
        }

        const VOYAGE_MAX_BATCH: usize = 128;
        let mut all_embeddings: Vec<Vec<f32>> = Vec::with_capacity(texts.len());
        let voyage_api_key = self.required_secret(&["VOYAGE_API_KEY"])?;

        for chunk in texts.chunks(VOYAGE_MAX_BATCH) {
            let body = serde_json::json!({
                "model": "voyage-4",
                "input": chunk,
                "input_type": input_type
            });

            let response = self
                .http
                .post("https://api.voyageai.com/v1/embeddings")
                .header(CONTENT_TYPE, "application/json")
                .header(AUTHORIZATION, format!("Bearer {}", voyage_api_key))
                .json(&body)
                .send()
                .await
                .map_err(|e| format!("Voyage batch API request failed: {}", e))?;

            if !response.status().is_success() {
                let status = response.status();
                let text = response.text().await.unwrap_or_default();
                return Err(format!("Voyage batch API error: {} - {}", status, text));
            }

            let json: Value = response
                .json()
                .await
                .map_err(|e| format!("Failed to parse Voyage batch response: {}", e))?;

            let data = json["data"]
                .as_array()
                .ok_or("Invalid Voyage batch response: missing data array")?;

            for item in data {
                let embedding = item["embedding"]
                    .as_array()
                    .ok_or("Invalid Voyage batch response: missing embedding in item")?;

                let vec: Vec<f32> = embedding
                    .iter()
                    .filter_map(|v| v.as_f64().map(|f| f as f32))
                    .collect();

                if vec.len() != 1024 {
                    return Err(format!("Expected 1024-dim embedding, got {}", vec.len()));
                }

                all_embeddings.push(vec);
            }
        }

        if all_embeddings.len() != texts.len() {
            return Err(format!(
                "Voyage batch returned {} embeddings for {} inputs",
                all_embeddings.len(),
                texts.len()
            ));
        }

        Ok(all_embeddings)
    }

    /// Call Voyage rerank API and return (original_index, relevance_score) pairs.
    pub async fn rerank_voyage(
        &self,
        query: &str,
        documents: &[String],
        top_k: usize,
    ) -> Result<Vec<(usize, f64)>, String> {
        let (filtered_docs, index_map) = non_empty_rerank_documents(documents);
        if filtered_docs.is_empty() {
            return Ok(vec![]);
        }
        let voyage_api_key = self.required_secret(&["VOYAGE_RERANK_API_KEY", "VOYAGE_API_KEY"])?;
        let effective_top_k = top_k.max(1).min(filtered_docs.len());

        let body = serde_json::json!({
            "model": "rerank-2.5",
            "query": query,
            "documents": filtered_docs,
            "top_k": effective_top_k,
        });

        let response = self
            .http
            .post("https://api.voyageai.com/v1/rerank")
            .header(CONTENT_TYPE, "application/json")
            .header(AUTHORIZATION, format!("Bearer {}", voyage_api_key))
            .json(&body)
            .send()
            .await
            .map_err(|e| format!("Voyage rerank API request failed: {}", e))?;

        if !response.status().is_success() {
            let status = response.status();
            let text = response.text().await.unwrap_or_default();
            return Err(format!("Voyage rerank API error: {} - {}", status, text));
        }

        let json: Value = response
            .json()
            .await
            .map_err(|e| format!("Failed to parse Voyage rerank response: {}", e))?;
        let data = json["data"]
            .as_array()
            .ok_or("Invalid Voyage rerank response: missing data array")?;

        let mut out = Vec::with_capacity(data.len());
        for item in data {
            let filtered_index = item["index"]
                .as_u64()
                .ok_or("Invalid Voyage rerank response: missing index")?
                as usize;
            let relevance = item["relevance_score"]
                .as_f64()
                .ok_or("Invalid Voyage rerank response: missing relevance_score")?;
            let orig_index = index_map
                .get(filtered_index)
                .copied()
                .unwrap_or(filtered_index);
            out.push((orig_index, relevance));
        }
        Ok(out)
    }

    /// Backward-compatible generic chat call.
    /// Defaults to the reasoning lane unless a caller uses a lane-specific helper.
    #[allow(dead_code)]
    pub async fn call_llm(
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

    pub async fn call_reasoning_llm(
        &self,
        system: &str,
        user: &str,
        model: Option<&str>,
        temperature: f32,
        max_tokens: u32,
    ) -> Result<String, String> {
        // Try Claude Code CLI first for higher-quality reasoning
        match Self::call_claude_cli(system, user).await {
            Ok(response) => {
                tracing::info!(
                    "reasoning via claude-cli succeeded ({} chars)",
                    response.len()
                );
                return Ok(response);
            }
            Err(e) => {
                tracing::warn!("claude-cli reasoning failed, falling back to lane LLM: {e}");
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
        use tokio::process::Command;

        let prompt = format!("<system>\n{system}\n</system>\n\n{user}");

        // Add timeout protection (5 minutes) to prevent indefinite blocking
        let output = tokio::time::timeout(
            Duration::from_secs(300),
            Command::new("claude")
                .arg("-p")
                .arg("--output-format")
                .arg("text")
                .arg("--max-turns")
                .arg("1")
                .arg(&prompt)
                .output(),
        )
        .await
        .map_err(|_| "claude cli timeout after 5 minutes".to_string())?
        .map_err(|e| format!("claude cli spawn failed: {e}"))?;

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
        let api_key = self.required_secret(&lane_cfg.api_key_envs)?;

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
            let resp = self
                .http
                .post(&lane_cfg.base_url)
                .header(CONTENT_TYPE, "application/json")
                .header(AUTHORIZATION, format!("Bearer {}", api_key))
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
            let resp_text = resp
                .text()
                .await
                .unwrap_or_else(|e| format!("<read error: {e}>"));

            // Retry on 429 rate-limit or 5xx server errors
            if status.as_u16() == 429 || status.is_server_error() {
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
    /// On LLM error this falls back to a 100-char truncation of the input.
    /// This is intentional for *summary* (we always want some text), but is
    /// catastrophic for *distill* — the truncated input is never a real
    /// distillation. Distill callers MUST use `generate_distill` instead.
    pub async fn generate_summary(&self, text: &str) -> Result<String, String> {
        match self
            .call_summary_llm(crate::prompts::SUMMARY_PROMPT, text, None, 0.3, 100)
            .await
        {
            Ok(summary) => Ok(summary),
            Err(e) => {
                eprintln!("[llm] generate_summary fell back to truncation after error: {e}");
                // Fallback to truncation on error
                Ok(text.chars().take(100).collect())
            }
        }
    }

    /// Generate a distilled synthesis from concatenated source memories.
    ///
    /// Unlike `generate_summary`, this does NOT silently fall back to
    /// truncation on error. Callers (Foundry distill worker) want a hard
    /// failure so the job is marked failed/skipped rather than persisting
    /// a "frankenstein" memory whose text is just the prompt's input prefix.
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

    fn retry_delay(attempt: usize) -> Duration {
        let multiplier = 1u64 << attempt.saturating_sub(1).min(4);
        Duration::from_millis(Self::BASE_RETRY_DELAY_MS * multiplier)
    }

    /// Remove ```json markdown code fences from response
    pub fn strip_code_fence(text: &str) -> &str {
        let text = text.trim();
        let inner = if text.starts_with("```json") {
            text[7..].trim()
        } else if text.starts_with("```") {
            &text[3..]
        } else {
            return text;
        };

        if let Some(idx) = inner.rfind("```") {
            inner[..idx].trim()
        } else {
            inner
        }
    }

    /// Extract the first complete JSON object/array from an LLM response.
    ///
    /// Some reasoning models prepend hidden-thought text or other prose before
    /// the JSON even when the prompt asks for JSON-only. Keep strict JSON
    /// parsing, but feed the parser the first balanced JSON payload instead of
    /// the whole response.
    pub fn extract_json_payload(text: &str) -> Result<&str, String> {
        let text = Self::strip_code_fence(text).trim();
        let start = text
            .char_indices()
            .find_map(|(idx, ch)| matches!(ch, '{' | '[').then_some((idx, ch)))
            .ok_or_else(|| format!("No JSON object or array found in response: {text}"))?;
        let (start_idx, open) = start;
        let close = if open == '{' { '}' } else { ']' };
        let mut stack = vec![close];
        let mut in_string = false;
        let mut escaped = false;

        for (rel_idx, ch) in text[start_idx..].char_indices().skip(1) {
            if in_string {
                if escaped {
                    escaped = false;
                } else if ch == '\\' {
                    escaped = true;
                } else if ch == '"' {
                    in_string = false;
                }
                continue;
            }

            match ch {
                '"' => in_string = true,
                '{' => stack.push('}'),
                '[' => stack.push(']'),
                '}' | ']' => {
                    if stack.pop() != Some(ch) {
                        return Err(format!("Mismatched JSON delimiter in response: {text}"));
                    }
                    if stack.is_empty() {
                        let end_idx = start_idx + rel_idx + ch.len_utf8();
                        return Ok(&text[start_idx..end_idx]);
                    }
                }
                _ => {}
            }
        }

        Err(format!("Incomplete JSON payload in response: {text}"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    // NOTE: each test below MUST use a unique env-var name. Cargo runs
    // `#[test]` fns in parallel by default, so sharing a process-wide env var
    // causes ordering-dependent flakes (e.g. one test setting the var while
    // another asserts it's unset). See:
    //   crates/memory-server/src/tests.rs::home_test_lock for the pattern we
    //   use when an env var (HOME) genuinely cannot be uniquified.

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
