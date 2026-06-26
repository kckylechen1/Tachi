//! Labeled recall replay for `tachi_memory(action="recall_simulate")`.

use super::evidence_format::{json_string, wants_json};
use crate::memory_search_ops::search_memory_rows_with_access;
use crate::tool_params::*;
use crate::MemoryServer;
use serde::Deserialize;
use serde_json::{json, Value};

#[derive(Debug, Clone, Deserialize)]
struct RecallSimCase {
    #[serde(default)]
    name: Option<String>,
    query: String,
    #[serde(default)]
    expected_id: Option<String>,
    #[serde(default)]
    expected_ids: Vec<String>,
    #[serde(default)]
    top_k: Option<usize>,
    #[serde(default)]
    scope: Option<String>,
    #[serde(default)]
    project: Option<String>,
    #[serde(default)]
    domain: Option<String>,
    #[serde(default)]
    path_prefix: Option<String>,
    #[serde(default)]
    include_archived: Option<bool>,
    #[serde(default)]
    include_training: Option<bool>,
    #[serde(default)]
    as_of: Option<String>,
}

impl RecallSimCase {
    fn expected_ids(&self) -> Vec<String> {
        let mut expected = self.expected_ids.clone();
        if let Some(id) = self.expected_id.as_deref().map(str::trim) {
            if !id.is_empty() && !expected.iter().any(|candidate| candidate == id) {
                expected.push(id.to_string());
            }
        }
        expected
    }
}

pub(crate) async fn handle_memory_recall_simulate(
    server: &MemoryServer,
    params: &TachiMemoryParams,
) -> Result<String, String> {
    if params.enable_rerank {
        return Err(
            "recall_simulate currently replays the deterministic hybrid search path; omit enable_rerank or set it false."
                .to_string(),
        );
    }

    let cases = parse_cases(params)?;
    let default_top_k = crate::clamp_facade_top_k(params.top_k);
    let mut reports = Vec::with_capacity(cases.len());
    let mut hit_count = 0usize;
    let mut reciprocal_sum = 0.0f64;

    for (idx, case) in cases.iter().enumerate() {
        let expected_ids = case.expected_ids();
        if expected_ids.is_empty() {
            return Err(format!(
                "recall_simulate case {} is missing expected_id or expected_ids",
                case.name
                    .as_deref()
                    .map(str::to_string)
                    .unwrap_or_else(|| (idx + 1).to_string())
            ));
        }

        let top_k = crate::clamp_facade_top_k(case.top_k.unwrap_or(default_top_k));
        let search_params = build_search_params(params, case, top_k)?;
        let rows_result = search_memory_rows_with_access(server, search_params, false, false).await;
        let (rows, error) = match rows_result {
            Ok(rows) => (rows, None),
            Err(err) => (Vec::new(), Some(err)),
        };
        let returned_ids = returned_ids(&rows);
        let rank = first_expected_rank(&returned_ids, &expected_ids);
        let reciprocal_rank = rank.map(|rank| 1.0 / rank as f64).unwrap_or(0.0);
        if rank.is_some() {
            hit_count += 1;
            reciprocal_sum += reciprocal_rank;
        }

        reports.push(json!({
            "name": case.name,
            "query": case.query,
            "expected_ids": expected_ids,
            "top_k": top_k,
            "hit": rank.is_some(),
            "rank": rank,
            "reciprocal_rank": reciprocal_rank,
            "returned_ids": returned_ids,
            "returned": rows.iter().map(compact_row).collect::<Vec<_>>(),
            "error": error,
        }));
    }

    let case_count = reports.len();
    let recall_at_k = if case_count == 0 {
        0.0
    } else {
        hit_count as f64 / case_count as f64
    };
    let mrr = if case_count == 0 {
        0.0
    } else {
        reciprocal_sum / case_count as f64
    };

    let report = json!({
        "status": "completed",
        "action": "recall_simulate",
        "case_count": case_count,
        "top_k": default_top_k,
        "metrics": {
            "hit_count": hit_count,
            "miss_count": case_count.saturating_sub(hit_count),
            "recall_at_k": recall_at_k,
            "mrr": mrr,
        },
        "cases": reports,
        "notes": [
            "Uses the memory hybrid-search path without recall-cache short-circuiting.",
            "Does not mutate memory access_count or recall counters."
        ],
    });

    if wants_json(params.format.as_deref()) {
        return json_string(&report);
    }
    Ok(format_recall_simulate_markdown(&report))
}

