use crate::server_state::MemoryServer;
use memcore::MemoryStore;
use serde_json::{json, Value};

use super::{DailyStageReport, EvalEvidenceRow};

pub(crate) fn collect_routing_stats(
    server: &MemoryServer,
    date: &str,
) -> Result<DailyStageReport, String> {
    let eval_rows = collect_eval_rows_30d(server)?;

    let mut agent_stats: std::collections::BTreeMap<String, (u64, u64, f64, u64)> =
        std::collections::BTreeMap::new();
    for row in &eval_rows {
        let agent = row
            .metadata
            .get("agent")
            .and_then(Value::as_str)
            .unwrap_or("unknown")
            .to_string();
        let outcome = row
            .metadata
            .get("outcome")
            .and_then(Value::as_str)
            .unwrap_or("unknown");
        let quality = row
            .metadata
            .get("quality_score")
            .and_then(Value::as_f64)
            .unwrap_or(0.0);
        let entry = agent_stats.entry(agent).or_insert((0, 0, 0.0, 0));
        entry.0 += 1; // total
        if outcome == "success" {
            entry.1 += 1;
        }
        if quality > 0.0 {
            entry.2 += quality;
            entry.3 += 1;
        }
    }

    let agents_payload: Vec<Value> = agent_stats
        .iter()
        .map(|(agent, (total, success, quality_sum, quality_count))| {
            let success_rate = if *total > 0 {
                *success as f64 / *total as f64
            } else {
                0.0
            };
            let avg_quality = if *quality_count > 0 {
                quality_sum / *quality_count as f64
            } else {
                0.0
            };
            json!({
                "agent_id": agent,
                "total_evals": total,
                "successes": success,
                "success_rate": (success_rate * 100.0).round() / 100.0,
                "avg_quality": (avg_quality * 100.0).round() / 100.0,
            })
        })
        .collect();

    let payload = json!({
        "date": date,
        "total_eval_records": eval_rows.len(),
        "agents": agents_payload,
    });

    Ok(DailyStageReport {
        status: "collected".to_string(),
        summary: format!(
            "Routing statistics: {} eval record(s), {} agent(s)",
            eval_rows.len(),
            agent_stats.len(),
        ),
        details: payload,
    })
}

fn collect_eval_rows_30d(server: &MemoryServer) -> Result<Vec<EvalEvidenceRow>, String> {
    let collect = |store: &mut MemoryStore| -> Result<Vec<EvalEvidenceRow>, String> {
        store
            .list_eval_evidence(30, 500, true)
            .map_err(|e| format!("list routing eval evidence: {e}"))
    };

    let mut rows = server
        .with_global_store_read(collect)
        .map_err(|e| format!("collect global eval evidence: {e}"))?;
    if server.has_project_db() {
        let mut project_rows = server
            .with_project_store_read(collect)
            .map_err(|e| format!("collect project eval evidence: {e}"))?;
        rows.append(&mut project_rows);
    }
    Ok(rows)
}
