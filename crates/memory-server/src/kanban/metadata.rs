use super::*;

pub(super) fn add_trimmed_str_to_metadata(
    metadata: &mut serde_json::Value,
    key: &str,
    value: &Option<String>,
) {
    if let Some(value) = value
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        metadata[key] = json!(value);
    }
}