fn parse_cases(params: &TachiMemoryParams) -> Result<Vec<RecallSimCase>, String> {
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

fn build_search_params(
    params: &TachiMemoryParams,
    case: &RecallSimCase,
    top_k: usize,
) -> Result<SearchMemoryParams, String> {
    let scope = case
        .scope
        .as_deref()
        .or(params.scope.as_deref())
        .unwrap_or("memory")
        .trim()
        .to_ascii_lowercase();
    let mut path_prefix = case
        .path_prefix
        .clone()
        .or_else(|| params.path_prefix.clone());
    let mut include_training = case.include_training.unwrap_or(params.include_training);

    match scope.as_str() {
        "all" | "memory" => {}
        "wiki" => {
            path_prefix.get_or_insert_with(|| "/wiki".to_string());
        }
        "patterns" => {
            path_prefix.get_or_insert_with(|| "/user/patterns".to_string());
        }
        "sft" => {
            path_prefix = Some("/sft".to_string());
            include_training = true;
        }
        other => {
            return Err(format!(
                "recall_simulate unsupported scope '{other}'. Use memory, all, wiki, patterns, or sft."
            ));
        }
    }

    Ok(SearchMemoryParams {
        query: case.query.clone(),
        query_vec: None,
        top_k,
        path_prefix,
        include_training,
        include_archived: case.include_archived.unwrap_or(params.include_archived),
        candidates_per_channel: top_k.max(20),
        mmr_threshold: Some(0.85),
        graph_expand_hops: 1,
        graph_relation_filter: None,
        weights: None,
        context_symbols: Vec::new(),
        agent_role: None,
        project: case.project.clone().or_else(|| params.project.clone()),
        domain: case.domain.clone().or_else(|| params.domain.clone()),
        file_context: params.file_context.clone(),
        error_context: params.error_context.clone(),
        enable_rerank: false,
        as_of: case.as_of.clone().or_else(|| params.as_of.clone()),
        include_metadata: false,
    })
}

fn returned_ids(rows: &[Value]) -> Vec<String> {
    rows.iter()
        .filter_map(|row| row.get("id").and_then(Value::as_str).map(str::to_string))
        .collect()
}

fn first_expected_rank(returned_ids: &[String], expected_ids: &[String]) -> Option<usize> {
    returned_ids
        .iter()
        .position(|id| expected_ids.iter().any(|expected| expected == id))
        .map(|idx| idx + 1)
}

fn compact_row(row: &Value) -> Value {
    json!({
        "id": row.get("id").cloned().unwrap_or(Value::Null),
        "path": row.get("path").cloned().unwrap_or(Value::Null),
        "summary": row.get("summary").cloned().unwrap_or(Value::Null),
        "db": row.get("db").cloned().unwrap_or(Value::Null),
        "relevance": row.get("relevance").cloned().unwrap_or(Value::Null),
        "scores": row.get("scores").cloned().unwrap_or(Value::Null),
        "exact_token_match": row.get("exact_token_match").cloned().unwrap_or(Value::Null),
    })
}

fn format_recall_simulate_markdown(report: &Value) -> String {
    let metrics = &report["metrics"];
    let mut out = vec![
        "Tachi recall simulate".to_string(),
        format!(
            "status: {}",
            report
                .get("status")
                .and_then(Value::as_str)
                .unwrap_or("completed")
        ),
        format!(
            "cases: {} hit(s), {} miss(es), recall@k={:.3}, mrr={:.3}",
            metrics
                .get("hit_count")
                .and_then(Value::as_u64)
                .unwrap_or(0),
            metrics
                .get("miss_count")
                .and_then(Value::as_u64)
                .unwrap_or(0),
            metrics
                .get("recall_at_k")
                .and_then(Value::as_f64)
                .unwrap_or(0.0),
            metrics.get("mrr").and_then(Value::as_f64).unwrap_or(0.0),
        ),
    ];

    if let Some(cases) = report.get("cases").and_then(Value::as_array) {
        for case in cases {
            let status = if case.get("hit").and_then(Value::as_bool).unwrap_or(false) {
                "hit"
            } else {
                "miss"
            };
            let query = case.get("query").and_then(Value::as_str).unwrap_or("");
            let rank = case
                .get("rank")
                .and_then(Value::as_u64)
                .map(|rank| rank.to_string())
                .unwrap_or_else(|| "-".to_string());
            let returned = case
                .get("returned_ids")
                .and_then(Value::as_array)
                .map(|ids| {
                    ids.iter()
                        .filter_map(Value::as_str)
                        .take(5)
                        .collect::<Vec<_>>()
                        .join(", ")
                })
                .unwrap_or_default();
            out.push(format!(
                "- {status}: rank={rank} query=\"{query}\" returned=[{returned}]"
            ));
        }
    }

    out.join("\n")
}
