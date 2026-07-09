use crate::tool_params::TachiMemoryParams;
use serde_json::Value;

use super::types::{RecallSimCase, RecallSimVariant};

pub(super) fn parse_cases(params: &TachiMemoryParams) -> Result<Vec<RecallSimCase>, String> {
    let cases_value = if let Some(value) = params.metadata.as_ref().and_then(extract_cases_value) {
        value
    } else if let Some(text) = params.text.as_deref() {
        parse_text_cases_value(text)?.ok_or_else(|| {
            "recall_simulate text JSON must be an array or an object with cases/eval_cases"
                .to_string()
        })?
    } else {
        return Err(
            "recall_simulate requires metadata.cases, metadata.eval_cases, or text containing JSON cases"
                .to_string()
        );
    };

    let cases: Vec<RecallSimCase> = serde_json::from_value(cases_value)
        .map_err(|e| format!("parse recall_simulate cases: {e}"))?;
    if cases.is_empty() {
        return Err("recall_simulate requires at least one case".to_string());
    }
    for (idx, case) in cases.iter().enumerate() {
        if case.query.trim().is_empty() {
            return Err(format!(
                "recall_simulate case {} has an empty query",
                idx + 1
            ));
        }
    }
    Ok(cases)
}

pub(super) fn parse_variants(params: &TachiMemoryParams) -> Result<Vec<RecallSimVariant>, String> {
    let variants_value =
        if let Some(value) = params.metadata.as_ref().and_then(extract_variants_value) {
            Some(value)
        } else if let Some(text) = params.text.as_deref() {
            parse_text_variants_value(text)?
        } else {
            None
        };

    let Some(variants_value) = variants_value else {
        return Ok(Vec::new());
    };
    let variants: Vec<RecallSimVariant> = serde_json::from_value(variants_value)
        .map_err(|e| format!("parse recall_simulate variants: {e}"))?;
    Ok(variants)
}

fn extract_cases_value(value: &Value) -> Option<Value> {
    match value {
        Value::Array(_) => Some(value.clone()),
        Value::Object(map) => {
            if let Some(cases) = map.get("eval_cases").or_else(|| map.get("cases")) {
                return Some(cases.clone());
            }
            map.get("case")
                .cloned()
                .map(|case| Value::Array(vec![case]))
        }
        _ => None,
    }
}

fn extract_variants_value(value: &Value) -> Option<Value> {
    match value {
        Value::Object(map) => {
            if let Some(variants) = map.get("variants") {
                return Some(variants.clone());
            }
            map.get("variant")
                .cloned()
                .map(|variant| Value::Array(vec![variant]))
        }
        _ => None,
    }
}

fn parse_text_cases_value(text: &str) -> Result<Option<Value>, String> {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return Ok(None);
    }
    let value: Value = serde_json::from_str(trimmed)
        .map_err(|e| format!("parse recall_simulate text JSON: {e}"))?;
    if let Some(cases) = extract_cases_value(&value) {
        return Ok(Some(cases));
    }
    if value.is_array() {
        return Ok(Some(value));
    }
    Ok(None)
}

fn parse_text_variants_value(text: &str) -> Result<Option<Value>, String> {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return Ok(None);
    }
    let value: Value = serde_json::from_str(trimmed)
        .map_err(|e| format!("parse recall_simulate text JSON: {e}"))?;
    Ok(extract_variants_value(&value))
}
