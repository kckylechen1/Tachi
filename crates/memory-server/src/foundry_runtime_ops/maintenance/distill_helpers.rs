use super::super::helpers::{build_foundry_agent_root, dedup_strings, round3};
use super::super::FOUNDRY_RELATED_LIMIT;
use crate::utils::sanitize_safe_path_name;
use memory_core::MemoryEntry;
use regex::Regex;
use serde_json::json;
use std::collections::{HashMap, HashSet};
use std::sync::OnceLock;

const GUIDE_TYPE_CONSTRAINT: &str = "constraint";
const GUIDE_TYPE_FIX_PATTERN: &str = "fix_pattern";
const GUIDE_TYPE_DECISION: &str = "decision";
const GUIDE_TYPE_RUNBOOK: &str = "runbook";

pub(in crate::foundry_runtime_ops) fn scheduled_distill_path_prefix(path: &str) -> String {
    let segments = path
        .trim_matches('/')
        .split('/')
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>();
    match segments.as_slice() {
        [] => "/".to_string(),
        ["project", second, ..] => format!("/project/{second}"),
        ["kanban", from, to, ..] => format!("/kanban/{from}/{to}"),
        ["kanban", from] => format!("/kanban/{from}"),
        ["wiki", kind, domain, ..] => format!("/wiki/{kind}/{domain}"),
        ["wiki", kind] => format!("/wiki/{kind}"),
        [first, ..] => format!("/{first}"),
    }
}

fn is_generic_distill_topic(topic: &str) -> bool {
    matches!(
        topic.trim().to_ascii_lowercase().as_str(),
        "" | "unknown"
            | "general"
            | "other"
            | "misc"
            | "architecture"
            | "bug fix"
            | "bug fixes"
            | "bugfix"
            | "testing"
            | "test"
            | "changelog"
            | "roadmap"
            | "todo"
    )
}

pub(in crate::foundry_runtime_ops) fn coherence_bucket_key(
    topic: &str,
    entities: &[String],
) -> Option<String> {
    let topic = topic.trim();
    if !topic.is_empty() && !is_generic_distill_topic(topic) {
        return Some(format!("topic:{topic}"));
    }

    entities
        .iter()
        .map(|entity| entity.trim())
        .find(|entity| !entity.is_empty())
        .map(|entity| format!("entity:{entity}"))
}

fn source_namespace_count(entries: &[MemoryEntry]) -> usize {
    entries
        .iter()
        .map(|entry| scheduled_distill_path_prefix(&entry.path))
        .collect::<HashSet<_>>()
        .len()
}

pub(super) fn distill_quality_flags(entries: &[MemoryEntry]) -> Vec<String> {
    let mut flags = Vec::new();
    if entries.len() < FOUNDRY_DISTILL_MIN_BATCH {
        flags.push("min_batch_not_met".to_string());
    }
    if source_namespace_count(entries) > 1 {
        flags.push("mixed_namespace".to_string());
    }
    flags
}

pub(in crate::foundry_runtime_ops) fn infer_memory_insight(
    entry: &MemoryEntry,
    avg_importance: f64,
    contradiction_count: u32,
    same_topic_count: u32,
    related_count: usize,
) -> serde_json::Value {
    let surprise =
        memory_core::surprise_score(entry, avg_importance, contradiction_count, same_topic_count);
    let mut reasons = Vec::new();

    if contradiction_count > 0 {
        reasons.push("contradiction".to_string());
    }
    if same_topic_count <= 1 {
        reasons.push("novel_topic".to_string());
    }
    if entry.access_count == 0 && entry.importance > 0.7 {
        reasons.push("overlooked_high_importance".to_string());
    }
    if (entry.importance - avg_importance).abs() >= 0.25 {
        reasons.push("importance_outlier".to_string());
    }
    if related_count >= FOUNDRY_RELATED_LIMIT {
        reasons.push("dense_neighborhood".to_string());
    }

    json!({
        "kind": "memory_insight",
        "surprise": round3(surprise),
        "priority": if surprise >= 0.4 { "high" } else if surprise >= 0.2 { "medium" } else { "low" },
        "reasons": reasons,
        "signals": {
            "avg_importance": round3(avg_importance),
            "importance_delta": round3(entry.importance - avg_importance),
            "contradiction_count": contradiction_count,
            "same_topic_count": same_topic_count,
            "related_count": related_count,
        }
    })
}

pub(in crate::foundry_runtime_ops) fn build_foundry_distill_root(agent_id: &str) -> String {
    format!("{}/distilled", build_foundry_agent_root(agent_id))
}

