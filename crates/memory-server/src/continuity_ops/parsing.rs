use memory_core::{
    ContinuityCandidate, ContinuityCandidateBatch, ContinuityOutcomeLabel, OutcomeEvidenceBasis,
    ProjectionKind, SessionOutcomeKind,
};
use serde_json::{json, Value};

fn string_vec(value: Option<&Value>) -> Vec<String> {
    match value {
        Some(Value::Array(items)) => items
            .iter()
            .filter_map(|item| item.as_str())
            .map(str::trim)
            .filter(|item| !item.is_empty())
            .map(str::to_string)
            .collect(),
        Some(Value::String(item)) if !item.trim().is_empty() => vec![item.trim().to_string()],
        _ => Vec::new(),
    }
}

fn string_field<'a>(value: &'a Value, keys: &[&str]) -> Option<&'a str> {
    keys.iter()
        .find_map(|key| value.get(*key).and_then(|field| field.as_str()))
        .map(str::trim)
        .filter(|field| !field.is_empty())
}

fn confidence_field(value: &Value) -> Option<f64> {
    value
        .get("confidence")
        .or_else(|| value.get("score"))
        .and_then(|field| field.as_f64())
        .map(|value| value.clamp(0.0, 1.0))
}

pub(crate) fn parse_continuity_candidate_batch(
    raw: &str,
) -> Result<ContinuityCandidateBatch, String> {
    let json_str = tachi_llm::LlmClient::extract_json_payload(raw)?;
    let value: Value =
        serde_json::from_str(json_str).map_err(|e| format!("parse continuity JSON: {e}"))?;
    let candidate_values = if let Some(items) = value.get("candidates").and_then(|v| v.as_array()) {
        items.clone()
    } else if let Some(items) = value.as_array() {
        items.clone()
    } else {
        return Err(
            "continuity distill response must be an object with candidates or an array".into(),
        );
    };

    let mut candidates = Vec::new();
    for item in candidate_values {
        let projection_raw = string_field(&item, &["projection", "projection_kind", "kind"])
            .ok_or_else(|| "continuity candidate missing projection".to_string())?;
        let projection = ProjectionKind::from_str_opt(Some(projection_raw))
            .ok_or_else(|| format!("invalid continuity projection: {projection_raw}"))?;
        let summary = string_field(&item, &["summary", "title"])
            .unwrap_or_default()
            .to_string();
        let text = string_field(&item, &["text", "body", "detail"])
            .unwrap_or_default()
            .to_string();
        if summary.is_empty() && text.is_empty() {
            continue;
        }
        candidates.push(ContinuityCandidate {
            projection,
            event_type: string_field(&item, &["event_type"]).map(str::to_string),
            summary,
            text,
            confidence: confidence_field(&item),
            evidence_refs: string_vec(item.get("evidence_refs").or_else(|| item.get("evidence"))),
            metadata: item.get("metadata").cloned().unwrap_or_else(|| json!({})),
        });
    }

    Ok(ContinuityCandidateBatch {
        candidates,
        open_threads: string_vec(value.get("open_threads")),
    })
}

pub(crate) fn parse_continuity_outcome_label(raw: &str) -> Result<ContinuityOutcomeLabel, String> {
    let json_str = tachi_llm::LlmClient::extract_json_payload(raw)?;
    let value: Value =
        serde_json::from_str(json_str).map_err(|e| format!("parse outcome label JSON: {e}"))?;
    let outcome = SessionOutcomeKind::from_str_opt(string_field(
        &value,
        &["outcome", "outcome_label", "label"],
    ));
    let evidence_basis = OutcomeEvidenceBasis::from_str_opt(string_field(
        &value,
        &[
            "evidence_basis",
            "basis",
            "label_basis",
            "adversarial_basis",
        ],
    ));
    Ok(ContinuityOutcomeLabel {
        outcome,
        evidence_basis,
        confidence: confidence_field(&value),
        rationale: string_field(&value, &["rationale", "reason"])
            .unwrap_or_default()
            .to_string(),
        evidence_refs: string_vec(value.get("evidence_refs").or_else(|| value.get("evidence"))),
        claims: string_vec(value.get("claims")),
        open_questions: string_vec(value.get("open_questions")),
    })
}
