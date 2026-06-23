use super::helpers::{findings_to_vec, merge_findings, normalize_risk, risk_rank, signals_to_vec};
use chrono::Utc;

pub(in crate::hub_ops) fn merge_skill_scans(
    static_scan: &serde_json::Value,
    llm_scan: Option<&serde_json::Value>,
) -> serde_json::Value {
    let static_risk = static_scan
        .get("risk")
        .and_then(|v| v.as_str())
        .unwrap_or("low");
    let static_blocked = static_scan
        .get("blocked")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let static_findings = findings_to_vec(static_scan.get("findings"));
    let static_signals = merge_findings(
        &signals_to_vec(static_scan.get("signals")),
        &signals_to_vec(static_scan.get("dangerous_signals")),
    );

    let mut llm_risk = "low";
    let mut llm_blocked = false;
    let mut llm_findings: Vec<String> = Vec::new();
    let mut llm_signals: Vec<String> = Vec::new();
    let mut llm_status = "skipped".to_string();
    let mut llm_meta = serde_json::json!({});

    if let Some(scan) = llm_scan {
        llm_status = scan
            .get("status")
            .and_then(|v| v.as_str())
            .unwrap_or("unknown")
            .to_string();
        llm_meta = scan.clone();

        if llm_status == "ok" {
            if let Some(result) = scan.get("result") {
                llm_risk = result.get("risk").and_then(|v| v.as_str()).unwrap_or("low");
                llm_blocked = result
                    .get("blocked")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false);
                llm_findings = findings_to_vec(result.get("findings"));
                llm_signals = merge_findings(
                    &signals_to_vec(result.get("signals")),
                    &signals_to_vec(result.get("dangerous_signals")),
                );
            }
        }
    }

    let final_risk = if risk_rank(llm_risk) > risk_rank(static_risk) {
        llm_risk
    } else {
        static_risk
    };
    let blocked = static_blocked || llm_blocked || normalize_risk(final_risk) == "high";
    let findings = merge_findings(&static_findings, &llm_findings);
    let signals = merge_findings(&static_signals, &llm_signals);

    serde_json::json!({
        "scanned_at": Utc::now().to_rfc3339(),
        "risk": normalize_risk(final_risk),
        "blocked": blocked,
        "signals": signals,
        "findings": findings,
        "engine": "hybrid-static-llm-v1",
        "static": static_scan,
        "llm": llm_meta,
        "llm_status": llm_status
    })
}
