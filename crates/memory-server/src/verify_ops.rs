//! Verification ledger for background quality gates.
//!
//! Heavy checks such as `gitleaks detect`, `cargo check`, `cargo clippy`, or
//! full test suites are run by an external harness. This module only records
//! and reads their results under `.tachi/runs/<flow_id>/verification.json` so
//! leaders, briefing, and safe-merge gates consume the same evidence.

use crate::shell_ops::{run_dir_for_flow_id, shell_runs_root};
use crate::{MemoryServer, TachiVerifyParams};
use chrono::{DateTime, Utc};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};

const LEDGER_FILE: &str = "verification.json";
const DEFAULT_STATUS_LIMIT: usize = 6;
const RECENT_SCAN_MAX: usize = 128;

fn now() -> String {
    Utc::now().to_rfc3339()
}

fn valid_status(status: &str) -> bool {
    matches!(
        status,
        "pending" | "running" | "passed" | "failed" | "skipped" | "stale"
    )
}

fn normalize_status(status: Option<&str>, default: &str) -> Result<String, String> {
    let value = status.unwrap_or(default).trim().to_ascii_lowercase();
    if valid_status(&value) {
        Ok(value)
    } else {
        Err(format!(
            "invalid verification status '{value}' (allowed: pending, running, passed, failed, skipped, stale)"
        ))
    }
}

fn slugify_check_id(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len().min(64));
    let mut last_dash = false;
    for c in raw.trim().to_ascii_lowercase().chars() {
        if c.is_ascii_alphanumeric() {
            out.push(c);
            last_dash = false;
        } else if !last_dash {
            out.push('-');
            last_dash = true;
        }
    }
    let trimmed: String = out.trim_matches('-').chars().take(64).collect();
    if trimmed.is_empty() {
        "check".to_string()
    } else {
        trimmed
    }
}

fn check_id_for(params: &TachiVerifyParams, command: Option<&str>) -> String {
    params
        .check_id
        .as_deref()
        .or(params.kind.as_deref())
        .or(command)
        .map(slugify_check_id)
        .unwrap_or_else(|| "check".to_string())
}

fn ledger_path_for_flow(flow_id: &str) -> Result<PathBuf, String> {
    Ok(run_dir_for_flow_id(flow_id)?.join(LEDGER_FILE))
}

fn read_json(path: &Path) -> Result<Option<Value>, String> {
    match std::fs::read_to_string(path) {
        Ok(raw) => serde_json::from_str(&raw)
            .map(Some)
            .map_err(|e| format!("parse {}: {e}", path.display())),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(format!("read {}: {e}", path.display())),
    }
}

fn write_json(path: &Path, value: &Value) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("create {}: {e}", parent.display()))?;
    }
    let raw =
        serde_json::to_string_pretty(value).map_err(|e| format!("serialize verification: {e}"))?;
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, raw).map_err(|e| format!("write {}: {e}", tmp.display()))?;
    std::fs::rename(&tmp, path).map_err(|e| format!("rename verification tmp: {e}"))?;
    Ok(())
}

fn empty_ledger(flow_id: &str) -> Value {
    json!({
        "flow_id": flow_id,
        "updated_at": now(),
        "overall": "pending",
        "items": [],
    })
}

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

