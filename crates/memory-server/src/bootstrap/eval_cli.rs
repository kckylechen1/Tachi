use std::path::{Path, PathBuf};

use memory_core::MemoryStore;
use serde_json::{Value, json};
use tachi_bootstrap::cli::EvalAction;

#[derive(Debug, Clone)]
struct LoadedCase {
    case: Value,
    slice: String,
}

pub(in crate::bootstrap) async fn run_eval_command(
    action: EvalAction,
    db_path: &Path,
    project_db_path: Option<&PathBuf>,
    app_home: &Path,
) -> Result<(), Box<dyn std::error::Error>> {
    match action {
        EvalAction::Recall {
            cases,
            top_k,
            min_recall,
            min_mrr,
            enable_rerank,
            json,
        } => {
            let loaded = if let Some(path) = cases {
                load_cases_file(&path)?
            } else {
                let corpus_db = project_db_path.map(PathBuf::as_path).unwrap_or(db_path);
                load_eval_namespace_cases(corpus_db)?
            };
            let status = run_recall_eval(
                loaded,
                db_path,
                project_db_path,
                app_home,
                top_k,
                min_recall,
                min_mrr,
                enable_rerank,
            )
            .await?;
            if json {
                println!("{}", serde_json::to_string_pretty(&status)?);
            } else {
                print_recall_eval_summary(&status);
            }
            if status.get("status").and_then(Value::as_str) != Some("ok") {
                return Err("recall eval failed configured gate".into());
            }
            Ok(())
        }
    }
}

async fn run_recall_eval(
    loaded: Vec<LoadedCase>,
    db_path: &Path,
    project_db_path: Option<&PathBuf>,
    app_home: &Path,
    top_k: usize,
    min_recall: f64,
    min_mrr: f64,
    enable_rerank: bool,
) -> Result<Value, Box<dyn std::error::Error>> {
    if loaded.is_empty() {
        return Err(
            "recall eval corpus is empty; add labeled cases under /eval or pass --cases".into(),
        );
    }

    let cases = loaded
        .iter()
        .map(|loaded| loaded.case.clone())
        .collect::<Vec<_>>();
    let params: crate::tool_params::TachiMemoryParams = serde_json::from_value(json!({
        "action": "recall_simulate",
        "format": "json",
        "top_k": top_k,
        "enable_rerank": enable_rerank,
        "metadata": {
            "cases": cases
        }
    }))?;
    let server = crate::MemoryServer::new(db_path.to_path_buf(), project_db_path.cloned())?;
    let report = crate::facade_memory_ops::build_recall_simulation_report(&server, &params).await?;

    let status = build_aggregate_status(&report, &loaded, top_k, min_recall, min_mrr);
    write_status_artifact(app_home, &status)?;
    Ok(status)
}

fn load_cases_file(path: &Path) -> Result<Vec<LoadedCase>, Box<dyn std::error::Error>> {
    let raw = std::fs::read_to_string(path)?;
    let value: Value = serde_json::from_str(&raw)?;
    let cases = extract_cases_array(&value)
        .ok_or("cases file must be an array or object with cases/eval_cases")?;
    Ok(cases
        .iter()
        .cloned()
        .map(|case| LoadedCase {
            slice: metadata_string(&case, "slice").unwrap_or_else(|| "unsliced".to_string()),
            case,
        })
        .collect())
}

fn load_eval_namespace_cases(
    db_path: &Path,
) -> Result<Vec<LoadedCase>, Box<dyn std::error::Error>> {
    let store =
        MemoryStore::open_read_only(db_path.to_str().ok_or("eval DB path must be valid UTF-8")?)?;
    let rows = store.list_eval_evidence(3650, 10_000, false)?;
    let mut cases = Vec::new();
    for row in rows {
        if let Some(case) = case_from_eval_metadata(&row.metadata, &row.id, &row.summary) {
            cases.push(case);
        }
    }
    Ok(cases)
}

