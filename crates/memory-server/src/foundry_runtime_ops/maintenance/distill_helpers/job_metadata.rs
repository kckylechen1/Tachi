pub(in crate::foundry_runtime_ops) fn job_metadata_value<'a>(
    metadata: &'a serde_json::Value,
    key: &str,
) -> Option<&'a serde_json::Value> {
    metadata
        .get("job")
        .and_then(|job| job.get(key))
        .or_else(|| metadata.get(key))
}

pub(in crate::foundry_runtime_ops) fn job_metadata_usize(
    metadata: &serde_json::Value,
    key: &str,
    default: usize,
) -> usize {
    job_metadata_value(metadata, key)
        .and_then(|value| value.as_u64())
        .map(|value| value as usize)
        .unwrap_or(default)
}

pub(in crate::foundry_runtime_ops) fn job_metadata_string(
    metadata: &serde_json::Value,
    key: &str,
) -> Option<String> {
    job_metadata_value(metadata, key)
        .and_then(|value| value.as_str())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
}
