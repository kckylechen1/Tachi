pub(in crate::hub_ops::security_scan) fn risk_rank(risk: &str) -> u8 {
    match risk.trim().to_ascii_lowercase().as_str() {
        "high" => 3,
        "medium" => 2,
        _ => 1,
    }
}

pub(in crate::hub_ops::security_scan) fn normalize_risk(risk: &str) -> &'static str {
    match risk_rank(risk) {
        3 => "high",
        2 => "medium",
        _ => "low",
    }
}

pub(in crate::hub_ops) fn normalize_review_status(status: &str) -> Option<&'static str> {
    match status.trim().to_ascii_lowercase().as_str() {
        "pending" => Some("pending"),
        "approved" => Some("approved"),
        "rejected" => Some("rejected"),
        _ => None,
    }
}

pub(in crate::hub_ops::security_scan) fn findings_to_vec(
    value: Option<&serde_json::Value>,
) -> Vec<String> {
    value
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|x| match x {
                    serde_json::Value::String(s) => Some(s.trim().to_string()),
                    serde_json::Value::Object(map) => map
                        .get("finding")
                        .or_else(|| map.get("issue"))
                        .or_else(|| map.get("signal"))
                        .and_then(|v| v.as_str())
                        .map(|s| s.trim().to_string()),
                    _ => None,
                })
                .filter(|s| !s.is_empty())
                .collect::<Vec<String>>()
        })
        .unwrap_or_default()
}

pub(in crate::hub_ops::security_scan) fn signals_to_vec(
    value: Option<&serde_json::Value>,
) -> Vec<String> {
    value
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|x| match x {
                    serde_json::Value::String(s) => Some(s.trim().to_string()),
                    serde_json::Value::Object(map) => map
                        .get("signal")
                        .or_else(|| map.get("id"))
                        .or_else(|| map.get("name"))
                        .and_then(|v| v.as_str())
                        .map(|s| s.trim().to_string()),
                    _ => None,
                })
                .filter(|s| !s.is_empty())
                .collect::<Vec<String>>()
        })
        .unwrap_or_default()
}

pub(in crate::hub_ops::security_scan) fn merge_findings(
    static_findings: &[String],
    llm_findings: &[String],
) -> Vec<String> {
    let mut seen = std::collections::HashSet::new();
    let mut merged = Vec::new();
    for item in static_findings.iter().chain(llm_findings.iter()) {
        if seen.insert(item.as_str()) {
            merged.push(item.clone());
        }
    }
    merged
}
