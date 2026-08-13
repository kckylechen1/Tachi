// embedding.rs — Voyage embedding API calls on LlmClient
//
// Rerank lives in `rerank.rs` (provider-selectable seam).

use reqwest::header::{AUTHORIZATION, CONTENT_TYPE};
use serde_json::{json, Value};

use super::catalog_import::{DeploymentAttribution, ENV_EMBEDDING_LANE};
use super::embedding_config::EmbeddingConfig;

/// Voyage API base URL. Defaults to the public endpoint; `VOYAGE_BASE_URL`
/// overrides it (same `*_BASE_URL` idiom as the chat lanes). This is the seam
/// the recall fail-safe test (#926) uses to point embed/rerank at a local
/// blackhole listener.
/// The embeddings URL a request will actually go to.
///
/// Exposed (rather than leaving callers to rebuild `base + path`) so the
/// catalog import and the status projection describe the *same* endpoint the
/// request uses. A second copy of the default URL in a status blob is exactly
/// the hand-maintained mirror #1681 D3 kills.
pub fn voyage_embeddings_endpoint() -> String {
    voyage_endpoint("/v1/embeddings")
}

pub(in crate::llm) fn voyage_endpoint(path: &str) -> String {
    let base = std::env::var("VOYAGE_BASE_URL")
        .ok()
        .map(|v| v.trim().trim_end_matches('/').to_string())
        .filter(|v| !v.is_empty())
        .unwrap_or_else(|| "https://api.voyageai.com".to_string());
    format!("{base}{path}")
}

/// Parse a Voyage batch response.
///
/// `expected_dimension` is the width the *configured* model declares
/// ([`super::embedding_config::EmbeddingConfig`]), not a constant: a response
/// of the wrong width is the observable symptom of an embedding model whose
/// declaration is wrong, and it has to be caught here — before the vectors
/// reach a store that would happily write them at the wrong width.
pub(super) fn parse_voyage_batch_embeddings(
    data: &[Value],
    expected_count: usize,
    expected_dimension: usize,
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

        if vec.len() != expected_dimension {
            return Err(format!(
                "Expected {expected_dimension}-dim embedding, got {}",
                vec.len()
            ));
        }

        embeddings.push(vec);
    }

    Ok(embeddings)
}

impl super::LlmClient {
    /// Call the configured Voyage embedding model and return one f32 vector at
    /// the configured width.
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

    /// Batch call the configured Voyage embedding model. Returns one f32
    /// vector per input text, at the configured width.
    /// Voyage supports up to 128 inputs per request; this method handles chunking internally.
    pub async fn embed_voyage_batch(
        &self,
        texts: &[String],
        input_type: &str,
    ) -> Result<Vec<Vec<f32>>, String> {
        if texts.is_empty() {
            return Ok(vec![]);
        }

        // Resolved per call rather than cached on the client, for the same
        // reason `voyage_endpoint()` above reads `VOYAGE_BASE_URL` per call:
        // two env reads next to an HTTPS request cost nothing, and a cached
        // resolution would make the config a function of when the process
        // happened to build its client. Fails closed — a mis-declared
        // embedding model never reaches the wire (#1681 D3).
        let embedding = EmbeddingConfig::from_env()?;

        const VOYAGE_MAX_BATCH: usize = 128;
        let mut all_embeddings: Vec<Vec<f32>> = Vec::with_capacity(texts.len());
        for chunk in texts.chunks(VOYAGE_MAX_BATCH) {
            let body = json!({
                "model": embedding.model(),
                "input": chunk,
                "input_type": input_type
            });
            let mut chunk_embeddings: Option<Vec<Vec<f32>>> = None;
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
                // Bound here rather than hoisted out of the attempt loop so
                // the endpoint is still resolved exactly once per attempt, in
                // the same order, as before (#1681 D3's per-call resolution).
                let endpoint = voyage_endpoint("/v1/embeddings");
                // The embedding lane has a catalog row of its own
                // (`env:embedding`, #1681 D7 PR-B), so its outcomes are
                // attributable the same way a chat lane's are.
                let attribution = DeploymentAttribution::EnvLane {
                    lane: ENV_EMBEDDING_LANE,
                    endpoint: &endpoint,
                    model: embedding.model(),
                };
                let response = self
                    .http_client()
                    .post(&endpoint)
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
                        // No status line: deployment-only evidence (#1681 D4),
                        // exactly as on the chat lanes.
                        self.note_deployment_transport_failure(attribution);
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
                // The raw header for the deployment authority, parsed through
                // `RetryAfter::parse` so all three RFC 9110 date forms land —
                // see the same read in `lane_calls.rs` on why it is a second
                // read rather than a widening of `retry_after`.
                let retry_after_header = response
                    .headers()
                    .get("retry-after")
                    .and_then(|v| v.to_str().ok())
                    .map(str::to_string);
                let text = response.text().await.map_err(|e| {
                    // The body stalled: whatever the status line said, nothing
                    // usable came back.
                    self.note_deployment_unusable_body(attribution);
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
                    self.mark_secret_rate_limited(&selected, retry_after, attribution);
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
                        attribution,
                    );
                    return Err(format!("Voyage batch API error: {} - {}", status, text));
                }
                if !status.is_success() {
                    // 402, 5xx and the plain refusals — the same gap the chat
                    // lanes had (#1681 D4, codex CP6).
                    self.note_deployment_http_status(
                        attribution,
                        status.as_u16(),
                        retry_after_header.as_deref(),
                    );
                    return Err(format!("Voyage batch API error: {} - {}", status, text));
                }
                let json: Value = serde_json::from_str(&text).map_err(|e| {
                    // Protocol failure: a 2xx body that is not the protocol.
                    self.note_deployment_unusable_body(attribution);
                    format!("Failed to parse Voyage batch response: {}", e)
                })?;
                // Read **before** the success is recorded, not after it. A
                // body that parses as JSON and is not a batch — no `data`
                // array, the wrong number of embeddings, mismatched indexes,
                // the wrong width — is an unusable response, and the deployment
                // authority has to hear about it as one. Validating after
                // `mark_secret_success` recorded `Served`, which clears the
                // cooldown and resets the error count, while the caller was
                // handed an error: the seam's own accounting said the
                // deployment was fine at the moment it demonstrably was not
                // (codex re-review of PR-C, CP6). This is the chat lane's rule
                // too — an empty completion is `note_deployment_unusable_body`
                // and no success (`lane_calls.rs`).
                let embeddings = match json["data"]
                    .as_array()
                    .ok_or_else(|| "Invalid Voyage batch response: missing data array".to_string())
                    .and_then(|data| {
                        parse_voyage_batch_embeddings(
                            data,
                            chunk.len(),
                            embedding.dimension() as usize,
                        )
                    }) {
                    Ok(embeddings) => embeddings,
                    Err(err) => {
                        self.note_deployment_unusable_body(attribution);
                        return Err(err);
                    }
                };
                self.mark_secret_success(&selected, attribution);
                chunk_embeddings = Some(embeddings);
                break;
            }

            all_embeddings.extend(chunk_embeddings.ok_or_else(|| {
                if last_err.is_empty() {
                    "Voyage batch API failed without a response".to_string()
                } else {
                    last_err
                }
            })?);
        }

        Ok(all_embeddings)
    }
}
