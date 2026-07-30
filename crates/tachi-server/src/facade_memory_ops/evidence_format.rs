//! All formatting and evidence helpers used across facade_memory_ops sub-modules.

use crate::utils::compact_text_line;
use serde_json::{json, Value};

pub(crate) fn wants_full_format(format: Option<&str>) -> bool {
    format
        .map(str::trim)
        .filter(|format| !format.is_empty())
        .is_some_and(|format| format.eq_ignore_ascii_case("full"))
}

pub(crate) fn wants_json(format: Option<&str>) -> bool {
    match format
        .map(str::trim)
        .filter(|format| !format.is_empty())
        .map(str::to_ascii_lowercase)
        .as_deref()
    {
        Some("markdown" | "md" | "text" | "plain" | "human") => false,
        Some("json" | "application/json" | "structured" | "machine") => true,
        Some(_) => false,
        None => true,
    }
}

pub(crate) fn json_string(value: &Value) -> Result<String, String> {
    serde_json::to_string(value).map_err(|e| format!("serialize JSON response: {e}"))
}

pub(crate) fn parse_json_or_empty(raw: String) -> Value {
    serde_json::from_str(&raw).unwrap_or_else(|_| {
        let preview: String = raw.chars().take(500).collect();
        json!({ "raw_preview": preview, "parse_error": true })
    })
}

// `scope`/`scope_warning` (#925) are only ever present on the raw response
// when the effective save/checkpoint scope differed from what was
// requested — keep them through the compact receipt so a silent downgrade
// (e.g. `scope=project` falling back to global on a single-DB daemon) stays
// visible instead of being dropped by this allowlist.
const SAVE_BASE_RECEIPT_KEYS: &[&str] = &[
    "id",
    "path",
    "status",
    "enrichment",
    "scope",
    "scope_warning",
];
/// Per-route identity fields — kept whole when present, never echoed input text.
const SAVE_VARIANT_ROUTE_KEYS: &[&str] =
    &["wiki_path", "note_file", "note_path", "continuity_event"];

pub(crate) fn save_receipt_value(value: &Value) -> Value {
    let mut receipt = serde_json::Map::new();
    receipt.insert("ok".to_string(), json!(true));
    for key in SAVE_BASE_RECEIPT_KEYS
        .iter()
        .chain(SAVE_VARIANT_ROUTE_KEYS.iter())
    {
        if let Some(field) = value.get(*key) {
            if !field.is_null() {
                receipt.insert((*key).to_string(), field.clone());
            }
        }
    }
    if !receipt.contains_key("status") {
        receipt.insert("status".to_string(), json!("saved"));
    }
    Value::Object(receipt)
}

pub(crate) fn shape_save_facade_response(
    raw: &str,
    format: Option<&str>,
    echo: Option<&str>,
    requested_path: Option<&str>,
) -> Result<String, String> {
    let mut value = parse_json_or_empty(raw.to_string());
    if wants_full_format(format) {
        if let (Some(obj), Some(echo_text)) = (
            value.as_object_mut(),
            echo.filter(|text| !text.trim().is_empty()),
        ) {
            obj.insert("echo".to_string(), json!(echo_text));
        }
        return json_string(&value);
    }

    let receipt = save_receipt_value(&value);
    if wants_json(format) {
        json_string(&receipt)
    } else {
        Ok(format_save_result(
            &serde_json::to_string(&receipt).map_err(|e| format!("serialize save receipt: {e}"))?,
            requested_path.or_else(|| receipt.get("path").and_then(Value::as_str)),
        ))
    }
}

fn receipt_eval_entry(value: Option<&Value>) -> Value {
    let Some(value) = value else {
        return Value::Null;
    };
    let mut entry = serde_json::Map::new();
    for key in ["id", "path", "status", "enrichment"] {
        if let Some(field) = value.get(key) {
            if !field.is_null() {
                entry.insert(key.to_string(), field.clone());
            }
        }
    }
    Value::Object(entry)
}