fn guide_text_fragments<'a>(
    distill_text: &'a str,
    source_entries: &'a [MemoryEntry],
) -> impl Iterator<Item = &'a str> {
    std::iter::once(distill_text).chain(source_entries.iter().flat_map(|entry| {
        std::iter::once(entry.summary.as_str())
            .chain(std::iter::once(entry.text.as_str()))
            .chain(std::iter::once(entry.topic.as_str()))
            .chain(entry.keywords.iter().map(String::as_str))
    }))
}

fn contains_ignore_ascii_case(haystack: &str, needle: &str) -> bool {
    let n = needle.as_bytes();
    if n.is_empty() {
        return true;
    }
    let h = haystack.as_bytes();
    if h.len() < n.len() {
        return false;
    }
    h.windows(n.len())
        .any(|window| window.eq_ignore_ascii_case(n))
}

fn fragments_contain_any<'a, I>(fragments: I, needles: &[&str]) -> bool
where
    I: IntoIterator<Item = &'a str>,
{
    for fragment in fragments {
        for needle in needles {
            if contains_ignore_ascii_case(fragment, needle) {
                return true;
            }
        }
    }
    false
}

fn contains_any(haystack: &str, needles: &[&str]) -> bool {
    needles
        .iter()
        .any(|needle| contains_ignore_ascii_case(haystack, needle))
}

fn has_numbered_steps(text: &str) -> bool {
    text.lines()
        .filter(|line| {
            let trimmed = line.trim_start();
            let mut chars = trimmed.chars();
            matches!(chars.next(), Some(ch) if ch.is_ascii_digit())
                && matches!(chars.next(), Some('.' | ')'))
        })
        .take(2)
        .count()
        >= 2
}

pub(in crate::foundry_runtime_ops) fn classify_distill_guide_type(
    distill_text: &str,
    source_entries: &[MemoryEntry],
) -> &'static str {
    let fragments = || guide_text_fragments(distill_text, source_entries);

    if fragments_contain_any(
        fragments(),
        &[
            "fix",
            "fixed",
            "repair",
            "bug",
            "error",
            "failure",
            "failed",
            "panic",
            "exception",
            "regression",
            "linker",
            "修复",
            "报错",
            "错误",
            "失败",
        ],
    ) {
        return GUIDE_TYPE_FIX_PATTERN;
    }
    if fragments_contain_any(
        fragments(),
        &[
            "must",
            "must not",
            "never",
            "required",
            "constraint",
            "invariant",
            "policy",
            "do not",
            "don't",
            "不得",
            "必须",
            "禁止",
            "约束",
        ],
    ) {
        return GUIDE_TYPE_CONSTRAINT;
    }
    if fragments_contain_any(
        fragments(),
        &[
            "decided", "decision", "choose", "chosen", "accepted", "rejected", "tradeoff", "adr",
            "决定", "取舍", "拒绝",
        ],
    ) || source_entries
        .iter()
        .any(|entry| entry.category.eq_ignore_ascii_case("decision"))
    {
        return GUIDE_TYPE_DECISION;
    }
    if fragments_contain_any(
        fragments(),
        &[
            "runbook",
            "checklist",
            "procedure",
            "step",
            "steps",
            "playbook",
            "how to",
            "操作",
            "步骤",
            "流程",
        ],
    ) || has_numbered_steps(distill_text)
    {
        return GUIDE_TYPE_RUNBOOK;
    }
    GUIDE_TYPE_RUNBOOK
}

pub(super) fn build_guide_distill_path(
    agent_id: &str,
    guide_type: &str,
    timestamp_segment: &str,
) -> String {
    format!(
        "/guide/{}/{}/{}",
        guide_type,
        sanitize_safe_path_name(agent_id),
        timestamp_segment
    )
}

fn trim_context_token(raw: &str) -> String {
    raw.trim_matches(|ch: char| {
        ch.is_whitespace()
            || matches!(
                ch,
                '`' | '"' | '\'' | ',' | ';' | ':' | '(' | ')' | '[' | ']' | '{' | '}'
            )
    })
    .trim_end_matches('.')
    .to_string()
}

fn looks_like_file_pattern(value: &str) -> bool {
    let value = value.trim();
    if value.is_empty() {
        return false;
    }
    if value.starts_with("http://") || value.starts_with("https://") {
        return false;
    }
    let has_path_separator = value.contains('/');
    let has_wildcard = value.contains('*');
    if !has_path_separator && !has_wildcard {
        match value.rfind('.') {
            Some(dot) if dot > 0 && dot < value.len() - 1 => {}
            _ => return false,
        }
    }
    has_wildcard
        || value.ends_with(".rs")
        || value.ends_with(".ts")
        || value.ends_with(".tsx")
        || value.ends_with(".js")
        || value.ends_with(".jsx")
        || value.ends_with(".py")
        || value.ends_with(".go")
        || value.ends_with(".java")
        || value.ends_with(".md")
        || value.ends_with(".toml")
        || value.ends_with(".json")
        || value.ends_with(".yaml")
        || value.ends_with(".yml")
}

