use std::collections::BTreeMap;

use memcore::RecallConfig;
use serde_json::{json, Value};

use crate::facade_memory_ops::{json_string, wants_json};
use super::config::{recall_config_env_diff, recall_config_summary};
use super::input::{parse_cases, parse_variants};
use super::markdown::format_recall_simulate_markdown;
use super::types::RecallSimCase;
use crate::memory_search_ops::{
    apply_search_rerank_policy, expand_search_params_for_rerank, normalize_json_relevance,
    search_memory_rows_with_recall_config, SearchRerankPolicy,
};
use crate::tool_params::*;
use crate::MemoryServer;

pub(crate) async fn handle_memory_recall_simulate(
    server: &MemoryServer,
    params: &TachiMemoryParams,
) -> Result<String, String> {
    let report = build_recall_simulation_report(server, params).await?;
    if wants_json(params.format.as_deref()) {
        return json_string(&report);
    }
    Ok(format_recall_simulate_markdown(&report))
}

pub(crate) async fn build_recall_simulation_report(
    server: &MemoryServer,
    params: &TachiMemoryParams,
) -> Result<Value, String> {
    let cases = parse_cases(params)?;
    let default_top_k = crate::clamp_facade_top_k(params.top_k);
    let candidate_variants = parse_variants(params)?;
    let base_config = RecallConfig::get().clone();
    let current_report =
        run_variant(server, params, &cases, default_top_k, "current", None).await?;
    let mut variants = vec![current_report.clone()];
    for (idx, variant) in candidate_variants.iter().enumerate() {
        let config = variant.to_recall_config(&base_config);
        let mut report = run_variant(
            server,
            params,
            &cases,
            default_top_k,
            &variant.configured_name(idx + 1),
            Some(&config),
        )
        .await?;
        report["config_env"] = json!(recall_config_env_diff(&base_config, &config));
        variants.push(report);
    }

    Ok(json!({
        "status": "completed",
        "action": "recall_simulate",
        "case_count": cases.len(),
        "top_k": default_top_k,
        "rerank": {
            "enabled": params.enable_rerank,
            "policy": if params.enable_rerank { "adaptive" } else { "disabled" },
        },
        "metrics": current_report["metrics"],
        "cases": current_report["cases"],
        "variants": variants,
        "notes": [
            "Uses the memory hybrid-search path without recall-cache short-circuiting.",
            "When enable_rerank=true, uses the same adaptive rerank gate as search after expanding candidates.",
            "Does not mutate memory access_count or recall counters.",
            "Variant recall_config overrides apply only inside this simulation call."
        ],
    }))
}

