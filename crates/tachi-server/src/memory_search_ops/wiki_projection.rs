//! Wiki projection helpers: duplicate detection, supersede edges, and parent
//! paths for canonical Wiki/Guide projection writes.
//!
//! Pure functions used by the memory save path
//! (`save_memory::persist`); `copilot_ops` no longer owns them, so the memory
//! side does not depend upward on the copilot facade.

use memcore::MemoryEntry;
use serde_json::json;
use std::collections::HashSet;

const WIKI_DUP_JACCARD_THRESHOLD: f64 = 0.85;

fn wiki_text_tokens(input: &str) -> HashSet<String> {
    input
        .split(|ch: char| !ch.is_alphanumeric() && ch != '_' && ch != '-')
        .map(|token| token.trim().to_ascii_lowercase())
        .filter(|token| token.chars().count() >= 3)
        .collect()
}

fn wiki_subject_token(input: &str) -> Option<String> {
    let tokens = wiki_text_tokens(input);
    if tokens.len() == 1 {
        tokens.into_iter().next()
    } else {
        None
    }
}

fn wiki_text_jaccard_sets(a: &HashSet<String>, b: &HashSet<String>) -> f64 {
    if a.is_empty() || b.is_empty() {
        return 0.0;
    }
    let intersection = a.intersection(b).count() as f64;
    let union = a.union(b).count() as f64;
    if union == 0.0 {
        0.0
    } else {
        intersection / union
    }
}

/// Decide whether an active Wiki/Guide row describes the same semantic subject
/// as a canonical projection candidate.
pub(crate) fn is_wiki_projection_duplicate(
    candidate: &MemoryEntry,
    path: &str,
    topic: &str,
    text: &str,
) -> bool {
    let target_subject = wiki_subject_token(topic);
    let target_text_tokens = wiki_text_tokens(text);
    // Dedup criteria (OR-combined, but single-token topic match requires path
    // prefix overlap to avoid over-broad matching).
    let same_path = candidate.path == path;
    let same_parent = candidate.path.rsplit_once('/').map(|(parent, _)| parent)
        == path.rsplit_once('/').map(|(parent, _)| parent);
    let same_exact_topic = !topic.trim().is_empty()
        && candidate.topic.trim().eq_ignore_ascii_case(topic.trim())
        && same_parent;
    let same_subject_token = target_subject.as_ref().is_some_and(|token| {
        let candidate_token = wiki_subject_token(&candidate.topic);
        candidate_token.as_ref() == Some(token) && same_parent
    });
    let similar_text =
        wiki_text_jaccard_sets(&target_text_tokens, &wiki_text_tokens(&candidate.text))
            >= WIKI_DUP_JACCARD_THRESHOLD;
    same_path || same_exact_topic || same_subject_token || similar_text
}

pub(crate) fn wiki_projection_supersedes_edge(
    canonical_id: &str,
    candidate_id: &str,
    path: &str,
    topic: &str,
    created_at: &str,
) -> memcore::MemoryEdge {
    memcore::MemoryEdge {
        source_id: canonical_id.to_string(),
        target_id: candidate_id.to_string(),
        relation: "supersedes".to_string(),
        weight: 0.9,
        metadata: json!({
            "source": "wiki_write_dedup",
            "path": path,
            "topic": topic,
        }),
        created_at: created_at.to_string(),
        valid_from: String::new(),
        valid_to: None,
    }
}

pub(crate) fn wiki_parent_path(path: &str) -> String {
    path.trim_end_matches('/')
        .rsplit_once('/')
        .map(|(parent, _)| if parent.is_empty() { "/" } else { parent })
        .unwrap_or("/wiki")
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn wiki_candidate(id: &str, path: &str, topic: &str, text: &str) -> MemoryEntry {
        MemoryEntry {
            id: id.to_string(),
            path: path.to_string(),
            summary: text.to_string(),
            text: text.to_string(),
            importance: 0.7,
            timestamp: "2026-07-31T00:00:00Z".to_string(),
            valid_from: String::new(),
            valid_until: None,
            category: "experience".to_string(),
            topic: topic.to_string(),
            keywords: Vec::new(),
            persons: Vec::new(),
            entities: Vec::new(),
            location: String::new(),
            source: "wiki".to_string(),
            scope: "general".to_string(),
            archived: false,
            access_count: 0,
            scored_count: 0,
            last_access: None,
            last_use_at: None,
            revision: 1,
            metadata: json!({"wiki": true}),
            vector: None,
            retention_policy: Some("permanent".to_string()),
            domain: Some("wiki".to_string()),
            recall_count: 0,
            query_diversity: 0,
            tier: "raw".to_string(),
        }
    }

    #[test]
    fn single_token_topic_requires_the_same_parent_to_be_a_duplicate() {
        let candidate = wiki_candidate(
            "ops-mcp",
            "/wiki/ops/mcp",
            "mcp",
            "Operational transport retry notes with no engineering overlap.",
        );

        assert!(!is_wiki_projection_duplicate(
            &candidate,
            "/wiki/engineering/mcp",
            "mcp",
            "Engineering protocol schema conventions for tool interoperability.",
        ));
        assert!(is_wiki_projection_duplicate(
            &candidate,
            "/wiki/ops/mcp-replacement",
            "mcp",
            "A replacement operational transport note.",
        ));
    }
}
