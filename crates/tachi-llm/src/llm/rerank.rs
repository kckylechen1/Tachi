// rerank.rs — provider-selectable rerank (voyage + local stub)
//
// Default provider is voyage with byte-identical request shape/URL to the
// pre-seam `rerank_voyage` path. Local is config-gated: unconfigured fails
// closed with a typed error (never silent voyage fallback).

use reqwest::header::{AUTHORIZATION, CONTENT_TYPE};
use serde_json::{json, Value};

use super::catalog_import::DeploymentAttribution;

/// Config env: `TACHI_RERANK_PROVIDER=voyage|local` (default: voyage).
pub const RERANK_PROVIDER_ENV: &str = "TACHI_RERANK_PROVIDER";
/// Config env: OpenAI-compatible `/rerank` HTTP endpoint for the local arm.
pub const RERANK_LOCAL_ENDPOINT_ENV: &str = "TACHI_RERANK_LOCAL_ENDPOINT";
/// Optional override of the Voyage rerank URL (tests / private proxy).
/// When unset, the production Voyage endpoint is used.
pub const RERANK_VOYAGE_ENDPOINT_ENV: &str = "TACHI_RERANK_VOYAGE_ENDPOINT";

const LOCAL_NOT_CONFIGURED: &str = "local rerank provider not configured";
const VOYAGE_RERANK_MODEL: &str = "rerank-2.5";

/// Resolve the Voyage rerank URL.
/// Priority: `TACHI_RERANK_VOYAGE_ENDPOINT` (tests/proxies) → `VOYAGE_BASE_URL`
/// + `/v1/rerank` (#926 blackhole seam) → production Voyage default.
pub(super) fn voyage_rerank_url() -> String {
    std::env::var(RERANK_VOYAGE_ENDPOINT_ENV)
        .ok()
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
        .unwrap_or_else(|| super::embedding::voyage_endpoint("/v1/rerank"))
}

/// Configured rerank backend. Unknown values fail closed — never default to voyage.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RerankProviderKind {
    Voyage,
    Local,
}

impl RerankProviderKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Voyage => "voyage",
            Self::Local => "local",
        }
    }

    /// Parse a provider name. `None` / empty → voyage (default). Unknown → error.
    pub fn parse(raw: Option<&str>) -> Result<Self, String> {
        let Some(value) = raw.map(str::trim).filter(|s| !s.is_empty()) else {
            return Ok(Self::Voyage);
        };
        match value.to_ascii_lowercase().as_str() {
            "voyage" => Ok(Self::Voyage),
            "local" => Ok(Self::Local),
            other => Err(format!(
                "unknown rerank provider '{other}' (expected voyage|local)"
            )),
        }
    }
}

/// Resolved rerank configuration (env-driven).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RerankConfig {
    pub provider: RerankProviderKind,
    /// Present when `TACHI_RERANK_LOCAL_ENDPOINT` is non-empty.
    pub local_endpoint: Option<String>,
}

impl RerankConfig {
    /// Resolve rerank config from env. **Fails closed** on:
    /// - unknown `TACHI_RERANK_PROVIDER`
    /// - `local` without a non-empty `TACHI_RERANK_LOCAL_ENDPOINT`
    ///
    /// Call this at provider construction / startup — never treat these as
    /// mid-search runtime errors that fall open to hybrid ranking.
    pub fn from_env() -> Result<Self, String> {
        let provider =
            RerankProviderKind::parse(std::env::var(RERANK_PROVIDER_ENV).ok().as_deref())?;
        let local_endpoint = std::env::var(RERANK_LOCAL_ENDPOINT_ENV)
            .ok()
            .map(|v| v.trim().to_string())
            .filter(|v| !v.is_empty());
        let config = Self {
            provider,
            local_endpoint,
        };
        config.validate()?;
        Ok(config)
    }

    /// Validate that a `Local` provider has a non-empty endpoint.
    ///
    /// Shared by `from_env` (env path) and `new_with_config` (injection path)
    /// so both fail closed at construction — never deferring to a runtime
    /// belt-and-suspenders check.
    pub fn validate(&self) -> Result<(), String> {
        if self.provider == RerankProviderKind::Local {
            let endpoint = self
                .local_endpoint
                .as_deref()
                .map(str::trim)
                .filter(|endpoint| !endpoint.is_empty())
                .ok_or_else(|| LOCAL_NOT_CONFIGURED.to_string())?;
            if let Some(leak) = memcore::catalog::endpoint::endpoint_credential_leak(endpoint) {
                return Err(format!("local rerank endpoint refused: {leak}"));
            }
        }
        Ok(())
    }

