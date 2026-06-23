use super::SectionArtifact;
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

fn normalize_section_layer(raw: &str) -> String {
    match raw.trim().to_ascii_lowercase().as_str() {
        "static" => "static".to_string(),
        "session" => "session".to_string(),
        "live" => "live".to_string(),
        _ => "other".to_string(),
    }
}

fn normalize_section_kind(raw: &str) -> String {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return "context".to_string();
    }
    sanitize_safe_path_name(trimmed)
}

fn normalize_cache_boundary(raw: &str) -> String {
    match raw.trim().to_ascii_lowercase().as_str() {
        "none" => "none".to_string(),
        "turn" => "turn".to_string(),
        "session" => "session".to_string(),
        "conversation" => "conversation".to_string(),
        "agent" => "agent".to_string(),
        "static" => "static".to_string(),
        _ => "session".to_string(),
    }
}

fn default_section_title(kind: &str) -> &'static str {
    match kind {
        "memory_recall" => "Relevant Memories",
        "compact_rollup" => "Session Rollup",
        "session_memory" => "Durable Session Memory",
        "capability_bundle" => "Capability Bundle",
        _ => "Context Section",
    }
}

pub(super) fn truncate_to_token_budget(text: &str, target_tokens: Option<usize>) -> String {
    let Some(target_tokens) = target_tokens.filter(|budget| *budget > 0) else {
        return text.trim().to_string();
    };
    let max_chars = target_tokens.saturating_mul(4);
    let trimmed = text.trim();
    if trimmed.chars().count() <= max_chars {
        return trimmed.to_string();
    }
    let mut out = trimmed
        .chars()
        .take(max_chars.saturating_sub(1))
        .collect::<String>();
    out.push('…');
    out
}

fn section_seed(
    layer: &str,
    kind: &str,
    title: Option<&str>,
    content: &str,
    items: &[String],
    cache_boundary: &str,
    source_refs: &[String],
) -> String {
    format!(
        "{}|{}|{}|{}|{}|{}|{}",
        layer,
        kind,
        title.unwrap_or(""),
        content,
        items.join("|"),
        cache_boundary,
        source_refs.join("|")
    )
}

pub(super) fn build_section_artifact(
    layer: &str,
    kind: &str,
    title: Option<&str>,
    content: &str,
    items: &[String],
    cache_boundary: &str,
    source_refs: &[String],
    target_tokens: Option<usize>,
) -> SectionArtifact {
    let layer = normalize_section_layer(layer);
    let kind = normalize_section_kind(kind);
    let cache_boundary = normalize_cache_boundary(cache_boundary);
    let clean_items = dedup_strings(items.to_vec());
    let title = title
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned);
    let body = truncate_to_token_budget(content, target_tokens);
    let section_id = format!(
        "section:{}",
        uuid::Uuid::new_v5(
            &uuid::Uuid::NAMESPACE_OID,
            section_seed(
                &layer,
                &kind,
                title.as_deref(),
                &body,
                &clean_items,
                &cache_boundary,
                source_refs
            )
            .as_bytes()
        )
    );

    let mut block_lines = vec![format!(
        "<!-- tachi:section id={} layer={} kind={} cache_boundary={} -->",
        section_id, layer, kind, cache_boundary
    )];
    block_lines.push(format!(
        "## {}",
        title
            .clone()
            .unwrap_or_else(|| default_section_title(&kind).to_string())
    ));
    if !body.is_empty() {
        block_lines.push(String::new());
        block_lines.push(body);
    }
    if !clean_items.is_empty() {
        block_lines.push(String::new());
        for item in &clean_items {
            block_lines.push(format!("- {item}"));
        }
    }
    if !source_refs.is_empty() {
        block_lines.push(String::new());
        block_lines.push(format!("Source refs: {}", source_refs.join(", ")));
    }
    block_lines.push("<!-- /tachi:section -->".to_string());
    let block = block_lines.join("\n");

    SectionArtifact {
        section_id,
        layer,
        kind,
        title,
        cache_boundary,
        estimated_tokens: estimate_token_count(&block),
        item_count: clean_items.len(),
        source_refs: dedup_strings(source_refs.to_vec()),
        block,
    }
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
