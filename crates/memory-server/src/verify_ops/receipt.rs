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
        return params
            .checks
            .iter()
            .map(check_id_for_entry)
            .collect();
    }
    let command = params
        .command
        .as_deref()
        .or_else(|| params.commands.first().map(|s| s.as_str()));
    vec![check_id_for(params, command)]
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

fn single_recorded_status(ledger: &Value, check_id: &str) -> Option<String> {
    ledger.get("items").and_then(Value::as_array).and_then(|items| {
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
    let overall = value.get("overall").and_then(Value::as_str).unwrap_or("pending");
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
        let status = value.get("status").and_then(Value::as_str).unwrap_or("passed");
        out.push(format!("check_id: `{check_id}`"));
        out.push(format!("status: `{status}`"));
    }
    out.join("\n")
}