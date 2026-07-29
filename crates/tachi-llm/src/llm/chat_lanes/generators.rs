use serde_json::{self, Value};

use crate::{CompletionStatusV1, Generated, LLM_OUTPUT_TRUNCATED};

impl super::super::LlmClient {
    /// Generate L0 summary using SUMMARY_PROMPT.
    ///
    /// LLM failures are returned to callers so enrichment/backfill can record a
    /// real failure instead of storing a truncated input as if it were a summary.
    pub async fn generate_summary(&self, text: &str) -> Result<String, String> {
        self.call_summary_llm(crate::default_prompts::SUMMARY_PROMPT, text, None, 0.3, 100)
            .await
    }

    /// Generate an L0 summary with the actual serving-engine receipt. A
    /// provider-declared truncation is rejected before a downstream producer
    /// can treat the output as a clean artifact.
    pub async fn generate_summary_with_receipt(
        &self,
        text: &str,
    ) -> Result<Generated<String>, String> {
        Self::reject_truncated(
            self.call_summary_llm_with_receipt(
                crate::default_prompts::SUMMARY_PROMPT,
                text,
                None,
                0.3,
                100,
            )
            .await?,
        )
    }

    /// Generate a distilled synthesis from concatenated source memories.
    ///
    /// Distill callers (Foundry distill worker) want a hard failure so the job
    /// is marked failed/skipped rather than persisting a "frankenstein" memory
    /// whose text is just the prompt's input prefix.
    ///
    /// Historical bug: prior to this method, `generate_summary` was reused
    /// for distill and its silent fallback produced 15/23 (65%) garbage
    /// distill memories in the antigravity project DB after just two days.
    pub async fn generate_distill(&self, text: &str) -> Result<String, String> {
        let out = self
            .call_summary_llm(crate::default_prompts::SUMMARY_PROMPT, text, None, 0.4, 400)
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

    /// Receipt-preserving distill generator. It deliberately uses the same
    /// summary lane/prompt as the legacy method above so this API addition does
    /// not retune existing model routing.
    pub async fn generate_distill_with_receipt(
        &self,
        text: &str,
    ) -> Result<Generated<String>, String> {
        let response = Self::reject_truncated(
            self.call_summary_llm_with_receipt(
                crate::default_prompts::SUMMARY_PROMPT,
                text,
                None,
                0.4,
                400,
            )
            .await?,
        )?;
        let trimmed = response.value.trim();
        if trimmed.is_empty() {
            return Err("LLM returned empty distill payload".to_string());
        }
        let input_prefix: String = text.chars().take(60).collect();
        let trimmed_prefix = input_prefix.trim();
        if !trimmed_prefix.is_empty() && trimmed.starts_with(trimmed_prefix) {
            return Err(
                "LLM distill output appears to echo the input prefix; rejecting".to_string(),
            );
        }
        Ok(Generated {
            value: trimmed.to_string(),
            invocation: response.invocation,
        })
    }

    /// Extract keywords + entities for search enrichment.
    pub async fn extract_metadata(&self, text: &str) -> Result<(Vec<String>, Vec<String>), String> {
        let response = self
            .call_extract_llm(
                crate::default_prompts::METADATA_EXTRACTION_PROMPT,
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

    /// Extract metadata only after the provider has confirmed that its output
    /// was complete, keeping the receipt paired with the parsed value.
    pub async fn extract_metadata_with_receipt(
        &self,
        text: &str,
    ) -> Result<Generated<(Vec<String>, Vec<String>)>, String> {
        let response = Self::reject_truncated(
            self.call_extract_llm_with_receipt(
                crate::default_prompts::METADATA_EXTRACTION_PROMPT,
                text,
                None,
                0.2,
                400,
            )
            .await?,
        )?;
        let json_str = Self::extract_json_payload(&response.value)?;
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
        Ok(Generated {
            value: (keywords, entities),
            invocation: response.invocation,
        })
    }

    /// Expand synonym + bilingual (zh↔en) search keywords for write-side enrichment (#921).
    ///
    /// Uses the extract chat lane (same provider path as metadata extraction).
    ///
    /// # Egress contract (#568 / #943)
    /// Both `text` **and** every `existing_keywords` entry must already be routed
    /// through the caller's `external_llm_input` / `scrub_secrets` path before
    /// reaching this method. This function interpolates them into the extract-lane
    /// user message as-is; it does not invent a second scrub implementation.
    /// Callers that skip scrubbing on the seed list leak credentials.
    ///
    /// Parsed keywords are lightly sanitized here (length / controls / pure-punct);
    /// the enrichment batcher re-applies the full write-boundary sanitizer before
    /// persist.
    pub async fn expand_search_keywords(
        &self,
        text: &str,
        existing_keywords: &[String],
    ) -> Result<Vec<String>, String> {
        // Seed is assumed secret-scrubbed by the caller (see egress contract).
        let seed = if existing_keywords.is_empty() {
            "(none)".to_string()
        } else {
            existing_keywords.join(", ")
        };
        let user =
            format!("Memory text:\n{text}\n\nExisting keywords: {seed}\n\nReturn JSON only.");
        let response = self
            .call_extract_llm(
                crate::default_prompts::KEYWORD_ENRICHMENT_PROMPT,
                &user,
                None,
                0.2,
                400,
            )
            .await?;
        let json_str = Self::extract_json_payload(&response)?;
        let parsed: Value = serde_json::from_str(json_str).map_err(|e| {
            format!(
                "Failed to parse keyword enrichment JSON: {} - response was: {}",
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
                    .filter_map(Self::sanitize_llm_keyword)
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        Ok(keywords)
    }

    /// Receipt-preserving keyword enrichment parser. The legacy method stays
    /// untouched so existing callers retain its text/error behavior.
    pub async fn expand_search_keywords_with_receipt(
        &self,
        text: &str,
        existing_keywords: &[String],
    ) -> Result<Generated<Vec<String>>, String> {
        let seed = if existing_keywords.is_empty() {
            "(none)".to_string()
        } else {
            existing_keywords.join(", ")
        };
        let user =
            format!("Memory text:\n{text}\n\nExisting keywords: {seed}\n\nReturn JSON only.");
        let response = Self::reject_truncated(
            self.call_extract_llm_with_receipt(
                crate::default_prompts::KEYWORD_ENRICHMENT_PROMPT,
                &user,
                None,
                0.2,
                400,
            )
            .await?,
        )?;
        let json_str = Self::extract_json_payload(&response.value)?;
        let parsed: Value = serde_json::from_str(json_str).map_err(|e| {
            format!(
                "Failed to parse keyword enrichment JSON: {} - response was: {}",
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
                    .filter_map(Self::sanitize_llm_keyword)
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        Ok(Generated {
            value: keywords,
            invocation: response.invocation,
        })
    }

    /// Lightweight LLM-output keyword filter (full write-boundary sanitizer lives
    /// in tachi-server enrichment). Bounds length and drops control / pure-punct.
    pub(crate) fn sanitize_llm_keyword(raw: &str) -> Option<String> {
        const MAX_LEN: usize = 64;
        let stripped: String = raw
            .chars()
            .filter(|c| {
                let u = *c as u32;
                !(u <= 0x1F || (0x7F..=0x9F).contains(&u))
            })
            .collect();
        let trimmed = stripped.trim();
        if trimmed.is_empty() || !trimmed.chars().any(|c| c.is_alphanumeric()) {
            return None;
        }
        Some(trimmed.chars().take(MAX_LEN).collect())
    }

    /// Extract structured facts from text using EXTRACTION_PROMPT
    pub async fn extract_facts(&self, text: &str) -> Result<Vec<Value>, String> {
        let response = self
            .call_extract_llm(
                crate::default_prompts::EXTRACTION_PROMPT,
                text,
                None,
                0.3,
                2000,
            )
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

    /// Receipt-preserving fact extraction. Truncation is adjudicated before
    /// fence stripping or JSON parsing, so no incomplete provider response can
    /// become a clean parsed `Generated` value.
    pub async fn extract_facts_with_receipt(
        &self,
        text: &str,
    ) -> Result<Generated<Vec<Value>>, String> {
        let response = Self::reject_truncated(
            self.call_extract_llm_with_receipt(
                crate::default_prompts::EXTRACTION_PROMPT,
                text,
                None,
                0.3,
                2000,
            )
            .await?,
        )?;
        let json_str = Self::extract_json_payload(&response.value)?;
        if json_str.trim().is_empty() {
            return Err("LLM returned empty facts payload after stripping fences".to_string());
        }
        let value = serde_json::from_str(json_str).map_err(|e| {
            format!(
                "Failed to parse facts JSON: {} - response was: {}",
                e, json_str
            )
        })?;
        Ok(Generated {
            value,
            invocation: response.invocation,
        })
    }

    fn reject_truncated(response: Generated<String>) -> Result<Generated<String>, String> {
        if response.invocation.completion_status == CompletionStatusV1::Truncated {
            return Err(LLM_OUTPUT_TRUNCATED.to_string());
        }
        Ok(response)
    }
}