fn record_items(params: &TachiVerifyParams, status: &str) -> Result<Value, String> {
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

fn parse_updated_at(value: &Value) -> Option<DateTime<Utc>> {
    value
        .get("updated_at")
        .and_then(Value::as_str)
        .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
        .map(|dt| dt.with_timezone(&Utc))
}

pub(crate) fn recent_verification_summaries(limit: usize) -> Value {
    let root = shell_runs_root();
    let Ok(read_dir) = std::fs::read_dir(root) else {
        return json!([]);
    };
    let scan_cap = limit.saturating_mul(4).clamp(32, RECENT_SCAN_MAX);
    let mut candidates: Vec<(std::time::SystemTime, PathBuf)> = Vec::new();
    for entry in read_dir.flatten() {
        let ledger_path = entry.path().join(LEDGER_FILE);
        if !ledger_path.exists() {
            continue;
        }
        let modified = ledger_path
            .metadata()
            .and_then(|m| m.modified())
            .or_else(|_| entry.metadata().and_then(|m| m.modified()))
            .unwrap_or(std::time::SystemTime::UNIX_EPOCH);
        candidates.push((modified, ledger_path));
    }
    candidates.sort_by(|a, b| b.0.cmp(&a.0));
    candidates.truncate(scan_cap);

    let mut rows: Vec<Value> = Vec::new();
    for (_, path) in candidates {
        let Ok(Some(ledger)) = read_json(&path) else {
            continue;
        };
        let items = ledger
            .get("items")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        let failed = items
            .iter()
            .filter(|i| i.get("status").and_then(Value::as_str) == Some("failed"))
            .count();
        let pending = items
            .iter()
            .filter(|i| {
                i.get("status")
                    .and_then(Value::as_str)
                    .is_some_and(|s| matches!(s, "pending" | "running" | "stale"))
            })
            .count();
        rows.push(json!({
            "flow_id": ledger.get("flow_id").and_then(Value::as_str).unwrap_or("?"),
            "pr_ref": ledger.get("pr_ref").cloned().unwrap_or(Value::Null),
            "head_sha": ledger.get("head_sha").cloned().unwrap_or(Value::Null),
            "overall": ledger.get("overall").and_then(Value::as_str).unwrap_or("pending"),
            "updated_at": ledger.get("updated_at").cloned().unwrap_or(Value::Null),
            "total": items.len(),
            "failed": failed,
            "pending": pending,
        }));
    }
    rows.sort_by(|a, b| parse_updated_at(b).cmp(&parse_updated_at(a)));
    rows.truncate(limit);
    json!(rows)
}

fn gate_for_status(
    params: &TachiVerifyParams,
    ledger: Option<&Value>,
) -> Result<Option<Value>, String> {
    if let (Some(flow_id), Some(head_sha), Some(_)) = (
        params.flow_id.as_deref(),
        params.head_sha.as_deref(),
        ledger,
    ) {
        evaluate_verification_gate(Some(flow_id), head_sha)
    } else {
        Ok(None)
    }
}

fn render_status(value: &Value) -> String {
    if let Some(rows) = value.get("runs").and_then(Value::as_array) {
        let mut out = vec!["## Tachi verify board".to_string()];
        if rows.is_empty() {
            out.push("_No verification ledgers found._".to_string());
        } else {
            for row in rows {
                let flow_id = row.get("flow_id").and_then(Value::as_str).unwrap_or("?");
                let overall = row
                    .get("overall")
                    .and_then(Value::as_str)
                    .unwrap_or("pending");
                let total = row.get("total").and_then(Value::as_u64).unwrap_or(0);
                let failed = row.get("failed").and_then(Value::as_u64).unwrap_or(0);
                let pending = row.get("pending").and_then(Value::as_u64).unwrap_or(0);
                let pr = row.get("pr_ref").and_then(Value::as_str).unwrap_or("");
                out.push(format!(
                    "- [{overall}] `{flow_id}`{} checks={total} failed={failed} pending={pending}",
                    if pr.is_empty() {
                        String::new()
                    } else {
                        format!(" `{pr}`")
                    }
                ));
            }
        }
        return out.join("\n");
    }

    let ledger = value.get("verification").unwrap_or(value);
    let flow_id = ledger.get("flow_id").and_then(Value::as_str).unwrap_or("?");
    let overall = ledger
        .get("overall")
        .and_then(Value::as_str)
        .unwrap_or("pending");
    let mut out = vec![
        "## Tachi verify status".to_string(),
        format!("flow_id: `{flow_id}`"),
        format!("overall: `{overall}`"),
    ];
    if let Some(gate) = value.get("gate") {
        out.push(format!(
            "gate: `{}`",
            gate.get("overall")
                .and_then(Value::as_str)
                .unwrap_or("unknown")
        ));
    }
    if let Some(items) = ledger.get("items").and_then(Value::as_array) {
        for item in items {
            let id = item.get("id").and_then(Value::as_str).unwrap_or("check");
            let status = item
                .get("status")
                .and_then(Value::as_str)
                .unwrap_or("pending");
            let summary = item.get("summary").and_then(Value::as_str).unwrap_or("");
            out.push(format!(
                "- [{status}] `{id}`{}",
                if summary.is_empty() {
                    String::new()
                } else {
                    format!(" - {summary}")
                }
            ));
        }
    }
    out.join("\n")
}

pub(crate) async fn handle_tachi_verify(
    _server: &MemoryServer,
    params: TachiVerifyParams,
) -> Result<String, String> {
    let action = params.action.trim().to_ascii_lowercase();
    let raw = match action.as_str() {
        "start" => record_items(&params, "pending")?,
        "record" => {
            let status = normalize_status(params.status.as_deref(), "passed")?;
            record_items(&params, &status)?
        }
        "status" | "board" => {
            if let Some(flow_id) = params.flow_id.as_deref() {
                let ledger = read_verification_ledger(flow_id)?;
                let gate = gate_for_status(&params, ledger.as_ref())?;
                json!({
                    "status": "completed",
                    "action": action,
                    "flow_id": flow_id,
                    "verification": ledger.unwrap_or_else(|| empty_ledger(flow_id)),
                    "gate": gate,
                })
            } else {
                json!({
                    "status": "completed",
                    "action": action,
                    "runs": recent_verification_summaries(params.limit.unwrap_or(DEFAULT_STATUS_LIMIT as u32) as usize),
                })
            }
        }
        _ => {
            return Err(format!(
                "Invalid action '{}'. Use 'start', 'record', 'status', or 'board'.",
                params.action
            ))
        }
    };

    if params
        .format
        .as_deref()
        .is_some_and(|format| format.eq_ignore_ascii_case("json"))
    {
        serde_json::to_string(&raw).map_err(|e| format!("serialize tachi_verify: {e}"))
    } else {
        Ok(render_status(&raw))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tool_params::TachiVerifyParams;

    fn params(action: &str) -> TachiVerifyParams {
        TachiVerifyParams {
            action: action.to_string(),
            format: Some("json".to_string()),
            flow_id: Some("flow_test-verify".to_string()),
            pr_ref: None,
            head_sha: None,
            check_id: None,
            kind: None,
            command: None,
            commands: vec![],
            status: None,
            exit_code: None,
            log_path: None,
            summary: None,
            cwd: None,
            required: None,
            limit: None,
        }
    }

    #[test]
    fn record_items_upserts_and_computes_overall() {
        let _guard = crate::shell_ops::tachi_run_root_env_lock()
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let tmp = tempfile::tempdir().unwrap();
        let original = std::env::var_os("TACHI_RUN_ROOT");
        std::env::set_var("TACHI_RUN_ROOT", tmp.path());

        let mut first = params("record");
        first.kind = Some("gitleaks".to_string());
        first.head_sha = Some("abc".to_string());
        first.status = Some("passed".to_string());
        record_items(&first, "passed").unwrap();

        let mut second = params("record");
        second.kind = Some("clippy".to_string());
        second.head_sha = Some("abc".to_string());
        second.status = Some("failed".to_string());
        let out = record_items(&second, "failed").unwrap();

        assert_eq!(out["verification"]["overall"], "failed");
        assert_eq!(out["verification"]["items"].as_array().unwrap().len(), 2);
        if let Some(v) = original {
            std::env::set_var("TACHI_RUN_ROOT", v);
        } else {
            std::env::remove_var("TACHI_RUN_ROOT");
        }
    }

    #[test]
    fn verification_gate_detects_failed_pending_and_stale_items() {
        let _guard = crate::shell_ops::tachi_run_root_env_lock()
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let tmp = tempfile::tempdir().unwrap();
        let original = std::env::var_os("TACHI_RUN_ROOT");
        std::env::set_var("TACHI_RUN_ROOT", tmp.path());

        let flow_id = "flow_test-verify";
        let path = ledger_path_for_flow(flow_id).unwrap();
        write_json(
            &path,
            &json!({
                "flow_id": flow_id,
                "overall": "failed",
                "items": [
                    {"id":"gitleaks","status":"passed","head_sha":"abc","required":true},
                    {"id":"clippy","status":"failed","head_sha":"abc","required":true},
                    {"id":"test","status":"running","head_sha":"abc","required":true},
                    {"id":"check","status":"passed","head_sha":"old","required":true}
                ]
            }),
        )
        .unwrap();

        let gate = evaluate_verification_gate(Some(flow_id), "abc")
            .unwrap()
            .unwrap();
        assert_eq!(gate["overall"], "failed");
        assert!(gate["reasons"]
            .as_array()
            .unwrap()
            .iter()
            .any(|r| r == "verification:clippy:failed"));
        assert!(gate["waiting_on"]
            .as_array()
            .unwrap()
            .iter()
            .any(|r| r == "verification:test:pending"));
        assert!(gate["waiting_on"]
            .as_array()
            .unwrap()
            .iter()
            .any(|r| r == "verification:check:stale"));
        if let Some(v) = original {
            std::env::set_var("TACHI_RUN_ROOT", v);
        } else {
            std::env::remove_var("TACHI_RUN_ROOT");
        }
    }

    #[test]
    fn verification_gate_treats_missing_head_sha_as_stale() {
        let _guard = crate::shell_ops::tachi_run_root_env_lock()
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let tmp = tempfile::tempdir().unwrap();
        let original = std::env::var_os("TACHI_RUN_ROOT");
        std::env::set_var("TACHI_RUN_ROOT", tmp.path());

        let flow_id = "flow_test-verify";
        let path = ledger_path_for_flow(flow_id).unwrap();
        write_json(
            &path,
            &json!({
                "flow_id": flow_id,
                "overall": "passed",
                "items": [
                    {"id":"gitleaks","status":"passed","required":true}
                ]
            }),
        )
        .unwrap();

        let gate = evaluate_verification_gate(Some(flow_id), "abc")
            .unwrap()
            .unwrap();
        assert_eq!(gate["overall"], "pending");
        assert!(gate["waiting_on"]
            .as_array()
            .unwrap()
            .iter()
            .any(|r| r == "verification:gitleaks:stale"));
        if let Some(v) = original {
            std::env::set_var("TACHI_RUN_ROOT", v);
        } else {
            std::env::remove_var("TACHI_RUN_ROOT");
        }
    }

    #[test]
    fn verification_gate_treats_skipped_required_without_head_sha_as_stale() {
        let _guard = crate::shell_ops::tachi_run_root_env_lock()
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let tmp = tempfile::tempdir().unwrap();
        let original = std::env::var_os("TACHI_RUN_ROOT");
        std::env::set_var("TACHI_RUN_ROOT", tmp.path());

        let flow_id = "flow_test-verify";
        let path = ledger_path_for_flow(flow_id).unwrap();
        write_json(
            &path,
            &json!({
                "flow_id": flow_id,
                "overall": "passed",
                "items": [
                    {"id":"gitleaks","status":"skipped","required":true}
                ]
            }),
        )
        .unwrap();

        let gate = evaluate_verification_gate(Some(flow_id), "abc")
            .unwrap()
            .unwrap();
        assert_eq!(gate["overall"], "pending");
        assert!(gate["waiting_on"]
            .as_array()
            .unwrap()
            .iter()
            .any(|r| r == "verification:gitleaks:stale"));
        if let Some(v) = original {
            std::env::set_var("TACHI_RUN_ROOT", v);
        } else {
            std::env::remove_var("TACHI_RUN_ROOT");
        }
    }
}