fn file_pattern_token_regex() -> &'static Regex {
    static REGEX: OnceLock<Regex> = OnceLock::new();
    REGEX.get_or_init(|| Regex::new(r"[A-Za-z0-9_./*\-]+").expect("file pattern regex compiles"))
}

fn wildcard_for_file_path(path: &str) -> Option<String> {
    let slash = path.rfind('/')?;
    let dot = path.rfind('.')?;
    if dot <= slash {
        return None;
    }
    Some(format!("{}/*{}", &path[..slash], &path[dot..]))
}

fn collect_file_patterns_from_text(text: &str, out: &mut Vec<String>) {
    for mat in file_pattern_token_regex().find_iter(text) {
        let candidate = mat.as_str();
        let bytes = candidate.as_bytes();
        if !bytes.iter().any(|b| matches!(b, b'*' | b'.' | b'/')) {
            continue;
        }
        let token = trim_context_token(candidate);
        if looks_like_file_pattern(&token) {
            if let Some(wildcard) = wildcard_for_file_path(&token) {
                out.push(token);
                out.push(wildcard);
            } else {
                out.push(token);
            }
        }
    }
}

fn collect_metadata_file_patterns(value: &serde_json::Value, out: &mut Vec<String>) {
    match value {
        serde_json::Value::String(raw) => {
            let token = trim_context_token(raw);
            if looks_like_file_pattern(&token) {
                out.push(token.clone());
                if let Some(wildcard) = wildcard_for_file_path(&token) {
                    out.push(wildcard);
                }
            }
        }
        serde_json::Value::Array(items) => {
            for item in items {
                collect_metadata_file_patterns(item, out);
            }
        }
        serde_json::Value::Object(map) => {
            for (key, value) in map {
                let key = key.to_ascii_lowercase();
                if key.contains("file") || key.contains("path") {
                    collect_metadata_file_patterns(value, out);
                }
            }
        }
        _ => {}
    }
}

pub(super) fn infer_file_patterns(source_entries: &[MemoryEntry]) -> Vec<String> {
    let mut patterns = Vec::new();
    for entry in source_entries {
        let context_path = entry
            .metadata
            .get("context_path")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        for candidate in [&entry.path, &entry.location, context_path] {
            let token = trim_context_token(candidate);
            if looks_like_file_pattern(&token) {
                patterns.push(token.clone());
                if let Some(wildcard) = wildcard_for_file_path(&token) {
                    patterns.push(wildcard);
                }
            }
        }
        collect_metadata_file_patterns(&entry.metadata, &mut patterns);
        collect_file_patterns_from_text(&entry.text, &mut patterns);
    }
    dedup_strings(patterns).into_iter().take(12).collect()
}

fn looks_like_error_line(line: &str) -> bool {
    let lower = line.to_ascii_lowercase();
    contains_any(
        &lower,
        &[
            "error",
            "failed",
            "failure",
            "panic",
            "exception",
            "could not",
            "cannot",
            "linker",
            "报错",
            "错误",
            "失败",
        ],
    )
}

pub(super) fn infer_error_patterns(
    distill_text: &str,
    source_entries: &[MemoryEntry],
) -> Vec<String> {
    let mut patterns = Vec::new();
    for text in std::iter::once(distill_text).chain(
        source_entries
            .iter()
            .flat_map(|entry| [entry.summary.as_str(), entry.text.as_str()]),
    ) {
        for line in text.lines() {
            if looks_like_error_line(line) {
                patterns.push(line.trim().chars().take(120).collect::<String>());
            }
        }
    }
    dedup_strings(patterns).into_iter().take(8).collect()
}

fn mentions_rejection(text: &str) -> bool {
    contains_any(
        &text.to_ascii_lowercase(),
        &[
            "reject",
            "rejected",
            "avoid",
            "do not",
            "don't",
            "never",
            "instead of",
            "rather than",
            "拒绝",
            "不要",
            "避免",
            "禁止",
        ],
    )
}

fn guide_edge_relations(guide_type: &str, distill_text: &str) -> Vec<&'static str> {
    let mut relations = vec!["distilled_from"];
    match guide_type {
        GUIDE_TYPE_FIX_PATTERN => relations.push("fixed_by"),
        _ => relations.push("causes"),
    }
    if mentions_rejection(distill_text) {
        relations.push("rejected_because");
    }
    relations
}

