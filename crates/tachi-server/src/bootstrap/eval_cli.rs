use std::path::{Path, PathBuf};

use crate::memory_search_ops::recall_simulate::{
    build_recall_simulation_report, RecallSimulationParams,
};
use memcore::{EvalEvidenceRow, MemoryStore};
use serde_json::{json, Value};
use tachi_bootstrap::cli::EvalAction;

#[derive(Debug, Clone)]
struct LoadedCase {
    case: Value,
    slice: String,
}

#[derive(Default)]
struct LoadedCorpus {
    cases: Vec<LoadedCase>,
    skipped_rows: usize,
    disabled_rows: usize,
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
            reviewed_only,
            json,
        } => {
            let outcome = async {
                let corpus = if let Some(path) = cases {
                    load_cases_file(&path)?
                } else {
                    let corpus_db = project_db_path.map(PathBuf::as_path).unwrap_or(db_path);
                    load_eval_namespace_cases(corpus_db)?
                };
                run_recall_eval(
                    corpus,
                    db_path,
                    project_db_path,
                    app_home,
                    top_k,
                    min_recall,
                    min_mrr,
                    enable_rerank,
                    reviewed_only,
                )
                .await
            }
            .await;
            let status = match outcome {
                Ok(status) => status,
                Err(error) => {
                    // Replace stale green status even for file/DB/argument failures.
                    let status = json!({
                        "schema_version": "tachi.recall_eval.status.v1",
                        "status": "error",
                        "generated_at": chrono::Utc::now().to_rfc3339(),
                        "incomplete_reason": "corpus loading, validation, or replay failed"
                    });
                    write_status_artifact(app_home, &status)?;
                    if json {
                        println!("{}", serde_json::to_string_pretty(&status)?);
                    }
                    return Err(error);
                }
            };
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
    corpus: LoadedCorpus,
    db_path: &Path,
    project_db_path: Option<&PathBuf>,
    app_home: &Path,
    top_k: usize,
    min_recall: f64,
    min_mrr: f64,
    enable_rerank: bool,
    reviewed_only: bool,
) -> Result<Value, Box<dyn std::error::Error>> {
    for (name, value) in [("min_recall", min_recall), ("min_mrr", min_mrr)] {
        if !value.is_finite() || !(0.0..=1.0).contains(&value) {
            return Err(format!("{name} must be finite and between zero and one").into());
        }
    }
    if reviewed_only && min_mrr <= 0.0 {
        return Err("--reviewed-only requires an explicit positive --min-mrr ranking floor".into());
    }
    let loaded_count = corpus.cases.len();
    let loaded_labels = label_counts(&corpus.cases);
    let loaded = corpus
        .cases
        .into_iter()
        .filter(|case| !reviewed_only || label_kind(&case.case, &case.case) == "reviewed")
        .collect::<Vec<_>>();
    let top_k = crate::clamp_facade_top_k(top_k);
    let params = RecallSimulationParams {
        top_k,
        enable_rerank,
        metadata: Some(
            json!({ "cases": loaded.iter().map(|case| case.case.clone()).collect::<Vec<_>>() }),
        ),
        ..RecallSimulationParams::default()
    };
    let report = if loaded.is_empty() {
        json!({"variants": []})
    } else {
        let server = crate::MemoryServer::new(db_path.to_path_buf(), project_db_path.cloned())?;
        build_recall_simulation_report(&server, &params).await?
    };
    let mut status = build_aggregate_status(
        &report,
        &loaded,
        corpus.skipped_rows,
        top_k,
        min_recall,
        min_mrr,
    );
    status["loaded_case_count"] = json!(loaded_count);
    status["loaded_label_counts"] = loaded_labels;
    status["disabled_rows"] = json!(corpus.disabled_rows);
    status["excluded_by_label"] = json!(loaded_count - loaded.len());
    status["gate_label_policy"] = json!(if reviewed_only {
        "reviewed_only"
    } else {
        "all_loaded_cases"
    });
    if reviewed_only && corpus.skipped_rows > corpus.disabled_rows {
        status["status"] = json!("incomplete");
        status["incomplete_reason"] = json!("invalid corpus rows were skipped");
    }
    write_status_artifact(app_home, &status)?;
    Ok(status)
}

