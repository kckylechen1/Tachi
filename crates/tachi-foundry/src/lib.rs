use memcore::MemoryEntry;
use regex::Regex;
use serde::Serialize;
use serde_json::json;
use std::cmp::Reverse;
use std::collections::{HashMap, HashSet};
use std::sync::OnceLock;

mod daily_distill;

pub use daily_distill::{
    build_batch_prompt, build_batch_user_payload, build_fallback_user_payload,
    parse_distill_response, resolve_batch_size, resolve_candidate_scan_limit,
    resolve_distill_backend, resolve_processed_scan_limit, scrub_agent_noise, CandidateGroup,
    DistillBackend, DistillBatchReport, GroupPayload, SourceManifestEntry,
    DEFAULT_CANDIDATE_SCAN_LIMIT, DEFAULT_GROUPS_PER_BATCH, DEFAULT_PROCESSED_SCAN_LIMIT,
    DISTILL_DAILY_SYSTEM_PROMPT, DISTILL_DAILY_SYSTEM_PROMPT_SINGLE, MAX_BATCH_PAYLOAD_CHARS,
    MAX_DISTILL_SCAN_LIMIT, MIN_BUCKET_SIZE,
};

const FOUNDRY_DISTILL_MIN_BATCH: usize = 3;
pub const FOUNDRY_DISTILL_SOURCE: &str = "foundry_distill";
const DISTILLED_SOURCE_ARCHIVE_IMPORTANCE_CEILING: f64 = 0.85;
const GUIDE_TYPE_CONSTRAINT: &str = "constraint";
const GUIDE_TYPE_FIX_PATTERN: &str = "fix_pattern";
const GUIDE_TYPE_DECISION: &str = "decision";
const GUIDE_TYPE_RUNBOOK: &str = "runbook";

#[derive(Debug, Clone)]
pub struct DistillBucket {
    pub bucket_key: String,
    pub path_prefix: String,
    pub coherence_key: String,
    pub entries: Vec<MemoryEntry>,
    pub quality_flags: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct GuideDistillPlan {
    pub path: String,
    pub summary: String,
    pub guide_type: String,
    pub keywords: Vec<String>,
    pub entities: Vec<String>,
    pub file_patterns: Vec<String>,
    pub error_patterns: Vec<String>,
    pub source_memory_ids: Vec<String>,
    pub source_path_prefix: String,
    pub namespace_key: String,
    pub coherence_key: String,
    pub bucket_key: String,
    pub quality_flags: Vec<String>,
    pub legacy_distill_root: String,
}

#[derive(Debug, Clone)]
pub struct DailyDistillMemoryPlan {
    pub path: String,
    pub summary: String,
    pub keywords: Vec<String>,
    pub entities: Vec<String>,
    pub source_memory_ids: Vec<String>,
    pub namespace_key: String,
    pub bucket_key: String,
}

#[derive(Debug, Clone, Copy)]
pub struct DailyDistillMemoryInput<'a> {
    pub agent_id: &'a str,
    pub path_prefix: &'a str,
    pub coherence_key: &'a str,
    pub entries: &'a [MemoryEntry],
    pub payload_summary: &'a str,
    pub payload_text: &'a str,
    pub payload_keywords: &'a [String],
    pub timestamp_segment: &'a str,
}

#[derive(Debug, Clone, Copy)]
pub struct FoundryJobMetadata<'a> {
    metadata: &'a serde_json::Value,
}

impl<'a> FoundryJobMetadata<'a> {
    pub fn new(metadata: &'a serde_json::Value) -> Self {
        Self { metadata }
    }

    pub fn value(&self, key: &str) -> Option<&'a serde_json::Value> {
        self.metadata
            .get("job")
            .and_then(|job| job.get(key))
            .or_else(|| self.metadata.get(key))
    }

    pub fn usize(&self, key: &str, default: usize) -> usize {
        self.value(key)
            .and_then(|value| value.as_u64())
            .map(|value| value as usize)
            .unwrap_or(default)
    }

    pub fn string(&self, key: &str) -> Option<String> {
        self.value(key)
            .and_then(|value| value.as_str())
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(ToOwned::to_owned)
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct SectionArtifact {
    pub section_id: String,
    pub layer: String,
    pub kind: String,
    pub title: Option<String>,
    pub cache_boundary: String,
    pub estimated_tokens: usize,
    pub item_count: usize,
    pub source_refs: Vec<String>,
    pub block: String,
}

#[derive(Debug, Clone, Copy)]
pub struct SectionArtifactInput<'a> {
    pub layer: &'a str,
    pub kind: &'a str,
    pub title: Option<&'a str>,
    pub content: &'a str,
    pub items: &'a [String],
    pub cache_boundary: &'a str,
    pub source_refs: &'a [String],
    pub target_tokens: Option<usize>,
}

