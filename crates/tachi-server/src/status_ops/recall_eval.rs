use std::path::{Path, PathBuf};

use serde_json::{json, Map, Value};

const STATUS_REL_PATH: &[&str] = &["status", "recall_eval.latest.json"];

pub(crate) fn recall_eval_status_path(app_home: &Path) -> PathBuf {
    STATUS_REL_PATH
        .iter()
        .fold(app_home.to_path_buf(), |path, part| path.join(part))
}

pub(crate) fn read_recall_eval_status(app_home: &Path) -> Value {
    let path = recall_eval_status_path(app_home);
    if !path.exists() {
        return json!({
            "status": "not_configured",
            "artifact": path.display().to_string(),
            "operator_command": "tachi eval recall",
        });
    }
    let raw = match std::fs::read_to_string(&path) {
        Ok(raw) => raw,
        Err(err) => {
            return json!({
                "status": "error",
                "artifact": path.display().to_string(),
                "error": format!("read recall eval status: {err}"),
                "operator_command": "tachi eval recall",
            });
        }
    };
    let parsed: Value = match serde_json::from_str(&raw) {
        Ok(parsed) => parsed,
        Err(err) => {
            return json!({
                "status": "error",
                "artifact": path.display().to_string(),
                "error": format!("parse recall eval status: {err}"),
                "operator_command": "tachi eval recall",
            });
        }
    };
    sanitize_recall_eval_status(&parsed, &path)
}

pub(crate) fn recall_eval_warning(status: &Value) -> Option<String> {
    match status.get("status").and_then(Value::as_str) {
        Some("ok") | Some("not_configured") => None,
        Some("failed") => Some("personal recall eval failed its configured gate".to_string()),
        Some("error") => Some("personal recall eval status artifact is unreadable".to_string()),
        Some(other) => Some(format!("personal recall eval status is {other}")),
        None => Some("personal recall eval status artifact is malformed".to_string()),
    }
}

fn sanitize_recall_eval_status(value: &Value, path: &Path) -> Value {
    let Some(object) = value.as_object() else {
        return json!({
            "status": "error",
            "artifact": path.display().to_string(),
            "error": "recall eval status artifact must be a JSON object",
            "operator_command": "tachi eval recall",
        });
    };

    let mut out = Map::new();
    copy_scalar(object, &mut out, "schema_version");
    copy_scalar(object, &mut out, "status");
    copy_scalar(object, &mut out, "generated_at");
    copy_scalar(object, &mut out, "case_count");
    copy_scalar(object, &mut out, "top_k");
    copy_object(object, &mut out, "thresholds");
    copy_object(object, &mut out, "current");
    copy_object(object, &mut out, "per_slice");
    copy_array(object, &mut out, "variants");
    copy_scalar(object, &mut out, "detail");
    out.insert("artifact".to_string(), json!(path.display().to_string()));
    out.insert("operator_command".to_string(), json!("tachi eval recall"));
    Value::Object(out)
}

fn copy_scalar(input: &Map<String, Value>, out: &mut Map<String, Value>, key: &str) {
    if let Some(value) = input.get(key) {
        if value.is_string() || value.is_number() || value.is_boolean() || value.is_null() {
            out.insert(key.to_string(), value.clone());
        }
    }
}

fn copy_object(input: &Map<String, Value>, out: &mut Map<String, Value>, key: &str) {
    if let Some(value) = input.get(key).filter(|value| value.is_object()) {
        out.insert(key.to_string(), value.clone());
    }
}

fn copy_array(input: &Map<String, Value>, out: &mut Map<String, Value>, key: &str) {
    if let Some(value) = input.get(key).filter(|value| value.is_array()) {
        out.insert(key.to_string(), value.clone());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn status_sanitizer_drops_raw_case_fields() {
        let status = sanitize_recall_eval_status(
            &json!({
                "schema_version": "tachi.recall_eval.status.v1",
                "status": "failed",
                "generated_at": "2026-07-09T00:00:00Z",
                "case_count": 2,
                "top_k": 10,
                "thresholds": {"min_recall": 1.0},
                "current": {"recall_at_k": 0.5, "mrr": 0.5},
                "per_slice": {"summary": {"n": 2, "hits": 1}},
                "variants": [{"name": "current", "recall_at_k": 0.5}],
                "query": "private query",
                "expected_ids": ["private-id"],
                "cases": [{"query": "private query"}]
            }),
            Path::new("/tmp/recall_eval.latest.json"),
        );

        let rendered = serde_json::to_string(&status).expect("status JSON");
        assert!(!rendered.contains("private query"));
        assert!(!rendered.contains("private-id"));
        assert_eq!(status["status"], json!("failed"));
        assert_eq!(
            recall_eval_warning(&status).as_deref(),
            Some("personal recall eval failed its configured gate")
        );
    }
}
