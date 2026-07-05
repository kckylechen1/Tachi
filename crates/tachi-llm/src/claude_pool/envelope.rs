use super::*;

/// Extract the `result` field from Claude CLI's JSON envelope. Tolerates
/// either a JSON object or raw text falling back to the whole stdout.
pub(super) fn parse_claude_json_envelope(stdout: &str) -> Result<String, String> {
    let trimmed = stdout.trim();
    if trimmed.is_empty() {
        return Err("claude cli returned empty stdout".to_string());
    }
    if let Ok(value) = serde_json::from_str::<Value>(trimmed) {
        if let Some(s) = value.get("result").and_then(|v| v.as_str()) {
            return Ok(s.to_string());
        }
        // Some claude CLI versions wrap text in `content` arrays.
        if let Some(s) = value.get("text").and_then(|v| v.as_str()) {
            return Ok(s.to_string());
        }
        if let Some(arr) = value.get("content").and_then(|v| v.as_array()) {
            let mut buf = String::new();
            for item in arr {
                if let Some(s) = item.get("text").and_then(|v| v.as_str()) {
                    buf.push_str(s);
                }
            }
            if !buf.is_empty() {
                return Ok(buf);
            }
        }
    }
    // Non-JSON output: hand back as-is (caller will reject empty later).
    Ok(trimmed.to_string())
}