/// Pipeline stages whose value is structured route output, not a single status scalar.
///
/// `precedent_recording` carries `{recorded, skipped}` (#950/#962): without
/// this entry, `pipeline_stage_status`'s generic object handling below drops
/// straight to the `recorded` field and silently discards `skipped`, so a
/// capture-gate rejection under the *default* (non-`full`) receipt format
/// looked identical to a clean run — exactly the silent-data-loss shape this
/// module exists to prevent. `signature_recording` (`signature_evidence.rs`)
/// returns the same `{recorded, skipped}` shape and has the identical latent
/// gap; it is not added here because fixing it is outside this fix's scope,
/// but the same one-line addition is the fix if/when it's picked up.
// `precedent_candidate_decomposition` (#1076, `precedent_candidate_ops.rs`)
// returns the identical `{recorded, skipped}` shape as `precedent_recording`
// for the identical reason — listed here from day one rather than knowingly
// re-introducing the gap the doc comment above flags for `signature_recording`.
const PIPELINE_VARIANT_OBJECT_STAGES: &[&str] = &[
    "pattern_feedback",
    "kanban_update",
    "precedent_recording",
    "precedent_candidate_decomposition",
];

fn pipeline_stage_status(value: &Value) -> Option<Value> {
    match value {
        Value::String(_) | Value::Bool(_) | Value::Number(_) => Some(value.clone()),
        Value::Object(obj) => {
            if let Some(status) = obj.get("status") {
                return Some(status.clone());
            }
            if let Some(recorded) = obj.get("recorded") {
                return Some(recorded.clone());
            }
            None
        }
        _ => None,
    }
}

fn pipeline_stage_value(stage: &str, value: &Value) -> Option<Value> {
    if value.is_null() {
        return None;
    }
    if PIPELINE_VARIANT_OBJECT_STAGES.contains(&stage) {
        return Some(value.clone());
    }
    pipeline_stage_status(value)
}

fn whole_pipeline(value: Option<&Value>) -> Value {
    let Some(map) = value.and_then(Value::as_object) else {
        return Value::Null;
    };
    Value::Object(
        map.iter()
            .filter_map(|(key, value)| {
                pipeline_stage_value(key, value).map(|status| (key.clone(), status))
            })
            .collect(),
    )
}

fn whole_next_steps(value: Option<&Value>) -> Value {
    Value::Array(
        value
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .cloned()
            .collect(),
    )
}

pub(crate) fn shape_complete_response(bundle: Value, format: Option<&str>) -> Value {
    if wants_full_format(format) {
        return bundle;
    }
    let mut receipt = serde_json::Map::new();
    // Successful completions keep the compact receipt's long-standing shape,
    // but a deferred canonical outcome must be unmistakable to callers.
    if bundle.get("recorded").and_then(Value::as_bool) == Some(false) {
        receipt.insert("recorded".to_string(), Value::Bool(false));
    }
    if let Some(count) = bundle.get("subagent_count") {
        receipt.insert("subagent_count".to_string(), count.clone());
    }
    if let Some(pr_ref) = bundle.get("pr_ref") {
        if !pr_ref.is_null() {
            receipt.insert("pr_ref".to_string(), pr_ref.clone());
        }
    }
    // Security signal, not an echo: the caller must see that its metadata
    // contained secret-ish content and was scrubbed.
    if let Some(redactions) = bundle.get("secret_redactions") {
        receipt.insert("secret_redactions".to_string(), redactions.clone());
    }
    receipt.insert(
        "eval_entry".to_string(),
        receipt_eval_entry(bundle.get("eval_entry")),
    );
    receipt.insert(
        "next_steps".to_string(),
        whole_next_steps(bundle.get("next_steps")),
    );
    receipt.insert(
        "pipeline".to_string(),
        whole_pipeline(bundle.get("pipeline")),
    );
    Value::Object(receipt)
}

pub(crate) fn sections_to_evidence(sections: &[(String, Value)]) -> Result<Value, String> {
    let mut rows = Vec::new();
    for (section, value) in sections {
        match value {
            Value::Array(items) => {
                for item in items {
                    let mut row = item.clone();
                    if let Some(obj) = row.as_object_mut() {
                        obj.insert("section".to_string(), json!(section));
                    }
                    rows.push(row);
                }
            }
            Value::String(text) if text.starts_with("Error:") => {
                return Err(format!("search backend ({section}): {text}"));
            }
            Value::String(text) => rows.push(json!({ "section": section, "summary": text })),
            other => rows.push(json!({ "section": section, "value": other })),
        }
    }
    Ok(Value::Array(rows))
}

