use super::guide::GUIDE_TYPE_FIX_PATTERN;
use super::text::contains_any;
use memory_core::MemoryEntry;
use serde_json::json;
use std::collections::HashSet;

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
