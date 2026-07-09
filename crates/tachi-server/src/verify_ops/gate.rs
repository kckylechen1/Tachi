use super::ledger::read_verification_ledger;
use super::*;

fn item_is_stale(item: &Value, head_sha: &str) -> bool {
    match item.get("head_sha").and_then(Value::as_str) {
        Some(item_sha) if !item_sha.is_empty() => item_sha != head_sha,
        _ => true,
    }
}

pub(crate) fn evaluate_verification_gate(
    flow_id: Option<&str>,
    current_head_sha: &str,
) -> Result<Option<Value>, String> {
    let Some(flow_id) = flow_id else {
        return Ok(None);
    };
    let Some(ledger) = read_verification_ledger(flow_id)? else {
        return Ok(None);
    };
    let items = ledger
        .get("items")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let required: Vec<Value> = items
        .into_iter()
        .filter(|item| {
            item.get("required")
                .and_then(Value::as_bool)
                .unwrap_or(true)
        })
        .collect();
    if required.is_empty() {
        return Ok(Some(json!({
            "flow_id": flow_id,
            "overall": "not_required",
            "required_total": 0,
            "passed": [],
            "failed": [],
            "pending": [],
            "stale": [],
            "waiting_on": [],
        })));
    }

    let mut passed = Vec::new();
    let mut failed = Vec::new();
    let mut pending = Vec::new();
    let mut stale = Vec::new();

    for item in required {
        let id = item
            .get("id")
            .and_then(Value::as_str)
            .unwrap_or("check")
            .to_string();
        if item_is_stale(&item, current_head_sha) {
            stale.push(id);
            continue;
        }
        match item
            .get("status")
            .and_then(Value::as_str)
            .unwrap_or("pending")
        {
            "passed" | "skipped" => passed.push(id),
            "failed" => failed.push(id),
            "stale" => stale.push(id),
            _ => pending.push(id),
        }
    }

    let mut waiting_on: Vec<String> = pending
        .iter()
        .map(|id| format!("verification:{id}:pending"))
        .collect();
    waiting_on.extend(stale.iter().map(|id| format!("verification:{id}:stale")));
    let reasons: Vec<String> = failed
        .iter()
        .map(|id| format!("verification:{id}:failed"))
        .collect();
    let overall = if !failed.is_empty() {
        "failed"
    } else if !pending.is_empty() || !stale.is_empty() {
        "pending"
    } else {
        "passed"
    };

    Ok(Some(json!({
        "flow_id": flow_id,
        "overall": overall,
        "required_total": passed.len() + failed.len() + pending.len() + stale.len(),
        "current_head_sha": current_head_sha,
        "passed": passed,
        "failed": failed,
        "pending": pending,
        "stale": stale,
        "waiting_on": waiting_on,
        "reasons": reasons,
        "ledger_updated_at": ledger.get("updated_at").cloned().unwrap_or(Value::Null),
    })))
}