pub(in crate::foundry_runtime_ops) fn build_distill_edges(
    distill_entry: &MemoryEntry,
    source_entries: &[MemoryEntry],
    guide_type: &str,
    created_at: &str,
) -> Vec<memory_core::MemoryEdge> {
    let mut edges = Vec::new();
    let mut seen = HashSet::new();
    for source in source_entries {
        for relation in guide_edge_relations(guide_type, &distill_entry.text) {
            let (source_id, target_id, weight) = match relation {
                "distilled_from" => (distill_entry.id.clone(), source.id.clone(), 1.0),
                "fixed_by" => (source.id.clone(), distill_entry.id.clone(), 0.9),
                "rejected_because" => (distill_entry.id.clone(), source.id.clone(), 0.75),
                _ => (source.id.clone(), distill_entry.id.clone(), 0.7),
            };
            if seen.insert((source_id.clone(), target_id.clone(), relation.to_string())) {
                edges.push(memory_core::MemoryEdge {
                    source_id,
                    target_id,
                    relation: relation.to_string(),
                    weight,
                    metadata: json!({
                        "source": "foundry_distill",
                        "guide_type": guide_type,
                    }),
                    created_at: created_at.to_string(),
                    valid_from: created_at.to_string(),
                    valid_to: None,
                });
            }
        }
    }
    edges
}

pub(in crate::foundry_runtime_ops) fn job_metadata_value<'a>(
    metadata: &'a serde_json::Value,
    key: &str,
) -> Option<&'a serde_json::Value> {
    metadata
        .get("job")
        .and_then(|job| job.get(key))
        .or_else(|| metadata.get(key))
}

pub(in crate::foundry_runtime_ops) fn job_metadata_usize(
    metadata: &serde_json::Value,
    key: &str,
    default: usize,
) -> usize {
    job_metadata_value(metadata, key)
        .and_then(|value| value.as_u64())
        .map(|value| value as usize)
        .unwrap_or(default)
}

pub(in crate::foundry_runtime_ops) fn job_metadata_string(
    metadata: &serde_json::Value,
    key: &str,
) -> Option<String> {
    job_metadata_value(metadata, key)
        .and_then(|value| value.as_str())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
}

/// Minimum coherent batch size — fewer than this and a topic/entity bucket is
/// considered too thin to justify an LLM round-trip (avoids stitched hallucinations).
const FOUNDRY_DISTILL_MIN_BATCH: usize = 3;

/// Group memories by a coherence key (topic when available, otherwise the most
/// frequent shared entity). Buckets smaller than [`FOUNDRY_DISTILL_MIN_BATCH`]
/// are dropped — they would otherwise force the LLM to stitch unrelated facts
/// into a single false summary (the v0.15.x "缝合怪" regression).
pub(in crate::foundry_runtime_ops) fn coherent_distill_buckets(
    entries: Vec<MemoryEntry>,
) -> Vec<(String, Vec<MemoryEntry>)> {
    let mut buckets: HashMap<String, Vec<MemoryEntry>> = HashMap::new();
    for entry in entries {
        let Some(key) = coherence_bucket_key(&entry.topic, &entry.entities) else {
            // No coherence signal — skip rather than risk a stitched distill.
            continue;
        };
        let namespace = scheduled_distill_path_prefix(&entry.path);
        buckets
            .entry(format!("{namespace}#{key}"))
            .or_default()
            .push(entry);
    }
    // PR #50 review: `coherent_distill_buckets` already drops buckets
    // whose `distill_quality_flags` are non-empty, so by construction
    // the selected bucket has clean flags. We re-derive them here for
    // two reasons:
    //   (1) they are stored in the distill memory's metadata for audit;
    //   (2) defense in depth: if the filter contract drifts in future
    //       refactors, we still surface a structured skip reason instead
    //       of silently writing a low-quality distill.
    buckets
        .into_iter()
        .filter(|(_, group)| distill_quality_flags(group).is_empty())
        .collect()
}

pub(super) fn build_distill_input(entries: &[MemoryEntry]) -> String {
    entries
        .iter()
        .enumerate()
        .map(|(idx, entry)| {
            let summary = if entry.summary.trim().is_empty() {
                entry.text.chars().take(180).collect::<String>()
            } else {
                entry.summary.clone()
            };
            format!(
                "Memory {} | topic={} | importance={:.2}\nSummary: {}\nText: {}",
                idx + 1,
                if entry.topic.is_empty() {
                    "unknown"
                } else {
                    &entry.topic
                },
                entry.importance,
                summary,
                entry.text.chars().take(320).collect::<String>()
            )
        })
        .collect::<Vec<_>>()
        .join("\n\n")
}