fn round3(value: f64) -> f64 {
    (value * 1000.0).round() / 1000.0
}

fn dedup_strings(values: Vec<String>) -> Vec<String> {
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

fn sanitize_safe_path_name(name: &str) -> String {
    let sanitized: String = name
        .trim()
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '.') {
                ch
            } else {
                '_'
            }
        })
        .collect();
    let sanitized = sanitized.trim_matches(|ch| matches!(ch, '.' | '_' | '-'));
    if sanitized.is_empty() {
        "unnamed".to_string()
    } else {
        sanitized.to_string()
    }
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

fn truncate_to_token_budget(text: &str, target_tokens: Option<usize>) -> String {
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

pub fn build_section_artifact(input: SectionArtifactInput<'_>) -> SectionArtifact {
    let layer = normalize_section_layer(input.layer);
    let kind = normalize_section_kind(input.kind);
    let cache_boundary = normalize_cache_boundary(input.cache_boundary);
    let clean_items = dedup_strings(input.items.to_vec());
    let title = input
        .title
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned);
    let body = truncate_to_token_budget(input.content, input.target_tokens);
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
                input.source_refs
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
    if !input.source_refs.is_empty() {
        block_lines.push(String::new());
        block_lines.push(format!("Source refs: {}", input.source_refs.join(", ")));
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
        source_refs: dedup_strings(input.source_refs.to_vec()),
        block,
    }
}

fn build_foundry_agent_root(agent_id: &str) -> String {
    format!("/foundry/agents/{}", sanitize_safe_path_name(agent_id))
}

fn estimate_token_count(text: &str) -> usize {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        0
    } else {
        trimmed.chars().count().div_ceil(4)
    }
}

pub fn collect_coherent_distill_buckets(entries: Vec<MemoryEntry>) -> Vec<DistillBucket> {
    coherent_distill_buckets(entries)
        .into_iter()
        .map(|(bucket_key, entries)| distill_bucket_from_parts(bucket_key, entries))
        .collect()
}

pub fn select_memory_distill_bucket(
    entries: Vec<MemoryEntry>,
    preferred_coherence_key: Option<&str>,
    fallback_path_prefix: &str,
) -> Option<DistillBucket> {
    let mut buckets = collect_coherent_distill_buckets(entries);
    if buckets.is_empty() {
        return None;
    }

    if let Some(preferred_key) = preferred_coherence_key {
        let preferred_bucket_key = format!("{fallback_path_prefix}#{preferred_key}");
        if let Some(index) = buckets.iter().position(|bucket| {
            bucket.bucket_key == preferred_bucket_key || bucket.bucket_key == preferred_key
        }) {
            return Some(buckets.swap_remove(index));
        }
    }

    buckets.sort_by_key(|bucket| Reverse(bucket.entries.len()));
    buckets.into_iter().next()
}

pub fn build_memory_distill_input(bucket: &DistillBucket) -> String {
    build_distill_input(&bucket.entries)
}

pub fn plan_guide_distill_memory(
    agent_id: &str,
    bucket: &DistillBucket,
    distill_text: &str,
    timestamp_segment: &str,
) -> GuideDistillPlan {
    let guide_type = classify_distill_guide_type(distill_text, &bucket.entries);
    let mut keywords = vec![
        "foundry".to_string(),
        "distill".to_string(),
        "guide".to_string(),
        guide_type.to_string(),
    ];
    for entry in &bucket.entries {
        keywords.extend(entry.keywords.iter().cloned());
    }

    GuideDistillPlan {
        path: build_guide_distill_path(agent_id, guide_type, timestamp_segment),
        summary: distill_text.chars().take(100).collect(),
        guide_type: guide_type.to_string(),
        keywords: dedup_strings(keywords),
        entities: dedup_strings(
            bucket
                .entries
                .iter()
                .flat_map(|entry| entry.entities.clone())
                .collect(),
        ),
        file_patterns: infer_file_patterns(&bucket.entries),
        error_patterns: infer_error_patterns(distill_text, &bucket.entries),
        source_memory_ids: bucket
            .entries
            .iter()
            .map(|entry| entry.id.clone())
            .collect(),
        source_path_prefix: bucket.path_prefix.clone(),
        namespace_key: bucket.path_prefix.clone(),
        coherence_key: bucket.coherence_key.clone(),
        bucket_key: bucket.bucket_key.clone(),
        quality_flags: bucket.quality_flags.clone(),
        legacy_distill_root: build_foundry_distill_root(agent_id),
    }
}

