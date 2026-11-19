//! Read-only validation for reports written before automatic daily advice retired.

use serde::Deserialize;
use serde_json::Value;
use std::path::{Path, PathBuf};
use tachi_llm::PersistedModelInvocationReceiptV1;

const DAILY_MODEL_INVOCATIONS_SCHEMA_V1: &str = "daily-model-invocations-v1";

fn json_section(markdown: &str, heading: &str) -> Option<String> {
    let marker = format!("## {heading}\n\n```json\n");
    let start = markdown.find(&marker)? + marker.len();
    let end = markdown[start..].find("\n```\n")? + start;
    Some(markdown[start..end].to_string())
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct PersistedModelInvocationReceiptV1Wire {
    schema: String,
    lane: String,
    engine_kind: String,
    effective_provider: Option<String>,
    effective_model: Option<String>,
    effective_version: Option<String>,
    fallback_chain: Vec<String>,
    degraded: bool,
    completion_status: String,
    prompt_tokens: Option<u64>,
    completion_tokens: Option<u64>,
    total_tokens: Option<u64>,
    latency_ms: Option<u64>,
    content_hash: String,
    memory_id: String,
    revision: i64,
}

fn receipt_binding_matches(
    value: Option<&Value>,
    content: &str,
    memory_id: &str,
    revision: i64,
) -> bool {
    let Some(value) = value else {
        return false;
    };
    let Some(object) = value.as_object() else {
        return false;
    };
    for key in [
        "schema",
        "lane",
        "engine_kind",
        "effective_provider",
        "effective_model",
        "effective_version",
        "fallback_chain",
        "degraded",
        "completion_status",
        "prompt_tokens",
        "completion_tokens",
        "total_tokens",
        "latency_ms",
        "content_hash",
        "memory_id",
        "revision",
    ] {
        if !object.contains_key(key) {
            return false;
        }
    }
    let Ok(receipt) =
        serde_json::from_value::<PersistedModelInvocationReceiptV1Wire>(value.clone())
    else {
        return false;
    };
    // These fields are intentionally validated by deserialization even though
    // the daily reader does not interpret their values beyond their wire type.
    let _ = (
        receipt.degraded,
        receipt.prompt_tokens,
        receipt.completion_tokens,
        receipt.total_tokens,
        receipt.latency_ms,
    );
    receipt.schema == tachi_llm::MODEL_INVOCATION_SCHEMA_V1
        && receipt.lane == "reasoning"
        && matches!(receipt.engine_kind.as_str(), "provider_http" | "claude_cli")
        && [
            &receipt.effective_provider,
            &receipt.effective_model,
            &receipt.effective_version,
        ]
        .into_iter()
        .flatten()
        .all(|identity| !identity.trim().is_empty())
        && receipt.fallback_chain.len() <= 4
        && receipt.fallback_chain.iter().all(|step| {
            matches!(
                step.as_str(),
                "provider_http_fallback" | "claude_cli_to_provider_http"
            )
        })
        && matches!(receipt.completion_status.as_str(), "complete" | "unknown")
        && receipt.content_hash == PersistedModelInvocationReceiptV1::content_hash_for(content)
        && receipt.memory_id == memory_id
        && receipt.revision == revision
}

fn validated_payload_from_sidecar(sidecar_path: &Path) -> Option<(String, i64, PathBuf)> {
    let raw = std::fs::read_to_string(sidecar_path).ok()?;
    let sidecar: Value = serde_json::from_str(&raw).ok()?;
    let schema = sidecar.get("schema").and_then(Value::as_str)?;
    let date = sidecar.get("date").and_then(Value::as_str)?;
    let revision = sidecar.get("revision").and_then(Value::as_i64)?;
    let payload_basename = sidecar.get("payload_basename").and_then(Value::as_str)?;
    if schema != DAILY_MODEL_INVOCATIONS_SCHEMA_V1 || revision < 1 {
        return None;
    }
    let expected = format!("{date}.r{revision}.md");
    if payload_basename != expected
        || Path::new(payload_basename)
            .file_name()
            .and_then(|v| v.to_str())
            != Some(payload_basename)
    {
        return None;
    }
    let parent = sidecar_path.parent()?;
    let payload_path = parent.join(payload_basename);
    if !payload_path.is_file() {
        return None;
    }
    let markdown = std::fs::read_to_string(&payload_path).ok()?;
    if sidecar.get("report_content_hash").and_then(Value::as_str)
        != Some(PersistedModelInvocationReceiptV1::content_hash_for(&markdown).as_str())
    {
        return None;
    }
    let health = json_section(&markdown, "Health Check")?;
    let health_value = sidecar.get("health")?;
    if !receipt_binding_matches(
        Some(health_value),
        &health,
        &format!("daily-report:{date}:health"),
        revision,
    ) {
        return None;
    }
    let routing_section = json_section(&markdown, "Routing Analysis")?;
    let routing_details: Value = serde_json::from_str(&routing_section).ok()?;
    let routing = sidecar.get("routing")?;
    let is_non_model_skip =
        routing_details.get("disposition").and_then(Value::as_str) == Some("non_model_skip");
    match (is_non_model_skip, routing.is_null()) {
        (true, true) => {}
        (false, false) => {
            if !receipt_binding_matches(
                Some(routing),
                &routing_section,
                &format!("daily-report:{date}:routing"),
                revision,
            ) {
                return None;
            }
        }
        // A skip cannot carry a model receipt, and model-derived content
        // cannot omit one. Either mismatch makes the generation invalid.
        _ => return None,
    }
    Some((date.to_string(), revision, payload_path))
}

pub(crate) fn validated_latest_daily_report(app_home: &Path) -> Option<String> {
    let reports_dir = app_home.join("reports").join("daily");
    let entries = std::fs::read_dir(&reports_dir).ok()?;
    let mut sidecar_dates = std::collections::HashSet::new();
    let mut candidates = Vec::new();
    let mut legacy = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        let name = entry.file_name();
        let Some(name) = name.to_str() else { continue };
        if let Some(generation) = name.strip_suffix(".model-invocations-v1.json") {
            let Some((date, _revision)) =
                generation.rsplit_once(".r").and_then(|(date, revision)| {
                    revision
                        .parse::<i64>()
                        .ok()
                        .map(|revision| (date, revision))
                })
            else {
                continue;
            };
            sidecar_dates.insert(date.to_string());
            if let Some((date, revision, payload)) = validated_payload_from_sidecar(&path) {
                candidates.push((date, revision, payload));
            }
        } else if let Some(date) = name.strip_suffix(".md") {
            if !date.contains(".r") && path.is_file() {
                legacy.push((date.to_string(), 0_i64, path));
            }
        }
    }
    candidates.extend(
        legacy
            .into_iter()
            .filter(|(date, _, _)| !sidecar_dates.contains(date)),
    );
    candidates.sort_by(|a, b| a.0.cmp(&b.0).then(a.1.cmp(&b.1)));
    candidates
        .pop()
        .map(|(_, _, path)| path.display().to_string())
}