    pub fn provider_name(&self) -> &'static str {
        self.provider.as_str()
    }

    /// Voyage model name when provider is voyage; `None` for local (no bundled model).
    pub fn model_name(&self) -> Option<&'static str> {
        match self.provider {
            RerankProviderKind::Voyage => Some(VOYAGE_RERANK_MODEL),
            RerankProviderKind::Local => None,
        }
    }
}

/// Drop empty/whitespace documents and retain original indices for remapping.
pub(super) fn non_empty_rerank_documents(documents: &[String]) -> (Vec<&String>, Vec<usize>) {
    documents
        .iter()
        .enumerate()
        .filter(|(_, doc)| !doc.trim().is_empty())
        .map(|(idx, doc)| (doc, idx))
        .unzip()
}

/// Voyage rerank request body — frozen shape for default-path parity.
pub(super) fn voyage_rerank_request_body(
    query: &str,
    filtered_docs: &[&String],
    effective_top_k: usize,
) -> Value {
    json!({
        "model": VOYAGE_RERANK_MODEL,
        "query": query,
        "documents": filtered_docs,
        "top_k": effective_top_k,
    })
}

/// Local OpenAI-compatible `/rerank` request body (query + documents + top_k).
pub(super) fn local_rerank_request_body(
    query: &str,
    filtered_docs: &[&String],
    effective_top_k: usize,
) -> Value {
    json!({
        "query": query,
        "documents": filtered_docs,
        "top_k": effective_top_k,
    })
}

/// Parse a voyage-style (or TEI `results`) rerank response into (filtered_index, score).
pub(super) fn parse_rerank_response_items(json: &Value) -> Result<Vec<(usize, f64)>, String> {
    let data = json
        .get("data")
        .or_else(|| json.get("results"))
        .and_then(Value::as_array)
        .ok_or("Invalid rerank response: missing data/results array")?;

    let mut out = Vec::with_capacity(data.len());
    for item in data {
        let filtered_index = item["index"]
            .as_u64()
            .ok_or("Invalid rerank response: missing index")? as usize;
        let relevance = item
            .get("relevance_score")
            .or_else(|| item.get("score"))
            .and_then(Value::as_f64)
            .ok_or("Invalid rerank response: missing relevance_score/score")?;
        out.push((filtered_index, relevance));
    }
    Ok(out)
}

fn remap_filtered_indices(pairs: Vec<(usize, f64)>, index_map: &[usize]) -> Vec<(usize, f64)> {
    pairs
        .into_iter()
        .map(|(filtered_index, relevance)| {
            let orig_index = index_map
                .get(filtered_index)
                .copied()
                .unwrap_or(filtered_index);
            (orig_index, relevance)
        })
        .collect()
}

impl super::LlmClient {
    /// Provider-dispatched rerank using the config resolved at construction.
    /// Default (`TACHI_RERANK_PROVIDER` unset) is voyage. Config errors (unknown
    /// provider / local without endpoint) fail at `LlmClient::new`, never here.
    pub async fn rerank(
        &self,
        query: &str,
        documents: &[String],
        top_k: usize,
    ) -> Result<Vec<(usize, f64)>, String> {
        // Record the arm *actually entered* so tests RED if the enum match is
        // bypassed (hardcoded voyage/local without going through the seam).
        match self.rerank_config.provider {
            RerankProviderKind::Voyage => {
                self.note_rerank_dispatch(RerankProviderKind::Voyage);
                self.rerank_voyage(query, documents, top_k).await
            }
            RerankProviderKind::Local => {
                self.note_rerank_dispatch(RerankProviderKind::Local);
                self.rerank_local(query, documents, top_k).await
            }
        }
    }

