// recall_degradation.rs — observable "lexical-only" degradation marker (#926)
//
// When a recall-path provider call (query embedding) fails or times out, the
// search silently falls back to lexical/FTS-only. Before #926 the only trace
// was an `eprintln!`. This module extends the existing per-row `recall_quality`
// degraded-field mechanism (see `search_memory/rows.rs`) with a machine-
// readable `"degraded": "lexical_only: <reason>"` marker so callers can tell a
// full semantic recall from a lexical-only fallback.

use serde_json::{json, Value};

/// Condense a provider error into a short, single-line reason for the marker.
/// Keeps the marker stable/bounded regardless of the underlying error verbosity.
pub(crate) fn short_reason(err: &str) -> String {
    const MAX: usize = 120;
    let line = err.lines().next().unwrap_or(err).trim();
    if line.chars().count() > MAX {
        let truncated: String = line.chars().take(MAX).collect();
        format!("{truncated}…")
    } else {
        line.to_string()
    }
}

/// Merge the `lexical_only` provider-failure marker into a `recall_quality`
/// object. Vector-coverage semantics are preserved: an existing coverage
/// object keeps its `status`/`reason`/`vector_coverage` fields and gains the
/// `degraded` marker. When both a coverage degrade and a provider failure
/// apply, the provider-failure reason wins as the machine-readable `degraded`
/// value (the coverage detail remains under `reason`). When no coverage object
/// exists, a minimal degraded object is created.
pub(crate) fn merge_lexical_only_marker(existing: Option<Value>, reason: &str) -> Value {
    let marker = format!("lexical_only: {reason}");
    match existing {
        Some(Value::Object(mut map)) => {
            map.insert("status".to_string(), json!("degraded"));
            map.insert("degraded".to_string(), json!(marker));
            Value::Object(map)
        }
        _ => json!({
            "status": "degraded",
            "degraded": marker,
        }),
    }
}

/// Attach the `lexical_only` marker to a row that has no pre-computed
/// `recall_quality` (e.g. the path-DB recall-cache fallback). Merges with any
/// existing `recall_quality` object already on the row.
pub(crate) fn attach_lexical_only_marker(row: &mut Value, reason: &str) {
    if let Some(obj) = row.as_object_mut() {
        let existing = obj.remove("recall_quality");
        obj.insert(
            "recall_quality".to_string(),
            merge_lexical_only_marker(existing, reason),
        );
    }
}
