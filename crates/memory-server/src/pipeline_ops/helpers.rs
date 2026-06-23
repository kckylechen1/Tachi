use chrono::Utc;
use memory_core::MemoryEntry;
use serde_json::json;

use crate::utils::sanitize_safe_path_name;

pub(crate) fn merge_optional_metadata(metadata: Option<serde_json::Value>) -> serde_json::Value {
    match metadata {
        Some(serde_json::Value::Object(map)) => serde_json::Value::Object(map),
        Some(other) => json!({ "payload": other }),
        None => json!({}),
    }
}

pub(crate) fn serialize_json(value: serde_json::Value) -> Result<String, String> {
    serde_json::to_string(&value).map_err(|e| format!("Failed to serialize response: {e}"))
}

fn summary_from_text(text: &str) -> String {
    text.chars().take(100).collect()
}

pub(crate) fn default_ingest_chunk_size() -> usize {
    1200
}

pub(crate) fn default_ingest_chunk_overlap() -> usize {
    120
}

fn topic_from_path(path: &str) -> String {
    path.trim_matches('/')
        .rsplit('/')
        .find(|segment| !segment.is_empty())
        .unwrap_or("ingest")
        .replace('-', "_")
}

pub(crate) fn resolve_domain(domain: Option<String>) -> Option<String> {
    domain.filter(|value| !value.trim().is_empty()).or_else(|| {
        std::env::var("TACHI_DOMAIN")
            .ok()
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty())
    })
}

pub(crate) fn is_lazy_source(source: &str) -> bool {
    matches!(source, "extraction" | "auto" | "ingest_event")
}

pub(crate) fn should_enqueue_enrichment(entry: &MemoryEntry) -> bool {
    // Raw-tier memories skip LLM embedding — deferred to post-distillation pass.
    if entry.tier.eq_ignore_ascii_case("raw") {
        return false;
    }
    true
}

fn days_since(timestamp: &str) -> f64 {
    chrono::DateTime::parse_from_rfc3339(timestamp)
        .map(|dt| (Utc::now() - dt.with_timezone(&Utc)).num_seconds().max(0) as f64 / 86_400.0)
        .unwrap_or(0.0)
}

/// Calculate whether an accessed ephemeral memory should be promoted.
pub(crate) fn calculate_promotion_score(entry: &MemoryEntry, access_days: usize) -> f64 {
    let frequency = (access_days as f64).ln_1p() / 6.0_f64.ln_1p();
    let age_days = days_since(&entry.timestamp);
    let recency = (-0.693 * age_days / 14.0).exp();
    let conceptual = (entry.keywords.len() as f64 / 5.0).min(1.0);

    (frequency * 0.30 + recency * 0.25 + entry.importance * 0.25 + conceptual * 0.20)
        .clamp(0.0, 1.0)
}

pub(crate) fn default_source_path_prefix(
    source_url: Option<&str>,
    source: Option<&str>,
    domain: Option<&str>,
) -> String {
    let domain_part = domain.unwrap_or("general");
    let source_hint = source
        .filter(|value| !value.trim().is_empty())
        .or(source_url)
        .unwrap_or("source");
    format!(
        "/wiki/{}/{}",
        sanitize_safe_path_name(domain_part),
        sanitize_safe_path_name(source_hint)
    )
}

pub(crate) fn default_event_path_prefix(
    domain: Option<&str>,
    event_type: Option<&str>,
    payload: Option<&serde_json::Value>,
    conversation_id: &str,
) -> String {
    if matches!(domain, Some("trading")) {
        if let Some(ticker) = payload
            .and_then(|value| value.get("ticker"))
            .and_then(|value| value.as_str())
            .filter(|value| !value.trim().is_empty())
        {
            return format!("/trading/journal/{}", sanitize_safe_path_name(ticker));
        }
        return "/trading/journal".to_string();
    }

    if !conversation_id.trim().is_empty() {
        return format!(
            "/events/{}",
            sanitize_safe_path_name(conversation_id.trim())
        );
    }

    format!(
        "/events/{}",
        sanitize_safe_path_name(event_type.unwrap_or("event"))
    )
}

pub(crate) fn chunk_text(
    content: &str,
    chunk_size_chars: usize,
    chunk_overlap_chars: usize,
) -> Vec<String> {
    let chars: Vec<char> = content.chars().collect();
    if chars.is_empty() {
        return Vec::new();
    }

    let chunk_size = chunk_size_chars.max(1);
    let overlap = chunk_overlap_chars.min(chunk_size.saturating_sub(1));
    let step = chunk_size.saturating_sub(overlap).max(1);

    let mut chunks = Vec::new();
    let mut start = 0usize;
    while start < chars.len() {
        let end = (start + chunk_size).min(chars.len());
        let chunk: String = chars[start..end].iter().collect();
        let trimmed = chunk.trim();
        if !trimmed.is_empty() {
            chunks.push(trimmed.to_string());
        }
        if end >= chars.len() {
            break;
        }
        start += step;
    }

    if chunks.is_empty() {
        chunks.push(content.trim().to_string());
    }
    chunks
}

pub(crate) fn build_ingest_entry(
    id: String,
    path: String,
    text: String,
    importance: f64,
    source: String,
    scope: String,
    metadata: serde_json::Value,
    retention_policy: Option<String>,
    domain: Option<String>,
    needs_summary: bool,
) -> MemoryEntry {
    let summary = if needs_summary {
        String::new()
    } else {
        summary_from_text(&text)
    };
    let importance = importance.clamp(0.0, 1.0);
    let retention_policy = if is_lazy_source(&source) && importance < 0.5 {
        Some("ephemeral".to_string())
    } else {
        retention_policy
    };

    MemoryEntry {
        id,
        path: path.clone(),
        summary,
        text,
        importance,
        timestamp: Utc::now().to_rfc3339(),
        valid_from: String::new(),
        valid_until: None,
        category: "fact".to_string(),
        topic: topic_from_path(&path),
        keywords: Vec::new(),
        persons: Vec::new(),
        entities: Vec::new(),
        location: String::new(),
        source,
        scope,
        archived: false,
        access_count: 0,
        last_access: None,
        revision: 1,
        metadata,
        vector: None,
        retention_policy,
        domain,
        recall_count: 0,
        query_diversity: 0,
        tier: "raw".to_string(),
    }
}
