// helpers.rs — pure utility methods on LlmClient (no field access)
//
// Retry/backoff math and LLM response parsing. These read no `self` state, so
// they are grouped here independent of the rest of the client, together
// with the boundary every upstream response body passes through. They stay
// associated functions on `LlmClient` so call-sites (`Self::retry_delay`,
// `LlmClient::strip_code_fence`) are unchanged across the split.

use std::time::{Duration, SystemTime, UNIX_EPOCH};

impl super::LlmClient {
    pub(super) fn retry_delay(attempt: usize) -> Duration {
        let seed = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_nanos() as u64)
            .unwrap_or(0);
        Self::retry_delay_with_jitter(attempt, seed)
    }

    pub(super) fn retry_delay_with_jitter(attempt: usize, seed: u64) -> Duration {
        let multiplier = 1u64 << attempt.saturating_sub(1).min(4);
        let base_ms = Self::BASE_RETRY_DELAY_MS * multiplier;
        let jitter_ms = ((base_ms * Self::RETRY_JITTER_PERCENT) / 100).max(1);
        let mixed =
            seed ^ (attempt as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15) ^ base_ms.rotate_left(17);
        Duration::from_millis(base_ms + (mixed % (jitter_ms + 1)))
    }

    /// Remove ```json markdown code fences from response
    pub fn strip_code_fence(text: &str) -> &str {
        let text = text.trim();
        let inner = if let Some(stripped) = text.strip_prefix("```json") {
            stripped.trim()
        } else if let Some(stripped) = text.strip_prefix("```") {
            stripped
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

/// Upper bound on one upstream response body. Providers answer these requests
/// with JSON documents of at most a few MiB, so a larger body is treated as
/// one that never finished arriving.
pub(super) const MAX_UPSTREAM_RESPONSE_BYTES: usize = 16 * 1024 * 1024;

/// Why an upstream response body could not be used.
pub(super) enum ResponseBodyError {
    /// The transport failed before the body was complete.
    Transport(reqwest::Error),
    /// The body grew past [`MAX_UPSTREAM_RESPONSE_BYTES`].
    TooLarge,
}

impl From<reqwest::Error> for ResponseBodyError {
    fn from(error: reqwest::Error) -> Self {
        Self::Transport(error)
    }
}

impl std::fmt::Display for ResponseBodyError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Transport(error) => write!(f, "{error}"),
            Self::TooLarge => {
                write!(
                    f,
                    "response body exceeded {MAX_UPSTREAM_RESPONSE_BYTES} bytes"
                )
            }
        }
    }
}

impl ResponseBodyError {
    /// Whether the request's own time budget ran out. A body cut off at the
    /// byte cap is not that: the connection was delivering.
    pub(super) fn is_timeout(&self) -> bool {
        matches!(self, Self::Transport(error) if error.is_timeout())
    }
}

/// First boundary for an untrusted provider response body. Callers may inspect
/// the body in memory to classify a retry, but errors, health state, outage
/// aggregation, and tracing receive only this bounded marker.
pub(super) fn redact_provider_response(resp_text: &str) -> String {
    format!("provider response redacted ({} bytes)", resp_text.len())
}

/// `Response::text` with an upper bound: at most
/// [`MAX_UPSTREAM_RESPONSE_BYTES`] are read, then the body is decoded as
/// UTF-8. A body that declares another charset is not re-encoded; every
/// supported provider answers UTF-8 JSON.
pub(super) async fn read_upstream_body(
    mut response: reqwest::Response,
) -> Result<String, ResponseBodyError> {
    let mut body = Vec::new();
    while let Some(chunk) = response.chunk().await? {
        if body.len() + chunk.len() > MAX_UPSTREAM_RESPONSE_BYTES {
            return Err(ResponseBodyError::TooLarge);
        }
        body.extend_from_slice(&chunk);
    }
    let text = String::from_utf8_lossy(&body);
    Ok(text.strip_prefix('\u{feff}').unwrap_or(&text).to_owned())
}
