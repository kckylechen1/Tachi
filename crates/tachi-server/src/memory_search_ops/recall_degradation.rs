// recall_degradation.rs — observable recall degradation markers (#926 / #724)
//
// When a recall-path provider call fails or times out, search falls open
// (lexical-only for embed failure; hybrid ranking for rerank failure). Before
// #926 the only trace was an `eprintln!`. This module extends the existing
// per-row `recall_quality` degraded-field mechanism (see `search_memory/rows.rs`)
// with machine-readable markers:
//
//   - `"degraded": "lexical_only: <reason>"`  — query embedding failed (#926)
//   - `"degraded": "rerank_fallback: <reason>"` — runtime rerank failed (#724)
//
// Priority when both could apply on one row: **lexical_only wins**. Missing
// semantic recall is the stronger degradation signal; a subsequent rerank
// fall-open must not overwrite it.

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

/// Shared merge for a machine-readable `degraded` marker into `recall_quality`.
///
/// Vector-coverage semantics are preserved: an existing coverage object keeps
/// its `status`/`reason`/`vector_coverage` fields and gains the `degraded`
/// marker. When an embed-failure (`lexical_only:…`) marker is already present,
/// it is kept — rerank_fallback must not overwrite it (see module docs).
fn merge_degraded_marker(existing: Option<Value>, marker: String) -> Value {
    match existing {
        Some(Value::Object(mut map)) => {
            // Embed-failure (lexical_only) wins over a later rerank_fallback.
            if let Some(Value::String(existing_marker)) = map.get("degraded") {
                if existing_marker.starts_with("lexical_only") {
                    map.insert("status".to_string(), json!("degraded"));
                    return Value::Object(map);
                }
            }
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

/// Merge the `lexical_only` provider-failure marker into a `recall_quality`
/// object. When both a coverage degrade and a provider failure apply, the
/// provider-failure reason wins as the machine-readable `degraded` value
/// (the coverage detail remains under `reason`).
pub(crate) fn merge_lexical_only_marker(existing: Option<Value>, reason: &str) -> Value {
    let marker = format!("lexical_only: {reason}");
    // lexical_only is the stronger signal: always write it (even if a weaker
    // rerank_fallback marker were already present).
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

/// Merge the `rerank_fallback` runtime-failure marker into a `recall_quality`
/// object. Does not overwrite an existing `lexical_only` marker.
pub(crate) fn merge_rerank_fallback_marker(existing: Option<Value>, reason: &str) -> Value {
    merge_degraded_marker(existing, format!("rerank_fallback: {reason}"))
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

/// Attach a machine-readable `recall_quality.degraded = "rerank_fallback: …"`
/// marker when runtime rerank fails and search falls open to hybrid ranking.
///
/// Keeps the graceful fallback; makes the degradation observable. Rides the
/// same per-row `recall_quality` plumbing as #926's lexical-only marker.
/// If a row already carries `lexical_only`, that marker is preserved.
pub(crate) fn attach_rerank_fallback_degraded(rows: &mut [Value], err: &str) {
    let reason = short_reason(err);
    for row in rows {
        if let Some(obj) = row.as_object_mut() {
            let existing = obj.remove("recall_quality");
            obj.insert(
                "recall_quality".to_string(),
                merge_rerank_fallback_marker(existing, &reason),
            );
        }
    }
}
