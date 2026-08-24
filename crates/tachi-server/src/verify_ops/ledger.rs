use super::receipt::validate_record_params;
use super::storage::{
    check_id_for, check_id_for_entry, empty_ledger, ledger_path_for_flow, normalize_status, now,
    read_json, write_json,
};
use super::*;
use crate::TachiVerifyCheckItem;

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

fn base_item_from_check(entry: &TachiVerifyCheckItem, status: &str) -> Result<Value, String> {
    let status = normalize_status(Some(&entry.status), status)?;
    let id = check_id_for_entry(entry);
    let mut item = json!({
        "id": id,
        "kind": entry.kind,
        "status": status,
        "required": entry.required.unwrap_or(true),
        // #1454: server-forced provenance. The batch checks[] writer goes
        // through this same path, so no caller-supplied authority key can
        // survive into the ledger.
        "source": CALLER_ASSERTED_SOURCE,
        "updated_at": now(),
    });
    if let Some(command) = entry.command.as_deref().filter(|s| !s.trim().is_empty()) {
        item["command"] = json!(command);
    }
    if let Some(head_sha) = entry.head_sha.as_deref().filter(|s| !s.trim().is_empty()) {
        item["head_sha"] = json!(head_sha);
    }
    if let Some(summary) = entry.summary.as_deref().filter(|s| !s.trim().is_empty()) {
        item["summary"] = json!(summary);
    }
    Ok(item)
}

fn base_item(params: &TachiVerifyParams, command: Option<&str>, status: &str) -> Value {
    let id = check_id_for(params, command);
    let mut item = json!({
        "id": id,
        "kind": params.kind.as_deref().unwrap_or(&id),
        "status": status,
        "required": params.required.unwrap_or(true),
        // #1454: server-forced provenance. Caller-supplied `source`,
        // `evidence`, `exit_code`, `log_path`, `ran_at`, `duration_ms` are
        // stripped/overwritten here — a caller must not smuggle authority
        // fields through record/start. Server-run items are written by the
        // executor with their own `server_run:<kind>` source (slice 2).
        "source": CALLER_ASSERTED_SOURCE,
        "updated_at": now(),
    });
    if let Some(command) = command.filter(|s| !s.trim().is_empty()) {
        item["command"] = json!(command);
    }
    if let Some(head_sha) = params.head_sha.as_deref().filter(|s| !s.trim().is_empty()) {
        item["head_sha"] = json!(head_sha);
    }
    if let Some(summary) = params.summary.as_deref().filter(|s| !s.trim().is_empty()) {
        item["summary"] = json!(summary);
    }
    if let Some(cwd) = params.cwd.as_deref().filter(|s| !s.trim().is_empty()) {
        item["cwd"] = json!(cwd);
    }
    // Note: `params.exit_code` / `params.log_path` are deliberately not
    // persisted — they are caller-authored fields with no authority value.
    item
}

fn read_or_new_ledger(flow_id: &str) -> Result<Value, String> {
    Ok(read_json(&ledger_path_for_flow(flow_id)?)?.unwrap_or_else(|| empty_ledger(flow_id)))
}

pub(crate) fn record_items(params: &TachiVerifyParams, status: &str) -> Result<Value, String> {
    validate_record_params(params)?;
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

    if !params.checks.is_empty() {
        for entry in &params.checks {
            upsert_item(&mut ledger, base_item_from_check(entry, status)?);
        }
    } else {
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

/// #1454 slice 2: internal server-run item write.
///
/// This is the ONLY path that persists a `server_run:<kind>` item. It
/// deliberately bypasses the caller-facing record/start strip path
/// (`base_item`/`record_items`): the item JSON is built by the executor
/// entirely from server-observed state (spawn exit code, `git rev-parse HEAD`,
/// the run log the server itself wrote), so there is no caller-authored field
/// to strip. `source` starts with [`super::SERVER_RUN_SOURCE_PREFIX`] by
/// construction — the gate's merge-authority class.
pub(crate) fn record_server_run_item(flow_id: &str, item: Value) -> Result<Value, String> {
    let path = ledger_path_for_flow(flow_id)?;
    let mut ledger = read_or_new_ledger(flow_id)?;
    ledger["flow_id"] = json!(flow_id);
    upsert_item(&mut ledger, item);
    refresh_overall(&mut ledger);
    write_json(&path, &ledger)?;
    Ok(json!({
        "status": "completed",
        "action": "run",
        "flow_id": flow_id,
        "ledger_path": path.display().to_string(),
        "verification": ledger,
    }))
}