fn case_from_eval_metadata(metadata: &Value, row_id: &str, summary: &str) -> Option<LoadedCase> {
    let source = metadata
        .get("recall_eval")
        .or_else(|| metadata.get("eval"))
        .filter(|value| value.is_object())
        .unwrap_or(metadata);
    if source.get("enabled").and_then(Value::as_bool) == Some(false) {
        return None;
    }
    let query = metadata_string(source, "query")?;
    let expected_ids = expected_ids(source);
    if expected_ids.is_empty() {
        return None;
    }
    let name = metadata_string(source, "name")
        .or_else(|| (!summary.trim().is_empty()).then(|| summary.to_string()))
        .unwrap_or_else(|| row_id.to_string());
    let slice = metadata_string(source, "slice").unwrap_or_else(|| "unsliced".to_string());
    let mut case = json!({
        "name": name,
        "query": query,
        "expected_ids": expected_ids,
    });
    for key in ["scope", "project", "domain", "path_prefix", "as_of"] {
        if let Some(value) = metadata_string(source, key) {
            case[key] = json!(value);
        }
    }
    if let Some(value) = source.get("top_k").and_then(Value::as_u64) {
        case["top_k"] = json!(value as usize);
    }
    Some(LoadedCase { case, slice })
}

fn extract_cases_array(value: &Value) -> Option<&Vec<Value>> {
    if let Some(cases) = value.as_array() {
        return Some(cases);
    }
    value
        .get("eval_cases")
        .or_else(|| value.get("cases"))
        .and_then(Value::as_array)
}

fn metadata_string(value: &Value, key: &str) -> Option<String> {
    value
        .get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
}

fn expected_ids(value: &Value) -> Vec<String> {
    let mut ids = Vec::new();
    if let Some(id) = metadata_string(value, "expected_id") {
        ids.push(id);
    }
    if let Some(array) = value.get("expected_ids").and_then(Value::as_array) {
        for id in array.iter().filter_map(Value::as_str).map(str::trim) {
            if !id.is_empty() && !ids.iter().any(|existing| existing == id) {
                ids.push(id.to_string());
            }
        }
    }
    ids
}

fn build_aggregate_status(
    report: &Value,
    loaded: &[LoadedCase],
    top_k: usize,
    min_recall: f64,
    min_mrr: f64,
) -> Value {
    let current = report
        .get("variants")
        .and_then(Value::as_array)
        .and_then(|variants| {
            variants
                .iter()
                .find(|variant| variant.get("name").and_then(Value::as_str) == Some("current"))
        })
        .unwrap_or(&Value::Null);
    let metrics = current.get("metrics").cloned().unwrap_or_else(|| json!({}));
    let recall = metrics
        .get("recall_at_k")
        .and_then(Value::as_f64)
        .unwrap_or(0.0);
    let mrr = metrics.get("mrr").and_then(Value::as_f64).unwrap_or(0.0);
    let case_errors = current
        .get("cases")
        .and_then(Value::as_array)
        .map(|cases| {
            cases
                .iter()
                .filter(|case| !case.get("error").unwrap_or(&Value::Null).is_null())
                .count()
        })
        .unwrap_or(0);
    let passed = recall >= min_recall && mrr >= min_mrr && case_errors == 0;
    json!({
        "schema_version": "tachi.recall_eval.status.v1",
        "status": if passed { "ok" } else { "failed" },
        "generated_at": chrono::Utc::now().to_rfc3339(),
        "case_count": loaded.len(),
        "top_k": top_k,
        "thresholds": {
            "min_recall": min_recall,
            "min_mrr": min_mrr,
        },
        "current": {
            "hit_count": metrics.get("hit_count").cloned().unwrap_or(Value::Null),
            "miss_count": metrics.get("miss_count").cloned().unwrap_or(Value::Null),
            "recall_at_k": recall,
            "mrr": mrr,
            "case_errors": case_errors,
        },
        "per_slice": aggregate_slices(current, loaded),
        "variants": aggregate_variants(report),
        "detail": "aggregate-only; raw queries, expected ids, and returned ids are intentionally omitted",
    })
}

fn aggregate_slices(current: &Value, loaded: &[LoadedCase]) -> Value {
    let mut slices = serde_json::Map::new();
    let cases = current
        .get("cases")
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or(&[]);
    for (idx, loaded_case) in loaded.iter().enumerate() {
        let hit = cases
            .get(idx)
            .and_then(|case| case.get("hit"))
            .and_then(Value::as_bool)
            .unwrap_or(false);
        let entry = slices
            .entry(loaded_case.slice.clone())
            .or_insert_with(|| json!({"n": 0, "hits": 0}));
        entry["n"] = json!(entry["n"].as_u64().unwrap_or(0) + 1);
        if hit {
            entry["hits"] = json!(entry["hits"].as_u64().unwrap_or(0) + 1);
        }
    }
    Value::Object(slices)
}

