use super::storage::{check_id_for, check_id_for_entry};
use super::*;
use crate::facade_memory_ops::wants_full_format;

pub(super) fn validate_record_params(params: &TachiVerifyParams) -> Result<(), String> {
    if params.checks.is_empty() {
        return Ok(());
    }
    let has_single = params.check_id.is_some()
        || params.kind.is_some()
        || params.command.is_some()
        || !params.commands.is_empty();
    if has_single {
        return Err(
            "cannot combine checks array with single-check fields (check_id, kind, command, commands)"
                .into(),
        );
    }
    Ok(())
}

pub(super) fn recorded_check_ids(params: &TachiVerifyParams) -> Vec<String> {
    if !params.checks.is_empty() {
        return params.checks.iter().map(check_id_for_entry).collect();
    }
    let commands = if params.commands.is_empty() {
        vec![params.command.as_deref()]
    } else {
        params.commands.iter().map(|s| Some(s.as_str())).collect()
    };
    commands
        .into_iter()
        .map(|command| check_id_for(params, command))
        .collect()
}

pub(super) fn shape_record_response(
    raw: &Value,
    params: &TachiVerifyParams,
    recorded_ids: &[String],
) -> Value {
    if wants_full_format(params.format.as_deref()) {
        return raw.clone();
    }

    let ledger = raw
        .get("verification")
        .cloned()
        .unwrap_or_else(|| Value::Object(serde_json::Map::new()));
    let flow_id = raw
        .get("flow_id")
        .and_then(Value::as_str)
        .or_else(|| ledger.get("flow_id").and_then(Value::as_str))
        .unwrap_or("");
    let overall = ledger
        .get("overall")
        .and_then(Value::as_str)
        .unwrap_or("pending");

    if recorded_ids.len() > 1 {
        return json!({
            "ok": true,
            "flow_id": flow_id,
            "check_ids": recorded_ids,
            "overall": overall,
        });
    }

    let check_id = recorded_ids.first().map(String::as_str).unwrap_or("check");
    let status = single_recorded_status(&ledger, check_id)
        .or_else(|| params.status.clone())
        .unwrap_or_else(|| "passed".to_string());

    json!({
        "ok": true,
        "flow_id": flow_id,
        "check_id": check_id,
        "status": status,
        "overall": overall,
    })
}

/// F1 (#527/#913): default `status`/`board` responses omit the full ledger.
/// Use `format=full` for the complete verification board (including passed rows).
pub(super) fn shape_status_response(raw: &Value, params: &TachiVerifyParams) -> Value {
    if wants_full_format(params.format.as_deref()) {
        return raw.clone();
    }

    // Multi-run board listing (no flow_id): keep compact row summaries.
    if let Some(runs) = raw.get("runs").and_then(Value::as_array) {
        let compact_runs: Vec<Value> = runs
            .iter()
            .map(|row| {
                json!({
                    "flow_id": row.get("flow_id").cloned().unwrap_or(Value::Null),
                    "overall": row.get("overall").cloned().unwrap_or(json!("pending")),
                    "total": row.get("total").cloned().unwrap_or(json!(0)),
                    "failed": row.get("failed").cloned().unwrap_or(json!(0)),
                    "pending": row.get("pending").cloned().unwrap_or(json!(0)),
                    "pr_ref": row.get("pr_ref").cloned().unwrap_or(Value::Null),
                })
            })
            .collect();
        return json!({
            "ok": true,
            "action": raw.get("action").cloned().unwrap_or(json!("status")),
            "runs": compact_runs,
            "note": "compact board; format=full for full ledgers",
        });
    }

    let ledger = raw
        .get("verification")
        .cloned()
        .unwrap_or_else(|| Value::Object(serde_json::Map::new()));
    let flow_id = raw
        .get("flow_id")
        .and_then(Value::as_str)
        .or_else(|| ledger.get("flow_id").and_then(Value::as_str))
        .unwrap_or("");
    let overall = ledger
        .get("overall")
        .and_then(Value::as_str)
        .unwrap_or("pending");

    let items = ledger
        .get("items")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let mut passed = 0u64;
    let mut failed = 0u64;
    let mut pending = 0u64;
    let mut problems: Vec<Value> = Vec::new();
    for item in &items {
        let status = item
            .get("status")
            .and_then(Value::as_str)
            .unwrap_or("pending");
        match status {
            "passed" | "skipped" => passed += 1,
            "failed" | "stale" => {
                failed += 1;
                problems.push(json!({
                    "id": item.get("id").cloned().unwrap_or(json!("check")),
                    "status": status,
                    "summary": item.get("summary").cloned().unwrap_or(Value::Null),
                }));
            }
            _ => {
                pending += 1;
                problems.push(json!({
                    "id": item.get("id").cloned().unwrap_or(json!("check")),
                    "status": status,
                    "summary": item.get("summary").cloned().unwrap_or(Value::Null),
                }));
            }
        }
    }

    let mut out = json!({
        "ok": true,
        "flow_id": flow_id,
        "overall": overall,
        "counts": {
            "total": items.len() as u64,
            "passed_or_skipped": passed,
            "failed_or_stale": failed,
            "pending": pending,
        },
        "problems": problems,
        "note": "compact status (problems only); format=full for full verification board",
    });
    if let Some(gate) = raw.get("gate") {
        out.as_object_mut().expect("object").insert(
            "gate".to_string(),
            json!({
                "overall": gate.get("overall").cloned().unwrap_or(json!("unknown")),
                "reasons": gate.get("reasons").cloned().unwrap_or(json!([])),
            }),
        );
    }
    out
}

fn single_recorded_status(ledger: &Value, check_id: &str) -> Option<String> {
    ledger
        .get("items")
        .and_then(Value::as_array)
        .and_then(|items| {
            items.iter().find_map(|item| {
                (item.get("id").and_then(Value::as_str) == Some(check_id))
                    .then(|| {
                        item.get("status")
                            .and_then(Value::as_str)
                            .map(str::to_string)
                    })
                    .flatten()
            })
        })
}

pub(super) fn render_record_receipt(value: &Value) -> String {
    let flow_id = value.get("flow_id").and_then(Value::as_str).unwrap_or("?");
    let overall = value
        .get("overall")
        .and_then(Value::as_str)
        .unwrap_or("pending");
    let mut out = vec![
        "## Tachi verify receipt".to_string(),
        format!("flow_id: `{flow_id}`"),
        format!("overall: `{overall}`"),
    ];
    if let Some(ids) = value.get("check_ids").and_then(Value::as_array) {
        let joined = ids
            .iter()
            .filter_map(Value::as_str)
            .map(|id| format!("`{id}`"))
            .collect::<Vec<_>>()
            .join(", ");
        out.push(format!("check_ids: {joined}"));
    } else if let Some(check_id) = value.get("check_id").and_then(Value::as_str) {
        let status = value
            .get("status")
            .and_then(Value::as_str)
            .unwrap_or("passed");
        out.push(format!("check_id: `{check_id}`"));
        out.push(format!("status: `{status}`"));
    }
    out.join("\n")
}
