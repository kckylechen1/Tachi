use chrono::Utc;
use serde_json::{json, Value};
use std::path::Path;

use crate::dispatch_ops::dispatch_v2::append_trajectory_event;

use super::types::{AcpxEventSummary, ACPX_EVENTS_FILE};

pub(in crate::dispatch_ops) fn persist_acpx_events_and_map(
    workspace_dir: &Path,
    trajectory_path: &Path,
    dispatch_id: &str,
    agent: &str,
    output: &str,
    release_artifacts: bool,
) -> Result<AcpxEventSummary, String> {
    let events_file = workspace_dir.join(ACPX_EVENTS_FILE);
    let progress_path = workspace_dir.join("progress.jsonl");
    let mut raw_lines = Vec::new();
    let mut mapped_events = 0usize;
    let mut final_response = None;

    for (index, line) in output.lines().enumerate() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let event = serde_json::from_str::<Value>(trimmed).unwrap_or_else(|_| {
            json!({
                "event": "raw_text",
                "index": index,
                "text": trimmed,
            })
        });
        raw_lines.push(
            serde_json::to_string(&event)
                .map_err(|err| format!("Failed to serialize acpx event: {err}"))?,
        );
        if let Some(mapped) = map_acpx_event(dispatch_id, agent, &event) {
            if release_artifacts {
                let target_progress = mapped
                    .get("tachi_target")
                    .and_then(Value::as_str)
                    .is_some_and(|target| target == "progress");
                if target_progress {
                    append_trajectory_event(&progress_path, mapped.clone());
                } else {
                    append_trajectory_event(trajectory_path, mapped.clone());
                }
            }
            mapped_events += 1;
        }
        if final_response.is_none() && is_final_acpx_event(&event) {
            final_response = extract_event_text(&event);
        }
    }

    if raw_lines.is_empty() {
        raw_lines.push(
            serde_json::to_string(&json!({
                "event": "empty_output",
                "dispatch_id": dispatch_id,
            }))
            .map_err(|err| format!("Failed to serialize empty acpx event: {err}"))?,
        );
    }

    if release_artifacts {
        let raw_payload = format!("{}\n", raw_lines.join("\n"));
        crate::utils::write_owner_only_file_atomic(&events_file, raw_payload.as_bytes())
            .map_err(|err| format!("Failed to write {ACPX_EVENTS_FILE}: {err}"))?;
        append_trajectory_event(
            trajectory_path,
            json!({
                "event": "acpx_events_persisted",
                "dispatch_id": dispatch_id,
                "agent": agent,
                "events_file": events_file.to_string_lossy(),
                "raw_event_count": raw_lines.len(),
                "mapped_event_count": mapped_events,
                "timestamp": Utc::now().to_rfc3339(),
            }),
        );
    }

    Ok(AcpxEventSummary {
        events_file,
        mapped_events,
        final_response,
    })
}

fn map_acpx_event(dispatch_id: &str, agent: &str, event: &Value) -> Option<Value> {
    let kind = acpx_event_kind(event);
    let lower = kind.to_ascii_lowercase();
    let mapped_event = if lower.contains("thinking") || lower.contains("message") {
        "acpx_message"
    } else if lower.contains("tool") {
        "acpx_tool_event"
    } else if lower.contains("diff") || lower.contains("edit") {
        "acpx_diff_event"
    } else if lower.contains("permission")
        || lower.contains("approval")
        || lower.contains("denial")
        || lower.contains("deny")
    {
        "acpx_permission_event"
    } else if lower.contains("final")
        || lower.contains("end_turn")
        || lower.contains("result")
        || lower.contains("complete")
    {
        "acpx_final_event"
    } else if lower.contains("cancel")
        || lower.contains("status")
        || lower.contains("dead")
        || lower.contains("error")
    {
        "acpx_lifecycle_event"
    } else {
        return None;
    };
    let target = if mapped_event == "acpx_message" {
        "progress"
    } else {
        "trajectory"
    };
    Some(json!({
        "event": mapped_event,
        "dispatch_id": dispatch_id,
        "agent": agent,
        "acpx_event": kind,
        "text": extract_event_text(event),
        "tachi_target": target,
        "timestamp": Utc::now().to_rfc3339(),
    }))
}

fn acpx_event_kind(event: &Value) -> String {
    ["event", "type", "kind", "name"]
        .iter()
        .find_map(|key| event.get(*key).and_then(Value::as_str))
        .unwrap_or("unknown")
        .to_string()
}

fn is_final_acpx_event(event: &Value) -> bool {
    let lower = acpx_event_kind(event).to_ascii_lowercase();
    lower.contains("final")
        || lower.contains("end_turn")
        || lower.contains("result")
        || lower.contains("complete")
}

fn extract_event_text(event: &Value) -> Option<String> {
    for key in [
        "final_response",
        "result",
        "message",
        "content",
        "text",
        "output",
    ] {
        if let Some(text) = event.get(key).and_then(Value::as_str) {
            if !text.trim().is_empty() {
                return Some(text.to_string());
            }
        }
    }
    None
}