pub(crate) fn format_save_result(raw: &str, requested_path: Option<&str>) -> String {
    let value = parse_json_or_empty(raw.to_string());
    let status = value
        .get("status")
        .and_then(Value::as_str)
        .unwrap_or("saved");
    let id = value.get("id").and_then(Value::as_str).unwrap_or("?");
    let path = value
        .get("path")
        .and_then(Value::as_str)
        .or(requested_path)
        .unwrap_or("/");
    let enrichment = value
        .get("enrichment")
        .map(|v| format!("; enrichment {v}"))
        .unwrap_or_default();
    let warning = value
        .get("warning")
        .and_then(Value::as_str)
        .map(|w| format!("\nWarning: {w}"))
        .unwrap_or_default();
    // #925: loud, dedicated signal for a silent scope downgrade — kept
    // separate from the generic `warning` above so it survives the compact
    // receipt allowlist and is unambiguous about what changed.
    let scope_warning = value
        .get("scope_warning")
        .and_then(Value::as_str)
        .map(|w| format!("\nScope warning: {w}"))
        .unwrap_or_default();
    format!("Saved -> `{path}` (id: `{id}`, status: {status}{enrichment}){warning}{scope_warning}")
}

pub(crate) fn format_extract_result(raw: &str) -> String {
    let value = parse_json_or_empty(raw.to_string());
    let status = value
        .get("status")
        .and_then(Value::as_str)
        .unwrap_or("completed");
    let extracted = value
        .get("facts_extracted")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    let saved = value
        .get("facts_saved")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    let inserted = value
        .get("facts_inserted")
        .and_then(Value::as_u64)
        .unwrap_or(saved);
    let existing = value
        .get("facts_existing")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    let failed = value
        .get("facts_failed")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    let dropped = value
        .get("facts_dropped")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    let mut out = vec![format!(
        "Extract facts -> {status}; extracted {extracted}, saved {saved}; inserted {inserted}, existing {existing}, failed {failed}, dropped {dropped}"
    )];
    if let Some(facts) = value.get("facts").and_then(Value::as_array) {
        for (idx, fact) in facts.iter().enumerate() {
            let path = fact.get("path").and_then(Value::as_str).unwrap_or("/");
            let text = fact
                .get("summary")
                .or_else(|| fact.get("text"))
                .and_then(Value::as_str)
                .unwrap_or("");
            out.push(format!(
                "{}. `{path}` - {}",
                idx + 1,
                compact_text_line(text, 100)
            ));
        }
    }
    out.join("\n")
}

pub(crate) fn checkpoint_message(
    raw: &str,
    display_path: Option<&str>,
    already_formatted: bool,
    echo: Option<&str>,
    format: Option<&str>,
) -> String {
    if already_formatted {
        raw.to_string()
    } else if wants_full_format(format) {
        let mut msg = format_save_result(raw, display_path);
        if let Some(echo) = echo.filter(|text| !text.trim().is_empty()) {
            msg.push_str(&format!(
                "\nSummary: {}",
                compact_text_line(echo.trim(), 220)
            ));
        }
        msg
    } else {
        format_save_result(raw, display_path)
    }
}

pub(crate) fn checkpoint_saved_payload(raw: &str, already_formatted: bool) -> Value {
    if already_formatted {
        Value::Null
    } else {
        parse_json_or_empty(raw.to_string())
    }
}

