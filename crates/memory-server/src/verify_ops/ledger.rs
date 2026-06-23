use super::storage::{
    check_id_for, empty_ledger, ledger_path_for_flow, now, read_json, write_json,
};
use super::*;

fn compute_overall(items: &[Value]) -> &'static str {
    let required: Vec<&Value> = items
        .iter()
        .filter(|item| {
            item.get("required")
                .and_then(Value::as_bool)
                .unwrap_or(true)
        })
        .collect();
    let scoped = if required.is_empty() {
        items.iter().collect::<Vec<_>>()
    } else {
        required
    };
    if scoped.is_empty() {
        return "pending";
    }
    if scoped.iter().any(|item| {
        item.get("status")
            .and_then(Value::as_str)
            .is_some_and(|s| matches!(s, "failed" | "stale"))
    }) {
        return "failed";
    }
    if scoped.iter().any(|item| {
        item.get("status")
            .and_then(Value::as_str)
            .is_some_and(|s| matches!(s, "pending" | "running"))
    }) {
        return "pending";
    }
    "passed"
}

fn refresh_overall(ledger: &mut Value) {
    let items = ledger
        .get("items")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    ledger["overall"] = json!(compute_overall(&items));
    ledger["updated_at"] = json!(now());
}

fn upsert_item(ledger: &mut Value, item: Value) {
    let id = item.get("id").and_then(Value::as_str).unwrap_or("check");
    if !ledger.get("items").is_some_and(Value::is_array) {
        ledger["items"] = json!([]);
    }
    let items = ledger
        .get_mut("items")
        .and_then(Value::as_array_mut)
        .expect("ensured items array");
    if let Some(existing) = items
        .iter_mut()
        .find(|row| row.get("id").and_then(Value::as_str) == Some(id))
    {
        *existing = item;
    } else {
        items.push(item);
    }
}

fn base_item(params: &TachiVerifyParams, command: Option<&str>, status: &str) -> Value {
    let id = check_id_for(params, command);
    let mut item = json!({
        "id": id,
        "kind": params.kind.as_deref().unwrap_or(&id),
        "status": status,
        "required": params.required.unwrap_or(true),
        "updated_at": now(),
    });
    if let Some(command) = command.filter(|s| !s.trim().is_empty()) {
        item["command"] = json!(command);
    }
    if let Some(head_sha) = params.head_sha.as_deref().filter(|s| !s.trim().is_empty()) {
        item["head_sha"] = json!(head_sha);
    }
    if let Some(exit_code) = params.exit_code {
        item["exit_code"] = json!(exit_code);
    }
    if let Some(log_path) = params.log_path.as_deref().filter(|s| !s.trim().is_empty()) {
        item["log_path"] = json!(log_path);
    }
    if let Some(summary) = params.summary.as_deref().filter(|s| !s.trim().is_empty()) {
        item["summary"] = json!(summary);
    }
    if let Some(cwd) = params.cwd.as_deref().filter(|s| !s.trim().is_empty()) {
        item["cwd"] = json!(cwd);
    }
    item
}

fn read_or_new_ledger(flow_id: &str) -> Result<Value, String> {
    Ok(read_json(&ledger_path_for_flow(flow_id)?)?.unwrap_or_else(|| empty_ledger(flow_id)))
}

pub(super) fn record_items(params: &TachiVerifyParams, status: &str) -> Result<Value, String> {
    let flow_id = params
        .flow_id
        .as_deref()
        .ok_or_else(|| "flow_id is required for tachi_verify start/record".to_string())?;
    let path = ledger_path_for_flow(flow_id)?;
    let mut ledger = read_or_new_ledger(flow_id)?;
    ledger["flow_id"] = json!(flow_id);
    if let Some(pr_ref) = params.pr_ref.as_deref().filter(|s| !s.trim().is_empty()) {
        ledger["pr_ref"] = json!(pr_ref);
    }
    if let Some(head_sha) = params.head_sha.as_deref().filter(|s| !s.trim().is_empty()) {
        ledger["head_sha"] = json!(head_sha);
    }

    let commands = if params.commands.is_empty() {
        vec![params.command.as_deref()]
    } else {
        params.commands.iter().map(|s| Some(s.as_str())).collect()
    };
    for command in commands {
        if command.is_none() && params.check_id.is_none() && params.kind.is_none() {
            return Err(
                "command, kind, or check_id is required for tachi_verify start/record".into(),
            );
        }
        upsert_item(&mut ledger, base_item(params, command, status));
    }
    refresh_overall(&mut ledger);
    write_json(&path, &ledger)?;
    Ok(json!({
        "status": "completed",
        "action": params.action,
        "flow_id": flow_id,
        "ledger_path": path.display().to_string(),
        "verification": ledger,
    }))
}

pub(crate) fn read_verification_ledger(flow_id: &str) -> Result<Option<Value>, String> {
    read_json(&ledger_path_for_flow(flow_id)?)
}
