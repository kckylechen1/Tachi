//! All formatting and evidence helpers used across facade_memory_ops sub-modules.

use crate::utils::compact_text_line;
use serde_json::{json, Value};

pub(crate) fn wants_json(format: Option<&str>) -> bool {
    matches!(
        format
            .map(str::trim)
            .map(str::to_ascii_lowercase)
            .as_deref(),
        Some("json" | "application/json" | "structured")
    )
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
    format!("Saved -> `{path}` (id: `{id}`, status: {status}{enrichment}){warning}")
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
    let mut out = vec![format!(
        "Extract facts -> {status}; extracted {extracted}, saved {saved}"
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
) -> String {
    if already_formatted {
        raw.to_string()
    } else {
        let mut msg = format_save_result(raw, display_path);
        if let Some(echo) = echo.filter(|text| !text.trim().is_empty()) {
            msg.push_str(&format!(
                "\nSummary: {}",
                compact_text_line(echo.trim(), 220)
            ));
        }
        msg
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
                json!({
                    "id": row.get("id"),
                    "db": row.get("db"),
                    "path": row.get("path"),
                    "summary": row.get("summary"),
                    "topic": row.get("topic"),
                    "relevance": row.get("relevance"),
                })
            })
            .collect(),
    )
}

pub(crate) fn slim_kanban(value: Value) -> Value {
    json!({
        "count": value.get("count"),
        "tasks": value
            .get("tasks")
            .and_then(|v| v.as_array())
            .into_iter()
            .flatten()
            .take(5)
            .map(|task| {
                json!({
                    "summary": task.get("summary"),
                    "state": task.get("state"),
                    "updated_at": task.get("updated_at"),
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