pub(crate) fn format_agent_status(
    title: &str,
    fields: &[(&str, String)],
    rows: Option<&Value>,
    synthesis: Option<&str>,
) -> String {
    let mut out = vec![format!("## {title}")];
    for (key, value) in fields {
        out.push(format!("{key}: {value}"));
    }
    if let Some(rows) = rows {
        let evidence = evidence_rows(rows);
        if !evidence.is_empty() {
            out.push("\n### Evidence".to_string());
            for (idx, row) in evidence.into_iter().take(6).enumerate() {
                let topic = row.get("topic").and_then(Value::as_str).unwrap_or("entry");
                let path = row.get("path").and_then(Value::as_str).unwrap_or("/");
                let summary = row
                    .get("summary")
                    .and_then(Value::as_str)
                    .unwrap_or("(no summary)");
                let score = evidence_score(row);
                out.push(format!(
                    "{}. **{}** {:.3} `{}` - {}",
                    idx + 1,
                    topic,
                    score,
                    path,
                    compact_text_line(summary, 100)
                ));
            }
        }
    }
    if let Some(synthesis) = synthesis.filter(|s| !s.trim().is_empty()) {
        out.push("\n### Synthesis".to_string());
        out.push(compact_text_line(synthesis, 600));
    }
    out.join("\n")
}

pub(crate) fn synthesis_markdown_text(value: &Value) -> Option<String> {
    value
        .get("answer")
        .and_then(Value::as_str)
        .map(str::to_string)
        .or_else(|| {
            value.get("error").and_then(Value::as_str).map(|err| {
                let status = value
                    .get("status")
                    .and_then(Value::as_str)
                    .unwrap_or("failed");
                format!("{status}: {err}")
            })
        })
}

pub(crate) fn evidence_rows(value: &Value) -> Vec<&Value> {
    match value {
        Value::Array(rows) => rows.iter().filter(|v| !v.is_null()).collect(),
        Value::Object(map) => map
            .values()
            .flat_map(|value| match value {
                Value::Array(rows) => rows.iter().filter(|v| !v.is_null()).collect::<Vec<_>>(),
                other if !other.is_null() => vec![other],
                _ => vec![],
            })
            .collect(),
        other if !other.is_null() => vec![other],
        _ => vec![],
    }
}

pub(crate) fn evidence_score(row: &Value) -> f64 {
    row.get("relevance")
        .and_then(Value::as_f64)
        .or_else(|| {
            row.get("score")
                .and_then(|score| score.get("final"))
                .and_then(Value::as_f64)
        })
        .or_else(|| row.get("score").and_then(Value::as_f64))
        .unwrap_or(0.0)
}

pub(crate) fn evidence_ref(row: &Value) -> Value {
    let relevance = row
        .get("relevance")
        .and_then(Value::as_f64)
        .unwrap_or_else(|| evidence_score(row));
    json!({
        "id": row.get("id"),
        "path": row.get("path"),
        "summary": row.get("summary"),
        "excerpt": row.get("excerpt"),
        "topic": row.get("topic"),
        "relevance": relevance,
    })
}