fn load_cases_file(path: &Path) -> Result<LoadedCorpus, Box<dyn std::error::Error>> {
    let raw = std::fs::read_to_string(path)?;
    let value: Value = serde_json::from_str(&raw)?;
    let cases = extract_cases_array(&value)
        .ok_or("cases file must be an array or object with cases/eval_cases")?;
    let mut corpus = LoadedCorpus::default();
    for (index, mut case) in cases.iter().cloned().enumerate() {
        if !case.is_object() {
            return Err(format!("recall case {} must be an object", index + 1).into());
        }
        if case.get("enabled").and_then(Value::as_bool) == Some(false) {
            corpus.skipped_rows += 1;
            corpus.disabled_rows += 1;
            continue;
        }
        let kind = label_kind(&case, &case);
        case["label_kind"] = json!(kind);
        corpus.cases.push(LoadedCase {
            slice: metadata_string(&case, "slice").unwrap_or_else(|| "unsliced".to_string()),
            case,
        });
    }
    Ok(corpus)
}

fn load_eval_namespace_cases(db_path: &Path) -> Result<LoadedCorpus, Box<dyn std::error::Error>> {
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
fn collect_eval_namespace_cases(rows: Vec<EvalEvidenceRow>) -> LoadedCorpus {
    let mut corpus = LoadedCorpus::default();
    for row in rows {
        let source = eval_source(&row.metadata);
        if source.get("enabled").and_then(Value::as_bool) == Some(false) {
            corpus.disabled_rows += 1;
        }
        match case_from_eval_metadata(&row.metadata, &row.id, &row.summary) {
            Some(case) => corpus.cases.push(case),
            None => corpus.skipped_rows += 1,
        }
    }
    corpus
}

fn eval_source(metadata: &Value) -> &Value {
    metadata
        .get("recall_eval")
        .or_else(|| metadata.get("eval"))
        .filter(|value| value.is_object())
        .unwrap_or(metadata)
}

fn case_from_eval_metadata(metadata: &Value, row_id: &str, summary: &str) -> Option<LoadedCase> {
    let source = eval_source(metadata);
    if source.get("enabled").and_then(Value::as_bool) == Some(false) {
        return None;
    }
    metadata_string(source, "query")?;
    if source.get("expected_id").is_none()
        && source.get("expected_ids").is_none()
        && source.get("expectation").and_then(Value::as_str) != Some("empty")
    {
        return None;
    }
    let slice = metadata_string(source, "slice").unwrap_or_else(|| "unsliced".to_string());
    // Preserve source types until the shared RecallSimCase decoder validates
    // them. Filtering IDs or routing fields here could weaken an all/empty
    // contract that the equivalent --cases file correctly refuses.
    let mut case = source.clone();
    if case.get("name").is_none() {
        case["name"] = json!(if summary.trim().is_empty() {
            row_id
        } else {
            summary
        });
    }
    case["label_kind"] = json!(label_kind(metadata, source));
    Some(LoadedCase { case, slice })
}

/// Auto-captured search hits are behavioral observations, not independent gold
/// labels. Old captures predate `label_kind`, so retain their source markers.
fn label_kind(metadata: &Value, source: &Value) -> &'static str {
    if metadata.get("auto_captured").and_then(Value::as_bool) == Some(true)
        || source.get("source").and_then(Value::as_str) == Some("record_access")
        || source.get("signal").and_then(Value::as_str) == Some("recalled_and_used")
        || source.get("slice").and_then(Value::as_str) == Some("auto_capture")
        || source.get("label_kind").and_then(Value::as_str) == Some("weak")
    {
        "weak"
    } else if source.get("label_kind").and_then(Value::as_str) == Some("reviewed") {
        "reviewed"
    } else {
        "unspecified"
    }
}

