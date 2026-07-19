use chrono::{DateTime, Duration as ChronoDuration, Utc};
use serde_json::Value;
use std::path::Path;

const RUN_STALE_FALLBACK_SECS: i64 = 30 * 60;
const RUN_STALE_GRACE_SECS: i64 = 60;
const RUN_STALE_MAX_TIMEOUT_SECS: i64 = 30 * 24 * 60 * 60;

pub(super) fn map_filter_state(state_filter: &str) -> &str {
    match state_filter {
        "working" => "TASK_STATE_WORKING",
        "completed" => "TASK_STATE_COMPLETED",
        "failed" => "TASK_STATE_FAILED",
        "pending" => "TASK_STATE_PENDING",
        "input_required" => "TASK_STATE_INPUT_REQUIRED",
        "canceled" => "TASK_STATE_CANCELED",
        other => other,
    }
}

/// tachi#1173 item 3: terminal states that fold into a per-state count row
/// on the default (unfiltered, non-verbose) board view. Mirrors
/// `crate::tools::is_terminal_task_state` (private to that module, used by
/// `wait`/`status`/`cancel`'s own terminal-poll decision) -- duplicated here
/// rather than widening that fn's visibility across the `tools`/
/// `dispatch_ops` module boundary for a one-line predicate both call sites
/// already independently agree on.
pub(super) fn is_terminal_state(state: &str) -> bool {
    matches!(
        state,
        "TASK_STATE_COMPLETED" | "TASK_STATE_FAILED" | "TASK_STATE_CANCELED"
    )
}

/// INPUT_REQUIRED is normally actionable plan-review work. Only the explicit
/// durable marker written for a `tachi_complete(partial)` outcome turns that
/// displayed state into terminal history.
pub(super) fn is_terminal_state_with_closure_kind(state: &str, closure_kind: Option<&str>) -> bool {
    is_terminal_state(state)
        || (state == "TASK_STATE_INPUT_REQUIRED" && closure_kind == Some("partial"))
}

pub(super) fn state_matches_filter_with_closure_kind(
    state_filter: &str,
    state: &str,
    closure_kind: Option<&str>,
) -> bool {
    state_matches_filter(state_filter, state)
        && !(state_filter == "active" && is_terminal_state_with_closure_kind(state, closure_kind))
}

pub(super) fn state_matches_filter(state_filter: &str, state: &str) -> bool {
    match state_filter {
        "all" => true,
        "active" => matches!(
            state,
            "TASK_STATE_WORKING"
                | "TASK_STATE_RUNNING"
                | "TASK_STATE_PENDING"
                | "TASK_STATE_INPUT_REQUIRED"
                | "TASK_STATE_PENDING_REVIEW"
        ),
        other => state == map_filter_state(other),
    }
}

pub(super) fn status_state(status: &serde_json::Value, result_written: bool) -> &'static str {
    if let Some(state) = status.get("state").and_then(|s| s.as_str()) {
        return match state {
            "TASK_STATE_COMPLETED" => "TASK_STATE_COMPLETED",
            "TASK_STATE_FAILED" => "TASK_STATE_FAILED",
            "TASK_STATE_PENDING" => "TASK_STATE_PENDING",
            "TASK_STATE_INPUT_REQUIRED" => "TASK_STATE_INPUT_REQUIRED",
            "TASK_STATE_CANCELED" => "TASK_STATE_CANCELED",
            _ => "TASK_STATE_WORKING",
        };
    }
    if status.get("plan_review_status").and_then(|s| s.as_str()) == Some("pending_review") {
        return "TASK_STATE_INPUT_REQUIRED";
    }
    match status.get("exit_code") {
        Some(serde_json::Value::Number(n)) if n.as_i64() == Some(0) => "TASK_STATE_COMPLETED",
        Some(serde_json::Value::Number(_)) => "TASK_STATE_FAILED",
        _ if result_written => "TASK_STATE_COMPLETED",
        _ => "TASK_STATE_WORKING",
    }
}

fn status_timeout_secs(status: &Value) -> Option<i64> {
    status
        .get("timeout_secs")
        .and_then(Value::as_i64)
        .filter(|secs| *secs > 0)
        .map(|secs| secs.min(RUN_STALE_MAX_TIMEOUT_SECS))
}

pub(super) fn stale_after_secs(status: &Value) -> i64 {
    status_timeout_secs(status)
        .map(|secs| secs.saturating_add(RUN_STALE_GRACE_SECS))
        .unwrap_or(RUN_STALE_FALLBACK_SECS)
}

pub(super) fn parse_status_updated_at(status: &Value, status_path: &Path) -> Option<DateTime<Utc>> {
    status
        .get("updated_at")
        .and_then(Value::as_str)
        .and_then(|raw| DateTime::parse_from_rfc3339(raw).ok())
        .map(|dt| dt.with_timezone(&Utc))
        .or_else(|| {
            std::fs::metadata(status_path)
                .ok()
                .and_then(|m| m.modified().ok())
                .map(DateTime::<Utc>::from)
        })
}

fn is_unresolved_exit(status: &Value) -> bool {
    match status.get("exit_code") {
        Some(Value::Number(_)) => false,
        Some(Value::Null) | None => true,
        _ => true,
    }
}

pub(super) fn is_abandoned_working_run(
    status: &Value,
    result_written: bool,
    updated_at: Option<DateTime<Utc>>,
    now: DateTime<Utc>,
) -> bool {
    if status_state(status, result_written) != "TASK_STATE_WORKING" {
        return false;
    }
    if !is_unresolved_exit(status) {
        return false;
    }
    let Some(updated_at) = updated_at else {
        return false;
    };
    now.signed_duration_since(updated_at) > ChronoDuration::seconds(stale_after_secs(status))
}

pub(super) fn mark_abandoned_kanban_task(task: &mut Value, now: DateTime<Utc>) {
    if task.get("source").and_then(Value::as_str) != Some("kanban") {
        return;
    }
    let Some(state) = task.get("state").and_then(Value::as_str) else {
        return;
    };
    if !state_matches_filter_with_closure_kind(
        "active",
        state,
        task.get("closure_kind").and_then(Value::as_str),
    ) {
        return;
    }
    let Some(updated_at) = task
        .get("updated_at")
        .and_then(Value::as_str)
        .and_then(|raw| DateTime::parse_from_rfc3339(raw).ok())
        .map(|dt| dt.with_timezone(&Utc))
    else {
        return;
    };
    let stale_after = stale_after_secs(task);
    if now.signed_duration_since(updated_at) <= ChronoDuration::seconds(stale_after) {
        return;
    }
    let Some(obj) = task.as_object_mut() else {
        return;
    };
    obj.insert(
        "state".to_string(),
        Value::String("TASK_STATE_FAILED".to_string()),
    );
    obj.insert("stale".to_string(), Value::Bool(true));
    obj.insert(
        "stale_reason".to_string(),
        Value::String(format!(
            "kanban card stayed active for more than {stale_after}s without a live run ledger"
        )),
    );
    obj.insert(
        "state_source".to_string(),
        Value::String("kanban_stale_timeout".to_string()),
    );
}
