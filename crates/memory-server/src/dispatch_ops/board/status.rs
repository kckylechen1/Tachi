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
    if result_written || !is_unresolved_exit(status) {
        return false;
    }
    let Some(updated_at) = updated_at else {
        return false;
    };
    now.signed_duration_since(updated_at) > ChronoDuration::seconds(stale_after_secs(status))
}