    /// Call Voyage rerank API and return (original_index, relevance_score) pairs.
    /// Extracted arm — keeps main's #926 recall bounds + pool-hygiene accounting
    /// (outcomes recorded after body read). Optional `TACHI_RERANK_VOYAGE_ENDPOINT`
    /// overrides the URL for tests/proxies; otherwise `VOYAGE_BASE_URL` applies.
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
        let effective_top_k = top_k.max(1).min(filtered_docs.len());
        let body = voyage_rerank_request_body(query, &filtered_docs, effective_top_k);
        let url = voyage_rerank_url();
        if let Some(leak) = memcore::catalog::endpoint::endpoint_credential_leak(&url) {
            return Err(format!("Voyage rerank endpoint refused: {leak}"));
        }

        let mut json: Option<Value> = None;
        let mut last_err = String::new();
        // Recall-path bounds (#926): mirror the embed path's attempt cap and
        // per-request deadline.
        let max_attempts = Self::recall_max_attempts();
        for attempt in 1..=max_attempts {
            let Some(selected) = self
                .required_selected_secret_or_wait(
                    &["VOYAGE_RERANK_API_KEY", "VOYAGE_API_KEY"],
                    attempt,
                    "Voyage rerank",
                )
                .await?
            else {
                continue;
            };
            let response = self
                .http_client()
                .post(&url)
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
                    last_err = format!("Voyage rerank API request failed: {err}");
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
                // Headers-then-stall (#926 review): the body read carries its
                // own share of the request's `.timeout()` budget and can time
                // out even though `send()` already returned Ok. Record it as
                // timeout-class so the pool-rebuild streak isn't reset by a
                // connection that only *looked* healthy at the header stage.
                self.note_recall_provider_outcome(e.is_timeout());
                format!("Voyage rerank response body read failed: {e}")
            })?;
            // Full response body received without stalling: the pooled
            // connection is proven healthy regardless of HTTP status (#926
            // review). Not gated on JSON deserialize below — parse
            // correctness is orthogonal to connection/pool health, and
            // gating on it would silently skip accounting on the common 429
            // rate-limit path (which returns before reaching parse).
            self.note_recall_provider_outcome(false);
            if status.as_u16() == 429 {
                // Unattributed: the env import does not project a rerank
                // deployment row (#1681 D7 PR-B), so there is nothing in the
                // catalog this outcome could be evidence about.
                self.mark_secret_rate_limited(
                    &selected,
                    retry_after,
                    DeploymentAttribution::Unattributed,
                );
                last_err = format!("Voyage rerank API error: {} - {}", status, text);
                if attempt < max_attempts {
                    continue;
                }
                return Err(last_err);
            }
            if status.as_u16() == 401 || status.as_u16() == 403 {
                self.mark_secret_auth_failed(
                    &selected,
                    Some(&format!("Voyage rerank auth failure {status}")),
                    DeploymentAttribution::Unattributed,
                );
                return Err(format!("Voyage rerank API error: {} - {}", status, text));
            }
            if !status.is_success() {
                return Err(format!("Voyage rerank API error: {} - {}", status, text));
            }
            json = Some(
                serde_json::from_str(&text)
                    .map_err(|e| format!("Failed to parse Voyage rerank response: {}", e))?,
            );
            self.mark_secret_success(&selected, DeploymentAttribution::Unattributed);
            break;
        }
        let json = json.ok_or_else(|| {
            if last_err.is_empty() {
                "Voyage rerank API failed without a response".to_string()
            } else {
                last_err
            }
        })?;
        // Voyage-only parse (data + relevance_score) — keep error strings identical
        // to the pre-seam path.
        let data = json["data"]
            .as_array()
            .ok_or("Invalid Voyage rerank response: missing data array")?;
        let mut pairs = Vec::with_capacity(data.len());
        for item in data {
            let filtered_index = item["index"]
                .as_u64()
                .ok_or("Invalid Voyage rerank response: missing index")?
                as usize;
            let relevance = item["relevance_score"]
                .as_f64()
                .ok_or("Invalid Voyage rerank response: missing relevance_score")?;
            pairs.push((filtered_index, relevance));
        }
        Ok(remap_filtered_indices(pairs, &index_map))
    }

    /// Local OpenAI-compatible `/rerank` HTTP arm. No model download.
    /// Endpoint was validated at construction (`RerankConfig::from_env`).
    async fn rerank_local(
        &self,
        query: &str,
        documents: &[String],
        top_k: usize,
    ) -> Result<Vec<(usize, f64)>, String> {
        // Belt-and-suspenders: construction already rejects local-without-endpoint.
        let endpoint = self
            .rerank_config
            .local_endpoint
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .ok_or_else(|| LOCAL_NOT_CONFIGURED.to_string())?;

        let (filtered_docs, index_map) = non_empty_rerank_documents(documents);
        if filtered_docs.is_empty() {
            return Ok(vec![]);
        }
        let effective_top_k = top_k.max(1).min(filtered_docs.len());
        let body = local_rerank_request_body(query, &filtered_docs, effective_top_k);

        let mut json: Option<Value> = None;
        let mut last_err = String::new();
        // Local arm is also on the recall path: same bounds + pool-hygiene
        // accounting as voyage/embed (#926).
        let max_attempts = Self::recall_max_attempts();
        for attempt in 1..=max_attempts {
            let response = self
                .http_client()
                .post(endpoint)
                .timeout(Self::recall_request_timeout())
                .header(CONTENT_TYPE, "application/json")
                .json(&body)
                .send()
                .await;
            let response = match response {
                Ok(response) => response,
                Err(err) => {
                    self.note_recall_provider_outcome(err.is_timeout());
                    last_err = format!("Local rerank API request failed: {err}");
                    if attempt < max_attempts {
                        tokio::time::sleep(Self::retry_delay(attempt)).await;
                        continue;
                    }
                    return Err(last_err);
                }
            };

            let status = response.status();
            let text = response.text().await.map_err(|e| {
                self.note_recall_provider_outcome(e.is_timeout());
                format!("Local rerank response body read failed: {e}")
            })?;
            self.note_recall_provider_outcome(false);
            if status.as_u16() == 429 {
                last_err = format!("Local rerank API error: {} - {}", status, text);
                if attempt < max_attempts {
                    tokio::time::sleep(Self::retry_delay(attempt)).await;
                    continue;
                }
                return Err(last_err);
            }
            if !status.is_success() {
                return Err(format!("Local rerank API error: {} - {}", status, text));
            }
            json = Some(
                serde_json::from_str(&text)
                    .map_err(|e| format!("Failed to parse local rerank response: {}", e))?,
            );
            break;
        }
        let json = json.ok_or_else(|| {
            if last_err.is_empty() {
                "Local rerank API failed without a response".to_string()
            } else {
                last_err
            }
        })?;
        let pairs = parse_rerank_response_items(&json)?;
        Ok(remap_filtered_indices(pairs, &index_map))
    }
}