fn label_counts(loaded: &[LoadedCase]) -> Value {
    let mut counts = json!({"weak": 0, "reviewed": 0, "unspecified": 0});
    for loaded in loaded {
        let kind = label_kind(&loaded.case, &loaded.case);
        counts[kind] = json!(counts[kind].as_u64().unwrap_or(0) + 1);
    }
    counts
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
    let positive_cases = loaded
        .iter()
        .filter(|case| case.case.get("expectation").and_then(Value::as_str) != Some("empty"))
        .count();
    let reported_cases = current
        .get("cases")
        .and_then(Value::as_array)
        .map_or(0, Vec::len);
    let complete = !loaded.is_empty() && reported_cases == loaded.len();
    let contract_failures = metrics
        .get("contract_failures")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    let baseline_errors = metrics
        .get("baseline_error_count")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    let passed = complete
        && (positive_cases == 0 || (recall >= min_recall && mrr >= min_mrr))
        && case_errors == 0
        && baseline_errors == 0
        && contract_failures == 0;
    json!({
        "schema_version": "tachi.recall_eval.status.v1",
        "status": if !complete { "incomplete" } else if passed { "ok" } else { "failed" },
        "incomplete_reason": if !complete { Some("no selected cases or incomplete replay") } else { None },
        "generated_at": chrono::Utc::now().to_rfc3339(),
        "case_count": loaded.len(),
        "skipped_rows": skipped_rows,
        "label_counts": label_counts(loaded),
        "gate_label_policy": "all_loaded_cases",
        "top_k": top_k,
        "thresholds": {
            "min_recall": min_recall,
            "min_mrr": min_mrr,
        },
        "current": {
            "hit_count": metrics.get("hit_count").cloned().unwrap_or(Value::Null),
            "miss_count": metrics.get("miss_count").cloned().unwrap_or(Value::Null),
            "recall_at_k": if positive_cases > 0 { Some(recall) } else { None },
            "positive_case_count": positive_cases,
            "empty_case_count": metrics.get("empty_case_count").cloned().unwrap_or(Value::Null),
            "empty_pass_count": metrics.get("empty_pass_count").cloned().unwrap_or(Value::Null),
            "contract_failures": contract_failures,
            "baseline_error_count": baseline_errors,
            "rank_cutoffs": metrics.get("rank_cutoffs").cloned().unwrap_or(Value::Null),
            "recall_at_1": metrics.get("recall_at_1").cloned().unwrap_or(Value::Null),
            "recall_at_3": metrics.get("recall_at_3").cloned().unwrap_or(Value::Null),
            "recall_at_3_case_count": metrics.get("recall_at_3_case_count").cloned().unwrap_or(Value::Null),
            "mrr": if positive_cases > 0 { Some(mrr) } else { None },
            "mrr_at_k": if positive_cases > 0 { Some(mrr) } else { None },
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
        let positive = loaded_case.case.get("expectation").and_then(Value::as_str) != Some("empty");
        let hit = cases
            .get(idx)
            .and_then(|case| case.get("hit"))
            .and_then(Value::as_bool)
            .unwrap_or(false);
        let entry = slices.entry(loaded_case.slice.clone()).or_insert_with(
            || json!({"n": 0, "hits": 0, "positive_cases": 0, "empty_cases": 0, "empty_passes": 0}),
        );
        entry["n"] = json!(entry["n"].as_u64().unwrap_or(0) + 1);
        let key = if positive {
            "positive_cases"
        } else {
            "empty_cases"
        };
        entry[key] = json!(entry[key].as_u64().unwrap_or(0) + 1);
        if positive && hit {
            entry["hits"] = json!(entry["hits"].as_u64().unwrap_or(0) + 1);
        } else if !positive
            && cases
                .get(idx)
                .and_then(|case| case.get("expectation_passed"))
                .and_then(Value::as_bool)
                == Some(true)
        {
            entry["empty_passes"] = json!(entry["empty_passes"].as_u64().unwrap_or(0) + 1);
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
                    "recall_at_1": metrics.get("recall_at_1").cloned().unwrap_or(Value::Null),
                    "recall_at_3": metrics.get("recall_at_3").cloned().unwrap_or(Value::Null),
                    "recall_at_3_case_count": metrics.get("recall_at_3_case_count").cloned().unwrap_or(Value::Null),
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
        current["positive_case_count"],
        current["recall_at_k"],
        current["mrr"],
    );
    println!(
        "  status: {}",
        crate::status_ops::recall_eval::recall_eval_status_path(
            &crate::path_utils::resolve_app_home()
        )
        .display()
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recall_eval_labels_preserve_weak_capture_provenance_and_private_aggregate() {
        let cases = [
            json!({"auto_captured": true, "recall_eval": {
                "query": "private auto query", "expected_id": "private-target", "label_kind": "reviewed"
            }}),
            json!({"recall_eval": {
                "query": "private legacy query", "expected_id": "private-target", "source": "record_access"
            }}),
            json!({"recall_eval": {
                "query": "private reviewed query", "expected_id": "private-target", "label_kind": "reviewed"
            }}),
            json!({"recall_eval": {
                "query": "private unspecified query", "expected_id": "private-target"
            }}),
        ];
        let loaded = cases
            .iter()
            .map(|metadata| {
                case_from_eval_metadata(metadata, "private-case", "private-summary").expect("case")
            })
            .collect::<Vec<_>>();
        let report = json!({"variants": [{"name": "current", "metrics": {
            "recall_at_k": 1.0, "mrr": 0.75, "recall_at_1": 0.5,
            "recall_at_3": 1.0, "recall_at_3_case_count": 4
        }}]});
        let status = build_aggregate_status(&report, &loaded, 0, 5, 0.9, 0.5);
        assert_eq!(
            status["label_counts"],
            json!({"weak": 2, "reviewed": 1, "unspecified": 1})
        );
        assert_eq!(status["gate_label_policy"], json!("all_loaded_cases"));
        assert_eq!(status["current"]["recall_at_1"], json!(0.5));
        assert_eq!(status["current"]["recall_at_3"], json!(1.0));
        assert!(!status.to_string().contains("private"));
    }

    #[test]
    fn recall_eval_file_labels_keep_legacy_auto_capture_weak() {
        let dir = tempfile::tempdir().expect("temporary cases");
        let path = dir.path().join("cases.json");
        std::fs::write(&path, serde_json::to_vec(&json!([
            {"query": "q", "expected_id": "id", "slice": "auto_capture", "label_kind": "reviewed"},
            {"query": "q2", "expected_id": "id2", "label_kind": "reviewed"}
        ])).unwrap()).unwrap();
        let loaded = load_cases_file(&path).expect("load cases");
        assert_eq!(
            label_counts(&loaded.cases),
            json!({"weak": 1, "reviewed": 1, "unspecified": 0})
        );
    }

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
        assert_eq!(loaded.case["expected_id"], json!("target-1"));
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
        assert_eq!(
            status["per_slice"]["summary"],
            json!({"n": 1, "hits": 1, "positive_cases": 1, "empty_cases": 0, "empty_passes": 0})
        );
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

        let corpus = collect_eval_namespace_cases(rows);
        let cases = corpus.cases;
        let skipped_rows = corpus.skipped_rows;

        assert_eq!(cases.len(), 1, "well-formed row must still produce a case");
        assert_eq!(cases[0].case["query"], json!("well-formed query"));
        assert_eq!(
            skipped_rows, 1,
            "malformed row must be counted, not dropped silently"
        );
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

        let corpus = collect_eval_namespace_cases(rows);
        let cases = corpus.cases;
        let skipped_rows = corpus.skipped_rows;
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
