use std::path::{Path, PathBuf};

use memcore::{EvalEvidenceRow, MemoryStore};
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
            let (loaded, skipped_rows) = if let Some(path) = cases {
                (load_cases_file(&path)?, 0usize)
            } else {
                let corpus_db = project_db_path.map(PathBuf::as_path).unwrap_or(db_path);
                load_eval_namespace_cases(corpus_db)?
            };
            let status = run_recall_eval(
                loaded,
                skipped_rows,
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
    skipped_rows: usize,
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

    let status = build_aggregate_status(&report, &loaded, skipped_rows, top_k, min_recall, min_mrr);
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
) -> Result<(Vec<LoadedCase>, usize), Box<dyn std::error::Error>> {
    let store =
        MemoryStore::open_read_only(db_path.to_str().ok_or("eval DB path must be valid UTF-8")?)?;
    let rows = store.list_eval_evidence(3650, 10_000, false)?;
    Ok(collect_eval_namespace_cases(rows))
}

/// Pure (no I/O) core of [`load_eval_namespace_cases`]: turns raw
/// `/eval`-namespace rows into recall cases, counting rows that
/// `case_from_eval_metadata` declines to convert (missing query, no expected
/// ids, explicitly disabled, etc.) instead of silently dropping them
/// (tachi#911 tail sweep, #922: a shrinking corpus should be visible in the
/// eval status artifact / gate summary, not swallowed).
fn collect_eval_namespace_cases(rows: Vec<EvalEvidenceRow>) -> (Vec<LoadedCase>, usize) {
    let mut cases = Vec::new();
    let mut skipped_rows = 0usize;
    for row in rows {
        match case_from_eval_metadata(&row.metadata, &row.id, &row.summary) {
            Some(case) => cases.push(case),
            None => skipped_rows += 1,
        }
    }
    (cases, skipped_rows)
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
    skipped_rows: usize,
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
        "skipped_rows": skipped_rows,
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
        "  cases={} skipped_rows={} top_k={} hit={}/{} recall={} mrr={}",
        status["case_count"],
        status["skipped_rows"],
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
            0,
            10,
            1.0,
            0.0,
        );
        let rendered = serde_json::to_string(&status).expect("status JSON");

        assert_eq!(status["status"], json!("ok"));
        assert_eq!(status["per_slice"]["summary"], json!({"n": 1, "hits": 1}));
        assert_eq!(status["skipped_rows"], json!(0));
        assert!(!rendered.contains("private query"));
        assert!(!rendered.contains("target-1"));
    }

    // #922 (tachi#911 tail sweep): `load_eval_namespace_cases` silently
    // dropped rows that `case_from_eval_metadata` declined to convert
    // (malformed/incomplete `/eval` metadata), so a shrinking eval corpus
    // was invisible. `collect_eval_namespace_cases` is the pure core that
    // counts those drops without needing a real sqlite fixture.

    #[test]
    fn collect_eval_namespace_cases_counts_one_malformed_row_as_skipped() {
        let rows = vec![
            EvalEvidenceRow {
                id: "eval-good".to_string(),
                path: "/eval/run-1".to_string(),
                summary: "good case".to_string(),
                text: String::new(),
                metadata: json!({
                    "recall_eval": {
                        "query": "well-formed query",
                        "expected_id": "target-1",
                    }
                }),
                created_at: "2026-07-01T00:00:00Z".to_string(),
            },
            EvalEvidenceRow {
                id: "eval-malformed".to_string(),
                path: "/eval/run-2".to_string(),
                summary: "malformed case".to_string(),
                text: String::new(),
                // No `query` and no `expected_id`/`expected_ids` — the row
                // reads as an eval-namespace entry but is not a usable case.
                metadata: json!({ "recall_eval": {} }),
                created_at: "2026-07-01T00:00:01Z".to_string(),
            },
        ];

        let (cases, skipped_rows) = collect_eval_namespace_cases(rows);

        assert_eq!(cases.len(), 1, "well-formed row must still produce a case");
        assert_eq!(cases[0].case["query"], json!("well-formed query"));
        assert_eq!(skipped_rows, 1, "malformed row must be counted, not dropped silently");
    }

    #[test]
    fn collect_eval_namespace_cases_reports_zero_skipped_when_all_rows_are_valid() {
        let rows = vec![EvalEvidenceRow {
            id: "eval-good".to_string(),
            path: "/eval/run-1".to_string(),
            summary: "good case".to_string(),
            text: String::new(),
            metadata: json!({
                "recall_eval": { "query": "q", "expected_id": "target-1" }
            }),
            created_at: "2026-07-01T00:00:00Z".to_string(),
        }];

        let (cases, skipped_rows) = collect_eval_namespace_cases(rows);
        assert_eq!(cases.len(), 1);
        assert_eq!(skipped_rows, 0);
    }

    // #922 RED-path aggregation coverage (tachi#911 tail sweep): before this,
    // `build_aggregate_status` had no test exercising a failing corpus, so a
    // regression that always reported "ok" (or panicked) on a real failure
    // could have shipped unnoticed.

    #[test]
    fn aggregate_status_reports_failed_on_recall_below_threshold() {
        let loaded = vec![
            LoadedCase {
                case: json!({"query": "q1", "expected_ids": ["target-1"]}),
                slice: "summary".to_string(),
            },
            LoadedCase {
                case: json!({"query": "q2", "expected_ids": ["target-2"]}),
                slice: "summary".to_string(),
            },
        ];
        let status = build_aggregate_status(
            &json!({
                "variants": [{
                    "name": "current",
                    "case_count": 2,
                    "metrics": {"hit_count": 1, "miss_count": 1, "recall_at_k": 0.5, "mrr": 0.5},
                    "cases": [
                        {
                            "query": "q1",
                            "expected_ids": ["target-1"],
                            "returned_ids": ["target-1"],
                            "hit": true,
                            "error": null
                        },
                        {
                            "query": "q2",
                            "expected_ids": ["target-2"],
                            "returned_ids": [],
                            "hit": false,
                            "error": null
                        }
                    ],
                    "rerank": {"policy_counts": {}}
                }]
            }),
            &loaded,
            0,
            10,
            /* min_recall */ 1.0,
            /* min_mrr */ 1.0,
        );

        assert_eq!(
            status["status"],
            json!("failed"),
            "recall/mrr below configured gate thresholds must fail, not pass"
        );
        assert_eq!(status["current"]["recall_at_k"], json!(0.5));
        assert_eq!(status["current"]["case_errors"], json!(0));

        // The caller (`run_eval_command`) treats any non-"ok" status as a
        // hard gate failure (`status != "ok"` => `Err(...)`), so asserting
        // the exact non-"ok" string here covers the RED exit path without
        // needing to run the CLI end-to-end.
        assert_ne!(status["status"].as_str(), Some("ok"));
    }

    #[test]
    fn aggregate_status_reports_failed_when_any_case_errors() {
        let loaded = vec![LoadedCase {
            case: json!({"query": "q1", "expected_ids": ["target-1"]}),
            slice: "summary".to_string(),
        }];
        let status = build_aggregate_status(
            &json!({
                "variants": [{
                    "name": "current",
                    "case_count": 1,
                    "metrics": {"hit_count": 1, "miss_count": 0, "recall_at_k": 1.0, "mrr": 1.0},
                    "cases": [{
                        "query": "q1",
                        "expected_ids": ["target-1"],
                        "returned_ids": ["target-1"],
                        "hit": true,
                        "error": "vector backend unavailable"
                    }],
                    "rerank": {"policy_counts": {}}
                }]
            }),
            &loaded,
            0,
            10,
            0.0,
            0.0,
        );

        assert_eq!(
            status["status"],
            json!("failed"),
            "a case-level error must fail the gate even when recall/mrr floors are met"
        );
        assert_eq!(status["current"]["case_errors"], json!(1));
    }
}