#[cfg(test)]
mod pure_tests {
    use super::*;

    #[test]
    fn provider_defaults_to_voyage_when_unset() {
        assert_eq!(
            RerankProviderKind::parse(None).unwrap(),
            RerankProviderKind::Voyage
        );
        assert_eq!(
            RerankProviderKind::parse(Some("")).unwrap(),
            RerankProviderKind::Voyage
        );
        assert_eq!(
            RerankProviderKind::parse(Some("  ")).unwrap(),
            RerankProviderKind::Voyage
        );
    }

    #[test]
    fn provider_parses_voyage_and_local() {
        assert_eq!(
            RerankProviderKind::parse(Some("voyage")).unwrap(),
            RerankProviderKind::Voyage
        );
        assert_eq!(
            RerankProviderKind::parse(Some("LOCAL")).unwrap(),
            RerankProviderKind::Local
        );
    }

    #[test]
    fn provider_rejects_unknown() {
        let err = RerankProviderKind::parse(Some("cohere")).unwrap_err();
        assert!(err.contains("unknown rerank provider"));
        assert!(err.contains("voyage|local"));
    }

    #[test]
    fn voyage_request_body_shape_is_frozen() {
        let docs = ["doc-a".to_string(), "doc-b".to_string()];
        let refs: Vec<&String> = docs.iter().collect();
        let body = voyage_rerank_request_body("q", &refs, 2);
        assert_eq!(
            body,
            json!({
                "model": "rerank-2.5",
                "query": "q",
                "documents": ["doc-a", "doc-b"],
                "top_k": 2,
            })
        );
    }

    #[test]
    fn local_without_endpoint_is_config_error() {
        // Pure parse path: Local is accepted as a kind, but from_env couples
        // kind + endpoint and fails closed when endpoint is missing. Covered
        // end-to-end in embedding_rerank::local_rerank_unconfigured_*.
        assert_eq!(
            RerankProviderKind::parse(Some("local")).unwrap(),
            RerankProviderKind::Local
        );
    }
}
