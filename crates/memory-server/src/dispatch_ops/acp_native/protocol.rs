use serde_json::{json, Value};

pub(super) fn is_json_rpc_request(message: &Value) -> bool {
    message.get("jsonrpc").and_then(Value::as_str) == Some("2.0")
        && message.get("method").and_then(Value::as_str).is_some()
        && message.get("id").is_some()
}

pub(super) fn is_json_rpc_notification(message: &Value) -> bool {
    message.get("jsonrpc").and_then(Value::as_str) == Some("2.0")
        && message.get("method").and_then(Value::as_str).is_some()
        && message.get("id").is_none()
}

pub(super) fn is_session_update(message: &Value) -> bool {
    message.get("method").and_then(Value::as_str) == Some("session/update")
}

pub(super) fn response_id_matches(message: &Value, expected: &str) -> bool {
    message.get("jsonrpc").and_then(Value::as_str) == Some("2.0")
        && message.get("method").is_none()
        && message.get("id").and_then(Value::as_str) == Some(expected)
        && (message.get("result").is_some() || message.get("error").is_some())
}

pub(super) fn extract_session_id(result: &Value) -> Option<String> {
    result
        .get("sessionId")
        .or_else(|| result.get("session_id"))
        .and_then(Value::as_str)
        .map(str::to_string)
}

pub(super) fn ensure_session_id(mut result: Value, session_id: &str) -> Value {
    if extract_session_id(&result).is_some() {
        return result;
    }
    if let Some(object) = result.as_object_mut() {
        object.insert(
            "sessionId".to_string(),
            Value::String(session_id.to_string()),
        );
    } else {
        result = json!({ "sessionId": session_id });
    }
    result
}

pub(super) fn extract_agent_session_id(result: &Value) -> Option<String> {
    let meta = result.get("_meta")?;
    [
        "agentSessionId",
        "agent_session_id",
        "runtimeSessionId",
        "sessionId",
    ]
    .iter()
    .find_map(|key| meta.get(*key).and_then(Value::as_str).map(str::to_string))
}

pub(super) fn extract_update_text(value: &Value) -> Option<String> {
    if let Some(content) = value.get("content") {
        if let Some(text) = content.get("text").and_then(Value::as_str) {
            return Some(text.to_string());
        }
        if let Some(items) = content.as_array() {
            let text = items
                .iter()
                .filter_map(|item| item.get("text").and_then(Value::as_str))
                .collect::<String>();
            if !text.trim().is_empty() {
                return Some(text);
            }
        }
    }
    extract_text_recursive(value)
}

pub(super) fn extract_text_recursive(value: &Value) -> Option<String> {
    match value {
        Value::String(text) if !text.trim().is_empty() => Some(text.to_string()),
        Value::Array(items) => {
            let text = items
                .iter()
                .filter_map(extract_text_recursive)
                .collect::<String>();
            if text.trim().is_empty() {
                None
            } else {
                Some(text)
            }
        }
        Value::Object(object) => {
            for key in ["final_response", "message", "content", "text", "output"] {
                if let Some(found) = object.get(key).and_then(extract_text_recursive) {
                    return Some(found);
                }
            }
            None
        }
        _ => None,
    }
}

pub(super) fn format_json_rpc_error(error: &Value) -> String {
    let message = error
        .get("message")
        .and_then(Value::as_str)
        .unwrap_or("unknown JSON-RPC error");
    let code = error.get("code").and_then(Value::as_i64);
    match code {
        Some(code) => format!("{message} (code {code})"),
        None => message.to_string(),
    }
}

pub(super) fn compact_json(value: &Value) -> String {
    serde_json::to_string(value).unwrap_or_else(|_| "<unserializable>".to_string())
}