pub(crate) fn build_thinking_scaffold(mode: &str, query: &str, evidence: &Value) -> Value {
    let mut rows = evidence_rows(evidence);
    rows.sort_by(|a, b| {
        evidence_score(b)
            .partial_cmp(&evidence_score(a))
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    let evidence_count = rows.len();
    let top_score = rows.first().map(|row| evidence_score(row)).unwrap_or(0.0);
    let confidence = if evidence_count == 0 {
        "none"
    } else if top_score >= 0.75 || evidence_count >= 5 {
        "high"
    } else if top_score >= 0.35 || evidence_count >= 2 {
        "medium"
    } else {
        "low"
    };
    let key_evidence = rows
        .iter()
        .take(3)
        .map(|row| evidence_ref(row))
        .collect::<Vec<_>>();

    let mut gaps = Vec::new();
    if evidence_count == 0 {
        gaps.push("No evidence rows were retrieved.".to_string());
    }
    if top_score < 0.35 && evidence_count > 0 {
        gaps.push("Top evidence relevance is weak; treat conclusions as tentative.".to_string());
    }
    if query.trim().len() < 8 {
        gaps.push("Query is short; refine it with topic, path, or error context.".to_string());
    }

    let next_steps = if mode == "consolidate" {
        vec![
            "Group candidates by topic/path before deciding canonical memories.",
            "Prefer archive/supersede actions over deletion unless data is clearly junk.",
        ]
    } else if evidence_count == 0 {
        vec![
            "Search again with a more specific query or path_prefix.",
            "If this should be known, save a checkpoint before relying on recall.",
        ]
    } else {
        vec![
            "Answer only from key_evidence unless LLM synthesis is explicitly enabled.",
            "Call out uncertainty when gaps is non-empty.",
        ]
    };

    json!({
        "mode": mode,
        "query": query,
        "confidence": confidence,
        "evidence_count": evidence_count,
        "top_relevance": top_score,
        "key_evidence": key_evidence,
        "gaps": gaps,
        "next_steps": next_steps,
    })
}

pub(crate) fn slim_memory_rows(value: Value) -> Value {
    let rows = match value {
        Value::Array(rows) => rows,
        other => vec![other],
    };
    Value::Array(
        rows.into_iter()
            .take(12)
            .map(|row| {
                let mut slim = json!({
                    "id": row.get("id"),
                    "db": row.get("db"),
                    "path": row.get("path"),
                    "summary": row.get("summary"),
                    "topic": row.get("topic"),
                    "relevance": row.get("relevance"),
                });
                if let Some(store) = row.get("store").filter(|value| !value.is_null()) {
                    slim["store"] = store.clone();
                }
                slim
            })
            .collect(),
    )
}

pub(crate) fn slim_kanban(value: Value) -> Value {
    // #925: mark aged WORKING rows so zombie in-flight dispatches are not
    // presented as "current work" without age context.
    const STALE_WORKING_SECS: i64 = 6 * 3600;
    let now = chrono::Utc::now();
    json!({
        "count": value.get("count"),
        "incomplete": value.get("incomplete"),
        "limit_incomplete": value.get("limit_incomplete"),
        "warning": value.get("warning"),
        "incomplete_reasons": value.get("incomplete_reasons"),
        "kanban_fetch_truncated": value.get("kanban_fetch_truncated"),
        "flow_fetch_truncated": value.get("flow_fetch_truncated"),
        "run_fallback_incomplete": value.get("run_fallback_incomplete"),
        "run_scan_truncated": value.get("run_scan_truncated"),
        "run_scan_invalid_entries": value.get("run_scan_invalid_entries"),
        "tasks": value
            .get("tasks")
            .and_then(|v| v.as_array())
            .into_iter()
            .flatten()
            .take(5)
            .map(|task| {
                let state = task.get("state").and_then(Value::as_str).unwrap_or("");
                let updated_at = task.get("updated_at").and_then(Value::as_str);
                let age_secs = updated_at.and_then(|ts| {
                    chrono::DateTime::parse_from_rfc3339(ts)
                        .ok()
                        .map(|dt| (now - dt.with_timezone(&chrono::Utc)).num_seconds().max(0))
                });
                let stale = matches!(state, "TASK_STATE_WORKING" | "working" | "in_progress")
                    && age_secs.is_some_and(|age| age >= STALE_WORKING_SECS);
                json!({
                    "summary": task.get("summary"),
                    "state": task.get("state"),
                    "updated_at": task.get("updated_at"),
                    "age_secs": age_secs,
                    "stale": stale,
                })
            })
            .collect::<Vec<_>>(),
    })
}

pub(crate) fn merge_keywords(mut keywords: Vec<String>, defaults: &[&str]) -> Vec<String> {
    for default in defaults {
        if !keywords.iter().any(|keyword| keyword == default) {
            keywords.push((*default).to_string());
        }
    }
    keywords
}

pub(crate) fn parse_evidence_array(raw: String) -> Value {
    match serde_json::from_str::<Value>(&raw) {
        Ok(Value::Array(rows)) => Value::Array(rows),
        Ok(value) => Value::Array(vec![value]),
        Err(_) => Value::Array(vec![json!({ "raw": raw })]),
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extract_result_keeps_replay_safe_accounting_visible() {
        let formatted = format_extract_result(
            &json!({
                "status": "completed",
                "facts_extracted": 2,
                "facts_saved": 0,
                "facts_inserted": 0,
                "facts_existing": 2,
                "facts_failed": 0,
                "facts_dropped": 0,
                "facts": [],
            })
            .to_string(),
        );

        assert_eq!(
            formatted,
            "Extract facts -> completed; extracted 2, saved 0; inserted 0, existing 2, failed 0, dropped 0"
        );
    }

    #[test]
    fn compact_completion_surfaces_a_pending_canonical_outcome() {
        let receipt = shape_complete_response(
            json!({
                "recorded": false,
                "subagent_count": 0,
                "eval_entry": {"id": "eval-pending"},
                "next_steps": [],
                "pipeline": {
                    "dispatch_outcome": {"recorded": false},
                    "completion_receipt": {"status": "pending_canonical_outcome"}
                },
                "secret_redactions": 0,
            }),
            None,
        );

        assert_eq!(receipt["recorded"], json!(false));
        assert_eq!(
            receipt["pipeline"]["completion_receipt"],
            json!("pending_canonical_outcome")
        );
    }

    #[test]
    fn slim_kanban_marks_stale_working_rows() {
        let old = (chrono::Utc::now() - chrono::Duration::hours(12)).to_rfc3339();
        let fresh = chrono::Utc::now().to_rfc3339();
        let board = json!({
            "count": 2,
            "tasks": [
                {
                    "summary": "zombie dispatch",
                    "state": "TASK_STATE_WORKING",
                    "updated_at": old,
                },
                {
                    "summary": "fresh work",
                    "state": "TASK_STATE_WORKING",
                    "updated_at": fresh,
                }
            ]
        });
        let slim = slim_kanban(board);
        let tasks = slim["tasks"].as_array().expect("tasks");
        assert_eq!(tasks[0]["stale"], json!(true));
        assert_eq!(tasks[1]["stale"], json!(false));
        assert!(tasks[0]["age_secs"]
            .as_i64()
            .is_some_and(|age| age >= 6 * 3600));
    }

    #[test]
    fn slim_kanban_preserves_incomplete_board_evidence() {
        let board = json!({
            "count": 0,
            "tasks": [],
            "incomplete": true,
            "limit_incomplete": true,
            "warning": "bounded fallback is incomplete",
            "incomplete_reasons": ["run_fallback_invalid_entries"],
            "kanban_fetch_truncated": false,
            "flow_fetch_truncated": false,
            "run_fallback_incomplete": true,
            "run_scan_truncated": false,
            "run_scan_invalid_entries": 1,
        });

        let slim = slim_kanban(board);

        assert_eq!(slim["incomplete"], json!(true), "{slim:#}");
        assert_eq!(slim["limit_incomplete"], json!(true), "{slim:#}");
        assert_eq!(
            slim["incomplete_reasons"],
            json!(["run_fallback_invalid_entries"]),
            "{slim:#}"
        );
        assert_eq!(slim["warning"], json!("bounded fallback is incomplete"));
        assert_eq!(slim["run_scan_invalid_entries"], json!(1));
    }

    #[test]
    fn thinking_scaffold_summarizes_key_evidence() {
        let evidence = json!([
            {
                "id": "low",
                "path": "/project/low",
                "summary": "Low relevance",
                "topic": "memory",
                "relevance": 0.2
            },
            {
                "id": "high",
                "path": "/project/high",
                "summary": "High relevance",
                "topic": "memory",
                "relevance": 0.82
            }
        ]);

        let thinking = build_thinking_scaffold("ask", "what happened", &evidence);

        assert_eq!(thinking["confidence"], json!("high"));
        assert_eq!(thinking["evidence_count"], json!(2));
        assert_eq!(thinking["key_evidence"][0]["id"], json!("high"));
        assert!(thinking["gaps"].as_array().unwrap().is_empty());
    }

    #[test]
    fn thinking_scaffold_marks_missing_evidence() {
        let thinking = build_thinking_scaffold("ask", "why", &json!([]));

        assert_eq!(thinking["confidence"], json!("none"));
        assert_eq!(thinking["evidence_count"], json!(0));
        assert!(thinking["gaps"]
            .as_array()
            .unwrap()
            .iter()
            .any(|gap| gap == "No evidence rows were retrieved."));
    }
}
#[test]
fn slim_memory_rows_preserves_typed_wiki_store_identity() {
    let store = json!({"kind": "named_project", "project": "wiki"});
    let rows = slim_memory_rows(json!([{
        "id": "wiki-row",
        "path": "/wiki/example",
        "store": store,
    }]));

    assert_eq!(rows[0]["store"], store);
}