fn aggregate_variants(report: &Value) -> Value {
    let variants = report
        .get("variants")
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or(&[]);
    Value::Array(
        variants
            .iter()
            .map(|variant| {
                let metrics = variant.get("metrics").unwrap_or(&Value::Null);
                json!({
                    "name": variant.get("name").cloned().unwrap_or(Value::Null),
                    "case_count": variant.get("case_count").cloned().unwrap_or(Value::Null),
                    "hit_count": metrics.get("hit_count").cloned().unwrap_or(Value::Null),
                    "miss_count": metrics.get("miss_count").cloned().unwrap_or(Value::Null),
                    "recall_at_k": metrics.get("recall_at_k").cloned().unwrap_or(Value::Null),
                    "mrr": metrics.get("mrr").cloned().unwrap_or(Value::Null),
                    "rerank_policy_counts": variant
                        .get("rerank")
                        .and_then(|rerank| rerank.get("policy_counts"))
                        .cloned()
                        .unwrap_or(Value::Null),
                })
            })
            .collect(),
    )
}

fn write_status_artifact(
    app_home: &Path,
    status: &Value,
) -> Result<(), Box<dyn std::error::Error>> {
    let path = crate::status_ops::recall_eval::recall_eval_status_path(app_home);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, serde_json::to_vec_pretty(status)?)?;
    std::fs::rename(tmp, path)?;
    Ok(())
}

fn print_recall_eval_summary(status: &Value) {
    let current = &status["current"];
    println!(
        "Recall eval: {}",
        status["status"].as_str().unwrap_or("unknown")
    );
    println!(
        "  cases={} top_k={} hit={}/{} recall={} mrr={}",
        status["case_count"],
        status["top_k"],
        current["hit_count"],
        status["case_count"],
        current["recall_at_k"],
        current["mrr"],
    );
    println!(
        "  status: {}",
        crate::status_ops::recall_eval::recall_eval_status_path(
            &crate::status_ops::resolve_app_home()
        )
        .display()
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn eval_metadata_becomes_recall_case_without_private_status_fields() {
        let loaded = case_from_eval_metadata(
            &json!({
                "recall_eval": {
                    "query": "private query",
                    "expected_id": "target-1",
                    "slice": "summary_cjk",
                    "path_prefix": "/notes",
                    "top_k": 7
                }
            }),
            "eval-row-1",
            "case summary",
        )
        .expect("case");

        assert_eq!(loaded.slice, "summary_cjk");
        assert_eq!(loaded.case["query"], json!("private query"));
        assert_eq!(loaded.case["expected_ids"], json!(["target-1"]));
        assert_eq!(loaded.case["path_prefix"], json!("/notes"));
        assert_eq!(loaded.case["top_k"], json!(7));
    }

    #[test]
    fn aggregate_status_omits_raw_recall_report_details() {
        let loaded = vec![LoadedCase {
            case: json!({"query": "private query", "expected_ids": ["target-1"]}),
            slice: "summary".to_string(),
        }];
        let status = build_aggregate_status(
            &json!({
                "variants": [{
                    "name": "current",
                    "case_count": 1,
                    "metrics": {"hit_count": 1, "miss_count": 0, "recall_at_k": 1.0, "mrr": 1.0},
                    "cases": [{
                        "query": "private query",
                        "expected_ids": ["target-1"],
                        "returned_ids": ["target-1"],
                        "hit": true,
                        "error": null
                    }],
                    "rerank": {"policy_counts": {}}
                }]
            }),
            &loaded,
            10,
            1.0,
            0.0,
        );
        let rendered = serde_json::to_string(&status).expect("status JSON");

        assert_eq!(status["status"], json!("ok"));
        assert_eq!(status["per_slice"]["summary"], json!({"n": 1, "hits": 1}));
        assert!(!rendered.contains("private query"));
        assert!(!rendered.contains("target-1"));
    }
}
