use std::collections::HashMap;

use serde_json::Value;

use crate::llm::LlmClient;

use super::types::GroupPayload;

/// Parse the model's JSON-array response into a map keyed by group_id.
/// Tolerates ```json fences and leading prose.
pub(crate) fn parse_distill_response(raw: &str) -> Result<HashMap<String, GroupPayload>, String> {
    let json_text = LlmClient::extract_json_payload(raw)?;
    let arr: Value = serde_json::from_str(json_text).map_err(|e| {
        format!(
            "invalid distill JSON: {e} (snippet: {})",
            snippet(json_text)
        )
    })?;
    let arr = arr
        .as_array()
        .ok_or_else(|| "distill response must be a JSON array".to_string())?;

    let mut out = HashMap::with_capacity(arr.len());
    for item in arr {
        let Some(group_id) = item.get("group_id").and_then(|v| v.as_str()) else {
            continue;
        };
        let text = item
            .get("text")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .trim()
            .to_string();
        let summary = item
            .get("summary")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .trim()
            .to_string();
        let keywords = item
            .get("keywords")
            .and_then(|v| v.as_array())
            .into_iter()
            .flatten()
            .filter_map(|v| v.as_str())
            .map(|s| s.to_string())
            .collect();
        let skip_reason = item
            .get("skip_reason")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());
        out.insert(
            group_id.to_string(),
            GroupPayload {
                summary,
                text,
                keywords,
                skip_reason,
            },
        );
    }
    Ok(out)
}

fn snippet(s: &str) -> String {
    s.chars().take(200).collect()
}
