use serde_json::Value;

pub(super) fn format_recall_simulate_markdown(report: &Value) -> String {
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

    if let Some(variants) = report.get("variants").and_then(Value::as_array) {
        if variants.len() > 1 {
            out.push("variants:".to_string());
            for variant in variants {
                let name = variant
                    .get("name")
                    .and_then(Value::as_str)
                    .unwrap_or("variant");
                let metrics = &variant["metrics"];
                out.push(format!(
                    "- {name}: hit={} miss={} recall@k={:.3} mrr={:.3}",
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
                ));
            }
        }
    }

    out.join("\n")
}
