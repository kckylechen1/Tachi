use std::collections::{HashMap, HashSet};

use crate::types::MemoryEdge;

/// Local PageRank on a subgraph of MemoryEdges.
/// Returns a map from node_id → PageRank score (normalized to [0, 1]).
/// Uses 5 iterations with damping factor d=0.85.
/// Nodes with more incoming edges from important nodes rank higher.
pub fn local_pagerank(edges: &[MemoryEdge], damping: f64) -> HashMap<String, f64> {
    // Collect all node IDs
    let mut nodes: HashSet<String> = HashSet::new();
    for edge in edges {
        nodes.insert(edge.source_id.clone());
        nodes.insert(edge.target_id.clone());
    }

    if nodes.is_empty() {
        return HashMap::new();
    }

    let n = nodes.len() as f64;
    let base = (1.0 - damping) / n;

    // Build adjacency: source → list of targets
    // Exclude 'contradicts' edges — contradictions indicate conflict, not authority
    let mut outgoing: HashMap<&str, Vec<&str>> = HashMap::new();
    for edge in edges {
        if edge.relation != "contradicts" {
            outgoing
                .entry(edge.source_id.as_str())
                .or_default()
                .push(edge.target_id.as_str());
        }
    }

    // Initialize scores uniformly
    let mut scores: HashMap<String, f64> = nodes.iter().map(|id| (id.clone(), 1.0 / n)).collect();

    // 5 iterations of PageRank
    for _ in 0..5 {
        let mut new_scores: HashMap<String, f64> =
            nodes.iter().map(|id| (id.clone(), base)).collect();

        for (source, targets) in &outgoing {
            if targets.is_empty() {
                continue;
            }
            let source_score = scores.get(*source).copied().unwrap_or(0.0);
            let share = source_score / targets.len() as f64;
            for target in targets {
                if let Some(s) = new_scores.get_mut(*target) {
                    *s += damping * share;
                }
            }
        }

        scores = new_scores;
    }

    // Normalize to [0, 1]
    let max_score = scores.values().cloned().fold(0.0_f64, f64::max);
    if max_score > 0.0 {
        for score in scores.values_mut() {
            *score /= max_score;
        }
    }

    scores
}

pub fn graph_relation_activation_weight(relation: &str) -> f64 {
    match relation {
        "supports" => 0.90,
        "elaborates" => 0.85,
        "causes" | "fixed_by" => 0.80,
        "reinforces" => 0.75,
        "follows" | "references" | "distilled_from" | "derived_from" => 0.70,
        "similar_to" | "related_to" | "merge_hint" => 0.55,
        "supersedes" => 0.40,
        "contradicts" | "rejected_because" => 0.30,
        _ => 0.50,
    }
}

pub fn graph_spreading_activation_with_seed_weights(
    seed_weights: &HashMap<String, f64>,
    edges: &[MemoryEdge],
    max_hops: u32,
    decay: f64,
) -> HashMap<String, f64> {
    if seed_weights.is_empty() || max_hops == 0 {
        return HashMap::new();
    }

    let seeds: HashSet<&String> = seed_weights.keys().collect();
    let mut activation: HashMap<String, f64> = seed_weights
        .iter()
        .filter_map(|(id, weight)| {
            let weight = if weight.is_finite() { *weight } else { 0.0 }.clamp(0.0, 1.0);
            (weight > 0.0).then(|| (id.clone(), weight))
        })
        .collect();
    if activation.is_empty() {
        return HashMap::new();
    }
    let mut frontier = activation.clone();

    for _ in 0..max_hops {
        if frontier.is_empty() {
            break;
        }
        let mut propagated_by_target = HashMap::<String, f64>::new();
        for edge in edges {
            for (source, target) in [
                (&edge.source_id, &edge.target_id),
                (&edge.target_id, &edge.source_id),
            ] {
                let Some(parent_activation) = frontier.get(source).copied() else {
                    continue;
                };
                if seeds.contains(target) {
                    continue;
                }
                let propagated = parent_activation
                    * edge.weight.clamp(0.0, 1.0)
                    * decay
                    * graph_relation_activation_weight(&edge.relation);
                if propagated <= 0.0 {
                    continue;
                }
                propagated_by_target
                    .entry(target.clone())
                    .and_modify(|acc| *acc = 1.0 - (1.0 - *acc) * (1.0 - propagated))
                    .or_insert(propagated);
            }
        }

        frontier = HashMap::new();
        for (id, propagated) in propagated_by_target {
            let propagated = propagated.clamp(0.0, 1.0);
            if propagated <= 0.0 {
                continue;
            }
            let current = activation.get(&id).copied().unwrap_or(0.0);
            // Converging graph paths should reinforce each other without letting
            // dense local clusters exceed a normalized activation ceiling.
            let combined = 1.0 - (1.0 - current) * (1.0 - propagated);
            if combined > current {
                activation.insert(id.clone(), combined);
                frontier.insert(id, propagated);
            }
        }
    }

    activation.retain(|id, _| !seeds.contains(id));
    activation
}
