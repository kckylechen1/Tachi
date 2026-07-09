use std::collections::HashSet;

use memcore::{MemoryEntry, ProjectionKind};
use serde_json::{json, Value};

use crate::MemoryServer;

use super::context::list_active_patterns;

#[derive(Debug, Clone)]
struct PatternFeedbackRef {
    pattern_ref: String,
    outcome: Option<String>,
    note: Option<String>,
}

pub(crate) fn pattern_ref_json(entry: &MemoryEntry) -> Value {
    json!({
        "id": entry.id,
        "path": entry.path,
        "summary": entry.summary,
        "projection_kind": entry.metadata.get("projection_kind").cloned().unwrap_or(Value::Null),
        "projection_key": entry.metadata.get("projection_key").cloned().unwrap_or(Value::Null),
        "source_event_id": entry.metadata.get("source_event_id").cloned().unwrap_or(Value::Null),
        "counters": entry.metadata.get("counters").cloned().unwrap_or_else(|| json!({})),
    })
}

pub(crate) fn pattern_row_ref_json(row: &Value) -> Value {
    let metadata = row.get("metadata").unwrap_or(&Value::Null);
    json!({
        "id": row.get("id").cloned().unwrap_or(Value::Null),
        "path": row.get("path").cloned().unwrap_or(Value::Null),
        "summary": row.get("summary").cloned().unwrap_or(Value::Null),
        "projection_kind": metadata.get("projection_kind").cloned().unwrap_or(Value::Null),
        "projection_key": metadata.get("projection_key").cloned().unwrap_or(Value::Null),
        "source_event_id": metadata.get("source_event_id").cloned().unwrap_or(Value::Null),
        "counters": metadata.get("counters").cloned().unwrap_or_else(|| json!({})),
    })
}

pub(crate) fn attach_pattern_ref_to_row(row: &mut Value) {
    let pattern_ref = pattern_row_ref_json(row);
    if let Some(object) = row.as_object_mut() {
        object.insert("pattern_ref".to_string(), pattern_ref);
    }
}

fn metadata_string<'a>(value: &'a Value, keys: &[&str]) -> Option<&'a str> {
    keys.iter().find_map(|key| {
        value
            .get(*key)
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|candidate| !candidate.is_empty())
    })
}

fn feedback_ref_from_value(value: &Value) -> Option<PatternFeedbackRef> {
    if let Some(raw) = value.as_str() {
        return feedback_ref_from_str(raw);
    }
    let object = value.as_object()?;
    let pattern_ref = metadata_string(
        value,
        &["pattern_ref", "pattern_id", "id", "projection_key", "path"],
    )?
    .to_string();
    let outcome = metadata_string(value, &["outcome", "feedback", "event"])
        .map(normalize_feedback_outcome)
        .transpose()
        .ok()
        .flatten();
    let note = metadata_string(value, &["note", "summary", "reason"]).map(str::to_string);
    if object.is_empty() {
        return None;
    }
    Some(PatternFeedbackRef {
        pattern_ref,
        outcome,
        note,
    })
}

fn feedback_ref_from_str(raw: &str) -> Option<PatternFeedbackRef> {
    let raw = raw.trim();
    if raw.is_empty() {
        return None;
    }
    let prefixes = [
        ("pattern-hit:", Some("hit")),
        ("pattern-miss:", Some("miss")),
        ("pattern-stale:", Some("stale")),
        ("pattern-seen:", Some("seen")),
        ("pattern:", None),
        ("pattern_ref:", None),
    ];
    for (prefix, outcome) in prefixes {
        if let Some(rest) = raw.strip_prefix(prefix) {
            let rest = rest.trim();
            if !rest.is_empty() {
                return Some(PatternFeedbackRef {
                    pattern_ref: rest.to_string(),
                    outcome: outcome.map(str::to_string),
                    note: None,
                });
            }
        }
    }
    None
}

