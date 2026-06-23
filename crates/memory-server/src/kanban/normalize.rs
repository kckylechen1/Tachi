use super::types::{default_card_priority, default_card_type};
use super::*;

pub(super) fn normalize_agent_id(value: &str) -> String {
    value.trim().to_ascii_lowercase()
}

pub(super) fn normalize_card_priority(value: &str) -> String {
    match value.trim().to_ascii_lowercase().as_str() {
        "low" => "low".to_string(),
        "medium" => "medium".to_string(),
        "high" => "high".to_string(),
        "critical" => "critical".to_string(),
        _ => default_card_priority(),
    }
}

pub(super) fn normalize_card_type(value: &str) -> String {
    match value.trim().to_ascii_lowercase() {
        s if matches!(
            s.as_str(),
            "request" | "report" | "alert" | "handoff" | "ack" | "progress" | "result"
        ) =>
        {
            s
        }
        _ => default_card_type(),
    }
}

pub(super) fn normalize_card_status(value: &str) -> Option<String> {
    match value.trim().to_ascii_lowercase().as_str() {
        "open" => Some("open".to_string()),
        "acknowledged" => Some("acknowledged".to_string()),
        "resolved" => Some("resolved".to_string()),
        "expired" => Some("expired".to_string()),
        _ => None,
    }
}

pub(super) fn kanban_priority_rank(priority: &str) -> u8 {
    match priority {
        "critical" => 0,
        "high" => 1,
        "medium" => 2,
        _ => 3,
    }
}

pub(super) fn kanban_priority_importance(priority: &str) -> f64 {
    match priority {
        "critical" => 1.0,
        "high" => 0.9,
        "medium" => 0.75,
        _ => 0.6,
    }
}

pub(super) fn card_metadata_str(entry: &MemoryEntry, key: &str) -> Option<String> {
    entry
        .metadata
        .get(key)
        .and_then(|v| v.as_str())
        .map(|v| v.to_string())
}

pub(super) fn card_to_agent(entry: &MemoryEntry) -> Option<String> {
    card_metadata_str(entry, "to_agent")
        .map(|v| normalize_agent_id(&v))
        .or_else(|| {
            entry
                .path
                .strip_prefix(KANBAN_PATH_PREFIX)
                .and_then(|rest| rest.split('/').nth(1))
                .map(normalize_agent_id)
        })
}

pub(super) fn card_from_agent(entry: &MemoryEntry) -> Option<String> {
    card_metadata_str(entry, "from_agent")
        .map(|v| normalize_agent_id(&v))
        .or_else(|| {
            entry
                .path
                .strip_prefix(KANBAN_PATH_PREFIX)
                .and_then(|rest| rest.split('/').next())
                .map(normalize_agent_id)
        })
}

pub(super) fn card_status(entry: &MemoryEntry) -> Option<String> {
    card_metadata_str(entry, "status").map(|v| v.to_ascii_lowercase())
}

pub(super) fn card_priority(entry: &MemoryEntry) -> String {
    card_metadata_str(entry, "priority")
        .map(|v| normalize_card_priority(&v))
        .unwrap_or_else(default_card_priority)
}

pub(super) fn card_type(entry: &MemoryEntry) -> String {
    card_metadata_str(entry, "card_type")
        .map(|v| normalize_card_type(&v))
        .unwrap_or_else(default_card_type)
}