pub fn plan_daily_distill_memory(input: DailyDistillMemoryInput<'_>) -> DailyDistillMemoryPlan {
    let summary = if input.payload_summary.trim().is_empty() {
        input.payload_text.chars().take(100).collect::<String>()
    } else {
        input.payload_summary.to_string()
    };
    let mut keywords = vec!["foundry".to_string(), "distill".to_string()];
    keywords.extend(input.payload_keywords.iter().cloned());
    for entry in input.entries {
        keywords.extend(entry.keywords.iter().cloned());
    }

    DailyDistillMemoryPlan {
        path: format!(
            "{}/{}",
            build_foundry_distill_root(input.agent_id),
            input.timestamp_segment
        ),
        summary,
        keywords: dedup_strings(keywords),
        entities: dedup_strings(
            input
                .entries
                .iter()
                .flat_map(|entry| entry.entities.clone())
                .collect(),
        ),
        source_memory_ids: input.entries.iter().map(|entry| entry.id.clone()).collect(),
        namespace_key: input.path_prefix.to_string(),
        bucket_key: format!("{}#{}", input.path_prefix, input.coherence_key),
    }
}

pub fn build_daily_distill_candidate_groups(
    candidate_entries: Vec<MemoryEntry>,
) -> Vec<CandidateGroup> {
    collect_coherent_distill_buckets(candidate_entries)
        .into_iter()
        .filter(|bucket| bucket.entries.len() >= MIN_BUCKET_SIZE)
        .map(|bucket| {
            let group_id = format!(
                "{}|{}",
                sanitize_daily_distill_group_id_segment(&bucket.path_prefix),
                sanitize_daily_distill_group_id_segment(&bucket.coherence_key)
            );
            CandidateGroup {
                group_id,
                path_prefix: bucket.path_prefix,
                coherence_key: bucket.coherence_key,
                entries: bucket.entries,
            }
        })
        .collect()
}

pub fn should_skip_daily_distill_candidate(entry: &MemoryEntry, wiki_project: bool) -> bool {
    entry.archived
        || entry.source.eq_ignore_ascii_case(FOUNDRY_DISTILL_SOURCE)
        || memcore::is_recall_cache_entry(entry)
        || is_quarantine_entry(entry)
        || (!wiki_project && memcore::is_wiki_entry(entry))
}

pub fn should_archive_daily_distill_source(entry: &MemoryEntry) -> bool {
    if entry.archived
        || entry.source.eq_ignore_ascii_case(FOUNDRY_DISTILL_SOURCE)
        || !entry.tier.eq_ignore_ascii_case("raw")
        || entry.access_count > 0
        || entry.recall_count > 0
    {
        return false;
    }

    if entry
        .retention_policy
        .as_deref()
        .is_some_and(|policy| matches!(policy, "pinned" | "permanent"))
    {
        return false;
    }

    entry.importance < DISTILLED_SOURCE_ARCHIVE_IMPORTANCE_CEILING
}

pub fn sanitize_daily_distill_group_id_segment(s: &str) -> String {
    let mut out: String = s
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect();
    while out.contains("__") {
        out = out.replace("__", "_");
    }
    out.trim_matches('_').chars().take(40).collect::<String>()
}

fn is_quarantine_entry(entry: &MemoryEntry) -> bool {
    entry.path.starts_with("/_quarantine/") || entry.path.starts_with("/quarantine/")
}

pub fn plan_distill_edges(
    distill_entry: &MemoryEntry,
    source_entries: &[MemoryEntry],
    guide_type: &str,
    created_at: &str,
) -> Vec<memcore::MemoryEdge> {
    build_distill_edges(distill_entry, source_entries, guide_type, created_at)
}

