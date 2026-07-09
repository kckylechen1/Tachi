use memcore::{MemoryEntry, ProjectionKind};
use serde_json::Value;

use crate::tool_params::TachiMemoryParams;
use crate::MemoryServer;

use super::{json_string, wants_json};

fn metadata_string<'a>(metadata: &'a Value, keys: &[&str]) -> Option<&'a str> {
    keys.iter().find_map(|key| {
        metadata
            .get(*key)
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
    })
}

fn params_pattern_ref(params: &TachiMemoryParams) -> Option<String> {
    params
        .id
        .as_deref()
        .or(params.path.as_deref())
        .or_else(|| {
            params.metadata.as_ref().and_then(|metadata| {
                metadata_string(
                    metadata,
                    &[
                        "pattern_id",
                        "pattern_ref",
                        "pattern_path",
                        "projection_key",
                    ],
                )
            })
        })
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
}

fn entry_projection_key(entry: &MemoryEntry) -> Option<&str> {
    entry
        .metadata
        .get("projection_key")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
}

fn entry_projection(entry: &MemoryEntry) -> ProjectionKind {
    match entry
        .metadata
        .get("projection_kind")
        .and_then(Value::as_str)
        .map(str::trim)
    {
        Some("bonding") => ProjectionKind::Bonding,
        _ => ProjectionKind::Pattern,
    }
}

fn ref_matches(entry: &MemoryEntry, pattern_ref: &str) -> bool {
    entry.id == pattern_ref
        || entry.path == pattern_ref
        || entry_projection_key(entry) == Some(pattern_ref)
}

fn normalize_outcome(raw: Option<&str>) -> Result<String, String> {
    let Some(raw) = raw.map(str::trim).filter(|value| !value.is_empty()) else {
        return Err(
            "event is required when action='pattern_feedback' (hit, miss, stale, or seen)"
                .to_string(),
        );
    };
    let outcome = raw.to_ascii_lowercase();
    match outcome.as_str() {
        "hit" | "matched" | "useful" | "accepted" => Ok("hit".to_string()),
        "miss" | "wrong" | "rejected" | "not_useful" => Ok("miss".to_string()),
        "stale" | "outdated" => Ok("stale".to_string()),
        "seen" | "shown" | "exposed" => Ok("seen".to_string()),
        _ => Err(format!(
            "invalid pattern feedback event '{raw}'; expected hit, miss, stale, or seen"
        )),
    }
}

fn resolve_pattern(
    server: &MemoryServer,
    params: &TachiMemoryParams,
    pattern_ref: &str,
) -> Result<MemoryEntry, String> {
    let patterns = crate::continuity_ops::list_active_patterns(
        server,
        params.project.as_deref(),
        Some(pattern_ref),
        100,
    )?;
    patterns
        .into_iter()
        .find(|entry| ref_matches(entry, pattern_ref))
        .ok_or_else(|| format!("no active pattern matched '{pattern_ref}'"))
}

pub(crate) fn handle_pattern_feedback(
    server: &MemoryServer,
    params: &TachiMemoryParams,
) -> Result<String, String> {
    let pattern_ref = params_pattern_ref(params)
        .ok_or_else(|| "id or metadata.pattern_ref is required for pattern_feedback".to_string())?;
    let outcome = normalize_outcome(params.event.as_deref())?;
    let pattern = resolve_pattern(server, params, &pattern_ref)?;
    let projection_key = entry_projection_key(&pattern).ok_or_else(|| {
        format!(
            "pattern '{}' is missing metadata.projection_key",
            pattern.id
        )
    })?;
    let value = crate::continuity_ops::emit_pattern_feedback_event(
        server,
        params.project.as_deref(),
        &pattern.id,
        projection_key,
        entry_projection(&pattern),
        &outcome,
        params.query.as_deref(),
        params.summary.as_deref().or(params.text.as_deref()),
        params.source.as_deref(),
        params.metadata.clone(),
    )?;

    if wants_json(params.format.as_deref()) {
        return json_string(&value);
    }
    Ok(format!(
        "pattern_feedback saved: pattern={} outcome={} event_id={}",
        value["pattern_id"].as_str().unwrap_or(pattern.id.as_str()),
        value["outcome"].as_str().unwrap_or(outcome.as_str()),
        value["event_id"].as_str().unwrap_or("")
    ))
}

#[cfg(test)]
mod tests {
    use super::normalize_outcome;

    #[test]
    fn normalizes_feedback_aliases() {
        assert_eq!(normalize_outcome(Some("useful")).unwrap(), "hit");
        assert_eq!(normalize_outcome(Some("not_useful")).unwrap(), "miss");
        assert_eq!(normalize_outcome(Some("outdated")).unwrap(), "stale");
        assert!(normalize_outcome(Some("maybe")).is_err());
    }
}
