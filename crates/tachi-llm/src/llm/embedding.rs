// embedding.rs — Voyage embedding API calls on LlmClient
//
// Rerank lives in `rerank.rs` (provider-selectable seam).

use reqwest::header::{AUTHORIZATION, CONTENT_TYPE};
use serde_json::{json, Value};

/// Voyage API base URL. Defaults to the public endpoint; `VOYAGE_BASE_URL`
/// overrides it (same `*_BASE_URL` idiom as the chat lanes). This is the seam
/// the recall fail-safe test (#926) uses to point embed/rerank at a local
/// blackhole listener.
pub(in crate::llm) fn voyage_endpoint(path: &str) -> String {
    let base = std::env::var("VOYAGE_BASE_URL")
        .ok()
        .map(|v| v.trim().trim_end_matches('/').to_string())
        .filter(|v| !v.is_empty())
        .unwrap_or_else(|| "https://api.voyageai.com".to_string());
    format!("{base}{path}")
}

pub(super) fn parse_voyage_batch_embeddings(
    data: &[Value],
    expected_count: usize,
) -> Result<Vec<Vec<f32>>, String> {
    if data.len() != expected_count {
        return Err(format!(
            "Voyage batch returned {} embeddings for {} inputs",
            data.len(),
            expected_count
        ));
    }

    let mut embeddings = Vec::with_capacity(expected_count);
    for (expected_index, item) in data.iter().enumerate() {
        if let Some(index) = item.get("index") {
            let actual_index = index
                .as_u64()
                .ok_or("Invalid Voyage batch response: index is not an unsigned integer")?
                as usize;
            if actual_index != expected_index {
                return Err(format!(
                    "Voyage batch response index mismatch: expected {expected_index}, got {actual_index}"
                ));
            }
        }

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

        embeddings.push(vec);
    }

    Ok(embeddings)
}

impl super::LlmClient {
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
        for chunk in texts.chunks(VOYAGE_MAX_BATCH) {
            let body = json!({
                "model": "voyage-4",
                "input": chunk,
                "input_type": input_type
            });
            let mut response_json: Option<Value> = None;
            let mut last_err = String::new();
            // Recall-path bounds (#926): fewer attempts + a per-request deadline
            // so a blackholed provider cannot freeze recall for minutes.
            let max_attempts = Self::recall_max_attempts();
            for attempt in 1..=max_attempts {
                let Some(selected) = self
                    .required_selected_secret_or_wait(&["VOYAGE_API_KEY"], attempt, "Voyage batch")
                    .await?
                else {
                    continue;
                };
                let response = self
                    .http_client()
                    .post(voyage_endpoint("/v1/embeddings"))
                    .timeout(Self::recall_request_timeout())
                    .header(CONTENT_TYPE, "application/json")
                    .header(AUTHORIZATION, format!("Bearer {}", selected.value))
                    .json(&body)
                    .send()
                    .await;
                let response = match response {
                    // Connect/send outcome only proves headers *might* arrive —
                    // it does not prove the connection is healthy end to end.
                    // Pool-hygiene accounting (#926 review) waits for the body
                    // read below so a provider that sends headers then stalls
                    // the body forever isn't recorded as a success.
                    Ok(response) => response,
                    Err(err) => {
                        self.note_recall_provider_outcome(err.is_timeout());
                        last_err = format!("Voyage batch API request failed: {err}");
                        if attempt < max_attempts {
                            tokio::time::sleep(Self::retry_delay(attempt)).await;
                            continue;
                        }
                        return Err(last_err);
                    }
                };

                let status = response.status();
                let retry_after = response
                    .headers()
                    .get("retry-after")
                    .and_then(|v| v.to_str().ok())
                    .and_then(|v| v.parse::<u64>().ok());
                let text = response.text().await.map_err(|e| {
                    // Headers-then-stall (#926 review): the body read carries
                    // its own share of the request's `.timeout()` budget and
                    // can time out even though `send()` already returned Ok.
                    // Record it as timeout-class so the pool-rebuild streak
                    // isn't reset by a connection that only *looked* healthy
                    // at the header stage.
                    self.note_recall_provider_outcome(e.is_timeout());
                    format!("Voyage batch response body read failed: {e}")
                })?;
                // Full response body received without stalling: the pooled
                // connection is proven healthy regardless of HTTP status
                // (#926 review). Not gated on JSON deserialize below — parse
                // correctness is orthogonal to connection/pool health, and
                // gating on it would silently skip accounting on the common
                // 429 rate-limit path (which returns before reaching parse).
                self.note_recall_provider_outcome(false);
                if status.as_u16() == 429 {
                    self.mark_secret_rate_limited(&selected, retry_after);
                    last_err = format!("Voyage batch API error: {} - {}", status, text);
                    if attempt < max_attempts {
                        continue;
                    }
                    return Err(last_err);
                }
                if status.as_u16() == 401 || status.as_u16() == 403 {
                    self.mark_secret_auth_failed(
                        &selected,
                        Some(&format!("Voyage batch auth failure {status}")),
                    );
                    return Err(format!("Voyage batch API error: {} - {}", status, text));
                }
                if !status.is_success() {
                    return Err(format!("Voyage batch API error: {} - {}", status, text));
                }
                response_json = Some(
                    serde_json::from_str(&text)
                        .map_err(|e| format!("Failed to parse Voyage batch response: {}", e))?,
                );
                self.mark_secret_success(&selected);
                break;
            }

            let json = response_json.ok_or_else(|| {
                if last_err.is_empty() {
                    "Voyage batch API failed without a response".to_string()
                } else {
                    last_err
                }
            })?;

            let data = json["data"]
                .as_array()
                .ok_or("Invalid Voyage batch response: missing data array")?;
            all_embeddings.extend(parse_voyage_batch_embeddings(data, chunk.len())?);
        }

        Ok(all_embeddings)
    }
}
