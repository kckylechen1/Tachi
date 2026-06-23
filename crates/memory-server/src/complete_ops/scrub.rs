pub(super) fn scrub_eval_string(text: &str, redactions: &mut usize) -> String {
    let (safe, count) = crate::memory_search_ops::scrub_secrets(text);
    *redactions += count;
    safe
}

fn eval_json_key_is_secretish(key: &str) -> bool {
    let key = key.to_ascii_lowercase();
    key.contains("api_key")
        || key.contains("apikey")
        || key.contains("token")
        || key.contains("secret")
        || key.contains("password")
        || key.contains("authorization")
}

fn scrub_eval_json_value(
    key: Option<&str>,
    value: serde_json::Value,
    redactions: &mut usize,
) -> serde_json::Value {
    match value {
        serde_json::Value::String(text) => {
            let safe = scrub_eval_string(&text, redactions);
            if safe != text {
                serde_json::Value::String(safe)
            } else if key.is_some_and(eval_json_key_is_secretish) && !text.trim().is_empty() {
                *redactions += 1;
                serde_json::Value::String("[REDACTED]".to_string())
            } else {
                serde_json::Value::String(text)
            }
        }
        serde_json::Value::Array(items) => serde_json::Value::Array(
            items
                .into_iter()
                .map(|item| scrub_eval_json_value(None, item, redactions))
                .collect(),
        ),
        serde_json::Value::Object(map) => serde_json::Value::Object(
            map.into_iter()
                .map(|(key, value)| {
                    let value = scrub_eval_json_value(Some(&key), value, redactions);
                    (key, value)
                })
                .collect(),
        ),
        other => other,
    }
}

pub(super) fn scrub_eval_json(
    value: serde_json::Value,
    redactions: &mut usize,
) -> serde_json::Value {
    scrub_eval_json_value(None, value, redactions)
}

pub(super) fn scrub_eval_strings(values: &[String], redactions: &mut usize) -> Vec<String> {
    values
        .iter()
        .map(|value| scrub_eval_string(value, redactions))
        .collect()
}