async fn run_variant(
    server: &MemoryServer,
    params: &TachiMemoryParams,
    cases: &[RecallSimCase],
    default_top_k: usize,
    name: &str,
    recall_config: Option<&RecallConfig>,
) -> Result<Value, String> {
    let mut reports = Vec::with_capacity(cases.len());
    let mut hit_count = 0usize;
    let mut reciprocal_sum = 0.0f64;
    let mut rerank_policy_counts: BTreeMap<&'static str, usize> = BTreeMap::new();
    let mut flip_report = FlipReport::default();

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
        let baseline = if params.enable_rerank {
            baseline_replay(server, params, case, top_k, &expected_ids, recall_config).await
        } else {
            CaseBaseline::not_run()
        };
        let mut search_params = build_search_params(params, case, top_k)?;
        expand_search_params_for_rerank(&mut search_params, top_k);
        let candidate_top_k = search_params.top_k;
        let rows_result = search_memory_rows_with_recall_config(
            server,
            search_params,
            false,
            false,
            recall_config,
        )
        .await;
        let (rows, rerank_policy, candidate_count, error) = match rows_result {
            Ok(rows) => {
                let candidate_count = rows.len();
                let (mut rows, policy) = apply_search_rerank_policy(
                    server,
                    &case.query,
                    rows,
                    top_k,
                    params.enable_rerank,
                )
                .await;
                normalize_json_relevance(&mut rows);
                (rows, Some(policy), candidate_count, None)
            }
            Err(err) => (Vec::new(), None, 0, Some(err)),
        };
        if let Some(policy) = rerank_policy {
            *rerank_policy_counts.entry(policy.as_str()).or_insert(0) += 1;
        }
        let returned_ids = returned_ids(&rows);
        let rank = first_expected_rank(&returned_ids, &expected_ids);
        let reciprocal_rank = rank.map(|rank| 1.0 / rank as f64).unwrap_or(0.0);
        if rank.is_some() {
            hit_count += 1;
            reciprocal_sum += reciprocal_rank;
        }
        if params.enable_rerank {
            flip_report.observe(baseline.rank, rank);
        }

        reports.push(json!({
            "name": case.name,
            "query": case.query,
            "expected_ids": expected_ids,
            "top_k": top_k,
            "candidate_top_k": candidate_top_k,
            "hit": rank.is_some(),
            "rank": rank,
            "reciprocal_rank": reciprocal_rank,
            "returned_ids": returned_ids,
            "returned": rows.iter().map(compact_row).collect::<Vec<_>>(),
            "baseline": baseline.to_json(),
            "rerank": {
                "enabled": params.enable_rerank,
                "policy": rerank_policy
                    .map(SearchRerankPolicy::as_str)
                    .unwrap_or("not_run"),
                "candidate_count": candidate_count,
            },
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

    Ok(json!({
        "name": name,
        "config": recall_config.map(recall_config_summary),
        "case_count": case_count,
        "metrics": {
            "hit_count": hit_count,
            "miss_count": case_count.saturating_sub(hit_count),
            "recall_at_k": recall_at_k,
            "mrr": mrr,
        },
        "rerank": {
            "enabled": params.enable_rerank,
            "policy_counts": rerank_policy_counts,
        },
        "flip_report": flip_report.to_json(),
        "cases": reports,
    }))
}

#[derive(Debug, Default)]
struct FlipReport {
    hit_to_miss: usize,
    miss_to_hit: usize,
    rank_improved: usize,
    rank_worsened: usize,
    hit_retained: usize,
    miss_retained: usize,
}

impl FlipReport {
    fn observe(&mut self, baseline_rank: Option<usize>, rerank_rank: Option<usize>) {
        match (baseline_rank, rerank_rank) {
            (Some(_), None) => self.hit_to_miss += 1,
            (None, Some(_)) => self.miss_to_hit += 1,
            (Some(before), Some(after)) if after < before => {
                self.hit_retained += 1;
                self.rank_improved += 1;
            }
            (Some(before), Some(after)) if after > before => {
                self.hit_retained += 1;
                self.rank_worsened += 1;
            }
            (Some(_), Some(_)) => self.hit_retained += 1,
            (None, None) => self.miss_retained += 1,
        }
    }

    fn to_json(&self) -> Value {
        json!({
            "hit_to_miss": self.hit_to_miss,
            "miss_to_hit": self.miss_to_hit,
            "rank_improved": self.rank_improved,
            "rank_worsened": self.rank_worsened,
            "hit_retained": self.hit_retained,
            "miss_retained": self.miss_retained,
        })
    }
}

#[derive(Debug)]
struct CaseBaseline {
    rank: Option<usize>,
    returned_ids: Vec<String>,
    error: Option<String>,
}

impl CaseBaseline {
    fn not_run() -> Self {
        Self {
            rank: None,
            returned_ids: Vec::new(),
            error: None,
        }
    }

    fn to_json(&self) -> Value {
        if self.returned_ids.is_empty() && self.rank.is_none() && self.error.is_none() {
            return Value::Null;
        }
        json!({
            "hit": self.rank.is_some(),
            "rank": self.rank,
            "returned_ids": self.returned_ids,
            "error": self.error,
        })
    }
}

async fn baseline_replay(
    server: &MemoryServer,
    params: &TachiMemoryParams,
    case: &RecallSimCase,
    top_k: usize,
    expected_ids: &[String],
    recall_config: Option<&RecallConfig>,
) -> CaseBaseline {
    let mut baseline_params = match build_search_params(params, case, top_k) {
        Ok(params) => params,
        Err(err) => {
            return CaseBaseline {
                rank: None,
                returned_ids: Vec::new(),
                error: Some(err),
            }
        }
    };
    baseline_params.enable_rerank = false;
    match search_memory_rows_with_recall_config(
        server,
        baseline_params,
        false,
        false,
        recall_config,
    )
    .await
    {
        Ok(mut rows) => {
            rows.truncate(top_k);
            normalize_json_relevance(&mut rows);
            let returned_ids = returned_ids(&rows);
            let rank = first_expected_rank(&returned_ids, expected_ids);
            CaseBaseline {
                rank,
                returned_ids,
                error: None,
            }
        }
        Err(err) => CaseBaseline {
            rank: None,
            returned_ids: Vec::new(),
            error: Some(err),
        },
    }
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
        enable_rerank: params.enable_rerank,
        as_of: case.as_of.clone().or_else(|| params.as_of.clone()),
        include_metadata: false,
        format: None,
    })
}

fn returned_ids(rows: &[Value]) -> Vec<String> {
    rows.iter()
        .filter_map(|row| row.get("id").and_then(Value::as_str).map(str::to_string))
        .collect()
}

/// Scope declaration (tachi#1504): this pathway is exact-ID-only and has no
/// lineage awareness. It matches `returned_ids` against `expected_ids` by
/// literal identity only, so a superseded `expected_id` reads as a permanent
/// miss even when its active successor is returned. Canonical/lineage
/// semantics (stored `superseded_by` resolution, reviewed equivalence) live
/// in `memcore::recall_coverage` (#1504), not here. The staleness this
/// implies for simulate-ops expected-id fixtures now rides in `tune_ops` as
/// part of the #1426 `tachi_tune` migration, not this issue.
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
        "match_type": row.get("match_type").cloned().unwrap_or(Value::Null),
        "rerank_policy": row.get("rerank_policy").cloned().unwrap_or(Value::Null),
        "rerank_score": row.get("rerank_score").cloned().unwrap_or(Value::Null),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn flip_report_counts_hit_retention_and_promotion() {
        let mut report = FlipReport::default();
        report.observe(Some(1), Some(1));
        report.observe(Some(3), Some(1));
        report.observe(None, Some(2));
        report.observe(Some(2), None);
        report.observe(Some(1), Some(3));
        report.observe(None, None);

        let json = report.to_json();
        assert_eq!(json["hit_to_miss"], json!(1));
        assert_eq!(json["miss_to_hit"], json!(1));
        assert_eq!(json["rank_improved"], json!(1));
        assert_eq!(json["rank_worsened"], json!(1));
        assert_eq!(json["hit_retained"], json!(3));
        assert_eq!(json["miss_retained"], json!(1));
    }
}
