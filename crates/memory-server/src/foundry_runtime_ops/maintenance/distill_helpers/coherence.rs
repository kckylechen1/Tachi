use memory_core::MemoryEntry;
use std::collections::{HashMap, HashSet};

/// Minimum coherent batch size — fewer than this and a topic/entity bucket is
/// considered too thin to justify an LLM round-trip (avoids stitched hallucinations).
const FOUNDRY_DISTILL_MIN_BATCH: usize = 3;

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

pub(in crate::foundry_runtime_ops::maintenance) fn distill_quality_flags(
    entries: &[MemoryEntry],
) -> Vec<String> {
    let mut flags = Vec::new();
    if entries.len() < FOUNDRY_DISTILL_MIN_BATCH {
        flags.push("min_batch_not_met".to_string());
    }
    if source_namespace_count(entries) > 1 {
        flags.push("mixed_namespace".to_string());
    }
    flags
}

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