fn normalize_feedback_outcome(raw: &str) -> Result<String, String> {
    let outcome = raw.trim().to_ascii_lowercase();
    match outcome.as_str() {
        "hit" | "matched" | "useful" | "accepted" => Ok("hit".to_string()),
        "miss" | "wrong" | "rejected" | "not_useful" => Ok("miss".to_string()),
        "stale" | "outdated" => Ok("stale".to_string()),
        "seen" | "shown" | "exposed" => Ok("seen".to_string()),
        _ => Err(format!("invalid pattern feedback outcome '{raw}'")),
    }
}

pub(crate) fn pattern_feedback_refs_from_strings(values: &[String]) -> Vec<Value> {
    values
        .iter()
        .filter_map(|value| feedback_ref_from_str(value))
        .map(|feedback| {
            json!({
                "pattern_ref": feedback.pattern_ref,
                "outcome": feedback.outcome,
                "note": feedback.note,
            })
        })
        .collect()
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

fn resolve_pattern(
    server: &MemoryServer,
    project: Option<&str>,
    pattern_ref: &str,
) -> Result<MemoryEntry, String> {
    let patterns = list_active_patterns(server, project, Some(pattern_ref), 100)?;
    patterns
        .into_iter()
        .find(|entry| ref_matches(entry, pattern_ref))
        .ok_or_else(|| format!("no active pattern matched '{pattern_ref}'"))
}

pub(crate) fn emit_pattern_feedback_for_refs(
    server: &MemoryServer,
    project: Option<&str>,
    refs: &[Value],
    default_outcome: &str,
    query: Option<&str>,
    note: Option<&str>,
    source: &str,
    metadata: Value,
) -> Value {
    let default_outcome = match normalize_feedback_outcome(default_outcome) {
        Ok(outcome) => outcome,
        Err(error) => {
            return json!({
                "status": "failed",
                "error": error,
            });
        }
    };
    let mut seen = HashSet::new();
    let mut saved = Vec::new();
    let mut errors = Vec::new();

    for value in refs {
        let Some(feedback_ref) = feedback_ref_from_value(value) else {
            continue;
        };
        if !seen.insert(feedback_ref.pattern_ref.clone()) {
            continue;
        }
        let outcome = feedback_ref
            .outcome
            .as_deref()
            .unwrap_or(default_outcome.as_str());
        match resolve_pattern(server, project, &feedback_ref.pattern_ref).and_then(|pattern| {
            let projection_key = entry_projection_key(&pattern).ok_or_else(|| {
                format!(
                    "pattern '{}' is missing metadata.projection_key",
                    pattern.id
                )
            })?;
            super::emit::emit_pattern_feedback_event(
                server,
                project,
                &pattern.id,
                projection_key,
                entry_projection(&pattern),
                outcome,
                query,
                feedback_ref.note.as_deref().or(note),
                Some(source),
                Some(metadata.clone()),
            )
        }) {
            Ok(value) => saved.push(value),
            Err(error) => errors.push(json!({
                "pattern_ref": feedback_ref.pattern_ref,
                "outcome": outcome,
                "error": error,
            })),
        }
    }

    json!({
        "status": if errors.is_empty() { "saved" } else { "partial" },
        "requested_count": refs.len(),
        "saved_count": saved.len(),
        "error_count": errors.len(),
        "events": saved,
        "errors": errors,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_pattern_feedback_refs_from_strings() {
        let refs = pattern_feedback_refs_from_strings(&[
            "pattern:abc".to_string(),
            "pattern-hit:def".to_string(),
            "plain-ref".to_string(),
        ]);
        assert_eq!(refs.len(), 2);
        assert_eq!(refs[0]["pattern_ref"], json!("abc"));
        assert_eq!(refs[0]["outcome"], Value::Null);
        assert_eq!(refs[1]["pattern_ref"], json!("def"));
        assert_eq!(refs[1]["outcome"], json!("hit"));
    }
}
