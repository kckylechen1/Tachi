pub(super) use tachi_foundry::{build_section_artifact, SectionArtifactInput};

use crate::utils::sanitize_safe_path_name;
use std::collections::HashSet;

pub(super) fn round3(value: f64) -> f64 {
    (value * 1000.0).round() / 1000.0
}

pub(super) fn normalize_scope(raw: &str, fallback: &str) -> String {
    match raw.trim().to_ascii_lowercase().as_str() {
        "user" => "user".to_string(),
        "project" => "project".to_string(),
        "general" => "general".to_string(),
        _ => fallback.to_string(),
    }
}

pub(super) fn normalize_category(raw: &str) -> String {
    match raw.trim().to_ascii_lowercase().as_str() {
        "fact" => "fact".to_string(),
        "decision" => "decision".to_string(),
        "preference" => "preference".to_string(),
        "entity" => "entity".to_string(),
        "experience" => "experience".to_string(),
        _ => "other".to_string(),
    }
}

pub(super) fn dedup_strings(values: Vec<String>) -> Vec<String> {
    let mut seen = HashSet::new();
    let mut out = Vec::new();
    for value in values {
        let trimmed = value.trim();
        if trimmed.is_empty() {
            continue;
        }
        let key = trimmed.to_ascii_lowercase();
        if seen.insert(key) {
            out.push(trimmed.to_string());
        }
    }
    out
}

pub(super) fn build_entry_path(base_path: &str, topic: &str) -> String {
    let base = base_path.trim_end_matches('/');
    let topic_segment = sanitize_safe_path_name(topic.trim());
    if topic_segment.is_empty() {
        base.to_string()
    } else {
        format!("{base}/{topic_segment}")
    }
}

pub(super) fn normalize_path_prefix_value(raw: &str) -> Option<String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return None;
    }
    if trimmed == "/" {
        return Some("/".to_string());
    }
    Some(trimmed.trim_end_matches('/').to_string())
}

pub(super) fn path_is_within_prefix(path: &str, prefix: &str) -> bool {
    path == prefix || path.starts_with(&format!("{prefix}/"))
}

pub(super) fn build_openclaw_agent_root(agent_id: &str) -> String {
    format!("/openclaw/agent-{}", sanitize_safe_path_name(agent_id))
}

pub(super) fn build_foundry_agent_root(agent_id: &str) -> String {
    format!("/foundry/agents/{}", sanitize_safe_path_name(agent_id))
}

pub(super) fn build_foundry_session_memory_root(agent_id: &str) -> String {
    format!("{}/session-memory", build_foundry_agent_root(agent_id))
}

pub(super) fn build_stable_foundry_memory_id(
    namespace: &str,
    agent_id: &str,
    conversation_id: &str,
    window_id: &str,
    suffix: &str,
) -> String {
    let seed = format!(
        "{}|{}|{}|{}|{}",
        namespace, agent_id, conversation_id, window_id, suffix
    );
    format!(
        "foundry:{}",
        uuid::Uuid::new_v5(&uuid::Uuid::NAMESPACE_OID, seed.as_bytes())
    )
}

pub(super) fn estimate_token_count(text: &str) -> usize {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        0
    } else {
        trimmed.chars().count().div_ceil(4)
    }
}