fn distill_bucket_from_parts(bucket_key: String, entries: Vec<MemoryEntry>) -> DistillBucket {
    let (path_prefix, coherence_key) = bucket_key
        .split_once('#')
        .map(|(namespace, coherence)| (namespace.to_string(), coherence.to_string()))
        .unwrap_or_else(|| {
            let path_prefix = entries
                .first()
                .map(|entry| scheduled_distill_path_prefix(&entry.path))
                .unwrap_or_else(|| "/".to_string());
            let coherence_key = entries
                .first()
                .and_then(|entry| coherence_bucket_key(&entry.topic, &entry.entities))
                .unwrap_or_else(|| "unknown".to_string());
            (path_prefix, coherence_key)
        });
    let quality_flags = distill_quality_flags(&entries);

    DistillBucket {
        bucket_key,
        path_prefix,
        coherence_key,
        entries,
        quality_flags,
    }
}

fn scheduled_distill_path_prefix(path: &str) -> String {
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

fn coherence_bucket_key(topic: &str, entities: &[String]) -> Option<String> {
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

fn distill_quality_flags(entries: &[MemoryEntry]) -> Vec<String> {
    let mut flags = Vec::new();
    if entries.len() < FOUNDRY_DISTILL_MIN_BATCH {
        flags.push("min_batch_not_met".to_string());
    }
    if source_namespace_count(entries) > 1 {
        flags.push("mixed_namespace".to_string());
    }
    flags
}

fn coherent_distill_buckets(entries: Vec<MemoryEntry>) -> Vec<(String, Vec<MemoryEntry>)> {
    let mut buckets: HashMap<String, Vec<MemoryEntry>> = HashMap::new();
    for entry in entries {
        let Some(key) = coherence_bucket_key(&entry.topic, &entry.entities) else {
            continue;
        };
        let namespace = scheduled_distill_path_prefix(&entry.path);
        buckets
            .entry(format!("{namespace}#{key}"))
            .or_default()
            .push(entry);
    }
    buckets
        .into_iter()
        .filter(|(_, group)| distill_quality_flags(group).is_empty())
        .collect()
}

fn build_distill_input(entries: &[MemoryEntry]) -> String {
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

fn classify_distill_guide_type(distill_text: &str, source_entries: &[MemoryEntry]) -> &'static str {
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

fn build_distill_edges(
    distill_entry: &MemoryEntry,
    source_entries: &[MemoryEntry],
    guide_type: &str,
    created_at: &str,
) -> Vec<memcore::MemoryEdge> {
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
                edges.push(memcore::MemoryEdge {
                    source_id,
                    target_id,
                    relation: relation.to_string(),
                    weight,
                    metadata: json!({
                        "source": FOUNDRY_DISTILL_SOURCE,
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

pub fn build_foundry_distill_root(agent_id: &str) -> String {
    format!("{}/distilled", build_foundry_agent_root(agent_id))
}

fn build_guide_distill_path(agent_id: &str, guide_type: &str, timestamp_segment: &str) -> String {
    format!(
        "/guide/{}/{}/{}",
        guide_type,
        sanitize_safe_path_name(agent_id),
        timestamp_segment
    )
}

pub fn infer_memory_insight(
    entry: &MemoryEntry,
    avg_importance: f64,
    contradiction_count: u32,
    same_topic_count: u32,
    related_count: usize,
    dense_related_limit: usize,
) -> serde_json::Value {
    let surprise =
        memcore::surprise_score(entry, avg_importance, contradiction_count, same_topic_count);
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
    if related_count >= dense_related_limit {
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

fn infer_file_patterns(source_entries: &[MemoryEntry]) -> Vec<String> {
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

fn infer_error_patterns(distill_text: &str, source_entries: &[MemoryEntry]) -> Vec<String> {
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

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(id: &str, path: &str, topic: &str, text: &str, entities: Vec<String>) -> MemoryEntry {
        MemoryEntry {
            id: id.to_string(),
            path: path.to_string(),
            summary: text.chars().take(40).collect(),
            text: text.to_string(),
            importance: 0.7,
            timestamp: "2026-01-01T00:00:00Z".to_string(),
            valid_from: String::new(),
            valid_until: None,
            category: "fact".to_string(),
            topic: topic.to_string(),
            keywords: vec!["tachi".to_string()],
            persons: vec![],
            entities,
            location: String::new(),
            source: "manual".to_string(),
            scope: "project".to_string(),
            archived: false,
            access_count: 0,
            last_access: None,
            revision: 1,
            metadata: json!({
                "file_path": "crates/tachi-server/src/tools.rs"
            }),
            vector: None,
            retention_policy: None,
            domain: None,
            recall_count: 0,
            query_diversity: 0,
            tier: "raw".to_string(),
        }
    }

    #[test]
    fn section_artifact_is_stable_for_same_inputs() {
        let first = build_section_artifact(SectionArtifactInput {
            layer: "session",
            kind: "memory_recall",
            title: None,
            content: "alpha beta",
            items: &["one".to_string(), "one".to_string()],
            cache_boundary: "session",
            source_refs: &["m1".to_string()],
            target_tokens: None,
        });
        let second = build_section_artifact(SectionArtifactInput {
            layer: "session",
            kind: "memory_recall",
            title: None,
            content: "alpha beta",
            items: &["one".to_string()],
            cache_boundary: "session",
            source_refs: &["m1".to_string()],
            target_tokens: None,
        });

        assert_eq!(first.section_id, second.section_id);
        assert_eq!(first.item_count, 1);
        assert!(first.block.contains("Relevant Memories"));
    }

    #[test]
    fn coherent_distill_buckets_keep_unrelated_topics_apart() {
        let entries = [
            ("m-1", "/hapi/launch/m-1", "launch-signal"),
            ("m-2", "/hapi/launch/m-2", "launch-signal"),
            ("m-3", "/hapi/launch/m-3", "launch-signal"),
            ("m-4", "/hapi/risk/m-4", "risk-signal"),
            ("m-5", "/hapi/risk/m-5", "risk-signal"),
            ("m-6", "/hapi/risk/m-6", "risk-signal"),
        ]
        .into_iter()
        .map(|(id, path, topic)| entry(id, path, topic, topic, vec![format!("entity-{topic}")]))
        .collect();

        let mut buckets = coherent_distill_buckets(entries);
        buckets.sort_by(|a, b| a.0.cmp(&b.0));

        assert_eq!(buckets.len(), 2);
        assert_eq!(buckets[0].0, "/hapi#topic:launch-signal");
        assert_eq!(buckets[1].0, "/hapi#topic:risk-signal");
    }

    #[test]
    fn daily_distill_group_id_segment_keeps_safe_chars() {
        assert_eq!(
            sanitize_daily_distill_group_id_segment("topic:foo bar"),
            "topic_foo_bar"
        );
        assert_eq!(
            sanitize_daily_distill_group_id_segment("/project/x"),
            "project_x"
        );
    }

    #[test]
    fn daily_distill_candidate_groups_sanitize_ids_and_enforce_min_size() {
        let entries = [
            ("m-1", "/project/bounded/1", "bounded scan"),
            ("m-2", "/project/bounded/2", "bounded scan"),
            ("m-3", "/project/bounded/3", "bounded scan"),
            ("m-4", "/project/small/1", "small"),
            ("m-5", "/project/small/2", "small"),
        ]
        .into_iter()
        .map(|(id, path, topic)| entry(id, path, topic, topic, vec![format!("entity-{topic}")]))
        .collect();

        let groups = build_daily_distill_candidate_groups(entries);

        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].group_id, "project_bounded|topic_bounded_scan");
        assert_eq!(groups[0].entries.len(), MIN_BUCKET_SIZE);
    }

    #[test]
    fn daily_distill_candidate_filter_hides_namespace_noise() {
        let mut archived = entry(
            "archived",
            "/project/bounded/archived",
            "bounded",
            "archived",
            vec![],
        );
        archived.archived = true;
        let mut recall_cache = entry(
            "cache",
            "/scratch/recall-cache/noise",
            "recall_rerank_cache",
            "cache",
            vec![],
        );
        recall_cache.source = memcore::FOUNDRY_RECALL_CACHE_SOURCE.to_string();
        recall_cache.metadata = json!({"recall_rerank_cache": true});
        let mut wiki = entry("wiki", "/wiki/page", "wiki", "wiki", vec![]);
        wiki.domain = Some("wiki".to_string());
        let quarantine = entry(
            "quarantine",
            "/_quarantine/cross-db/noise",
            "noise",
            "noise",
            vec![],
        );
        let visible = entry(
            "visible",
            "/project/bounded/visible",
            "bounded",
            "visible",
            vec![],
        );

        assert!(should_skip_daily_distill_candidate(&archived, false));
        assert!(should_skip_daily_distill_candidate(&recall_cache, false));
        assert!(should_skip_daily_distill_candidate(&wiki, false));
        assert!(!should_skip_daily_distill_candidate(&wiki, true));
        assert!(should_skip_daily_distill_candidate(&quarantine, false));
        assert!(!should_skip_daily_distill_candidate(&visible, false));
    }

    #[test]
    fn daily_distill_archive_policy_preserves_used_or_protected_raw_sources() {
        let eligible = entry(
            "eligible",
            "/project/bounded/eligible",
            "bounded",
            "eligible",
            vec![],
        );
        let mut used = eligible.clone();
        used.access_count = 1;
        let mut recalled = eligible.clone();
        recalled.recall_count = 1;
        let mut pinned = eligible.clone();
        pinned.retention_policy = Some("pinned".to_string());
        let mut permanent = eligible.clone();
        permanent.retention_policy = Some("permanent".to_string());
        let mut important = eligible.clone();
        important.importance = 0.95;
        let mut consolidated = eligible.clone();
        consolidated.tier = "consolidated".to_string();

        assert!(should_archive_daily_distill_source(&eligible));
        for source in [
            &used,
            &recalled,
            &pinned,
            &permanent,
            &important,
            &consolidated,
        ] {
            assert!(!should_archive_daily_distill_source(source));
        }
    }

    #[test]
    fn job_metadata_prefers_nested_job_values_and_falls_back_to_top_level() {
        let metadata = json!({
            "top_k": 4,
            "path_prefix": "/top-level",
            "job": {
                "candidate_multiplier": 5,
                "path_prefix": "/nested",
                "project": "docs",
                "queries": ["release drift"]
            }
        });
        let job = FoundryJobMetadata::new(&metadata);

        assert_eq!(job.usize("top_k", 99), 4);
        assert_eq!(job.usize("candidate_multiplier", 99), 5);
        assert_eq!(job.string("project").as_deref(), Some("docs"));
        assert_eq!(job.string("path_prefix").as_deref(), Some("/nested"));
        assert_eq!(job.value("queries").unwrap().as_array().unwrap().len(), 1);
        assert_eq!(job.usize("missing", 42), 42);
    }

    #[test]
    fn guide_planning_emits_edges_and_patterns() {
        let source = entry(
            "source-1",
            "/project/tachi/crates/tachi-server/src/tools.rs",
            "guide-layer",
            "error: linker failed in crates/tachi-server/src/tools.rs",
            vec!["Tachi".to_string()],
        );
        let guide = MemoryEntry {
            id: "guide-1".to_string(),
            path: "/guide/fix_pattern/codex/20260101T000000".to_string(),
            summary: "Fix linker errors".to_string(),
            text: "Fix linker error by rebuilding sqlite vec; avoid deleting migrations."
                .to_string(),
            category: "guide".to_string(),
            topic: "fix_pattern".to_string(),
            source: FOUNDRY_DISTILL_SOURCE.to_string(),
            ..source.clone()
        };

        assert_eq!(
            classify_distill_guide_type(
                "Fix linker error by rebuilding sqlite vec.",
                std::slice::from_ref(&source)
            ),
            "fix_pattern"
        );
        assert!(infer_file_patterns(std::slice::from_ref(&source))
            .contains(&"crates/tachi-server/src/tools.rs".to_string()));
        assert!(
            infer_error_patterns(&guide.text, std::slice::from_ref(&source))
                .iter()
                .any(|line| line.contains("linker error"))
        );
        let relations = build_distill_edges(&guide, &[source], "fix_pattern", &guide.timestamp)
            .into_iter()
            .map(|edge| edge.relation)
            .collect::<HashSet<_>>();
        assert!(relations.contains("distilled_from"));
        assert!(relations.contains("fixed_by"));
        assert!(relations.contains("rejected_because"));
    }
}
