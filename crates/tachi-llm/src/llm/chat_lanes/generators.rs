use serde_json::{self, Value};

use crate::{CompletionStatusV1, Generated, LLM_OUTPUT_TRUNCATED};

/// Completion budget shared by both L0 summary generators — legacy
/// `generate_summary` and receipt-paired `generate_summary_with_receipt` — so
/// the two paths cannot diverge. 512 lets a thinking-disabled Flash reply
/// finish without starving (the 2026-09-24 pilot showed the old 100-token
/// budget cutting a clean sentence off into `finish_reason=length`).
///
/// This is a *generation* budget, not a stored-length allowance. The owner
/// direction (2026-09-24) is "brief but not too short": brevity is prompt
/// guidance in [`crate::default_prompts::L0_SUMMARY_PROMPT`], and tachi-llm
/// deliberately enforces no character cap — the 2026-09-24 pilot failed 7/10
/// faithful summaries (103–136 chars) against a hard 100-char gate, and the
/// pre-04421ef3 `≤100 chars` wording elsewhere in the repo was documentation
/// guidance, never an enforced contract. Think-tag scrubbing and emptiness
/// disposition stay at the pre-existing caller seams (backfill's
/// `scrub_think_tags` + `EmptyOutput` skip; the memcore upsert scrub), so
/// this crate returns the provider text verbatim — no silent truncate, no
/// fallback, no extra transform.
pub(crate) const SUMMARY_MAX_TOKENS: u32 = 512;

impl super::super::LlmClient {
    /// Generate L0 summary using `L0_SUMMARY_PROMPT`.
    ///
    /// LLM failures are returned to callers so enrichment/backfill can record a
    /// real failure instead of storing a truncated input as if it were a summary.
    /// This legacy path keeps its documented `finish_reason` compatibility (a
    /// `length` reply with content still returns that text); the provider's
    /// reply is returned verbatim with no length judgment or trimming here.
    pub async fn generate_summary(&self, text: &str) -> Result<String, String> {
        self.call_summary_llm(
            crate::default_prompts::L0_SUMMARY_PROMPT,
            text,
            None,
            0.3,
            SUMMARY_MAX_TOKENS,
        )
        .await
    }

    /// Generate an L0 summary with the actual serving-engine receipt. A
    /// provider-declared truncation is rejected before a downstream producer
    /// can treat the output as a clean artifact. A complete reply is paired
    /// with its receipt and returned verbatim — brevity is the prompt's job,
    /// not an enforced character cap here.
    pub async fn generate_summary_with_receipt(
        &self,
        text: &str,
    ) -> Result<Generated<String>, String> {
        Self::reject_truncated(
            self.call_summary_llm_with_receipt(
                crate::default_prompts::L0_SUMMARY_PROMPT,
                text,
                None,
                0.3,
                SUMMARY_MAX_TOKENS,
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
    /// Distill keeps the legacy `SUMMARY_PROMPT` literal and its own 400-token
    /// budget verbatim; the L0-specific `L0_SUMMARY_PROMPT` and its shared
    /// 512-token budget belong to `generate_summary(_with_receipt)` only and
    /// must not retune this contract.
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
    /// legacy `SUMMARY_PROMPT` and 400-token budget as `generate_distill` so
    /// this API addition does not retune existing model routing or inherit
    /// the L0-specific prompt and budget.
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
        // Unknown remains parseable for legacy compatibility: only the
        // provider's explicit `finish_reason=length` is authoritative evidence
        // of truncation. The receipt preserves Unknown so downstream policy can
        // choose a stricter disposition without us mislabeling it Complete.
        if response.invocation.completion_status() == CompletionStatusV1::Truncated {
            return Err(LLM_OUTPUT_TRUNCATED.to_string());
        }
        Ok(response)
    }
}
