use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use super::entry::{default_metadata, MemoryEntry};

// ─── Scoring Types ───────────────────────────────────────────────────────────

/// Per-channel scores for a single search result.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HybridScore {
    /// Cosine similarity from vector KNN (0.0–1.0)
    pub vector: f64,
    /// BM25 FTS score (normalized 0.0–1.0)
    pub fts: f64,
    /// Bag-of-words symbolic overlap (0.0–1.0)
    pub symbolic: f64,
    /// ACT-R memory decay factor
    pub decay: f64,
    /// Weighted final score
    #[serde(rename = "final")]
    pub final_score: f64,
}

/// A ranked search result: entry + scores.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SearchResult {
    pub entry: MemoryEntry,
    pub score: HybridScore,
    /// `true` when this result was appended by post-ranking graph expansion
    /// (`search/graph_expansion.rs::append_graph_expansion`) rather than
    /// surfaced by the primary vector/FTS/symbolic channels — tachi#1647.
    ///
    /// Before this field existed, the only signal a consumer had for "was
    /// this a graph hit" was that graph-injected results carry an
    /// all-zero [`HybridScore`] on every component except `final_score`
    /// (`vector: 0.0, fts: 0.0, symbolic: 0.0, decay: 0.0` — see the
    /// construction site in `graph_expansion.rs`): an undocumented,
    /// pattern-matched heuristic. This field replaces that heuristic with an
    /// explicit, documented marker; the all-zero shape is still produced (it
    /// is honest — none of those channels ran) but is no longer the API.
    ///
    /// `skip_serializing_if` keeps every non-injected result byte-identical
    /// to before this field existed.
    #[serde(default, skip_serializing_if = "is_false")]
    pub graph_injected: bool,
}

fn is_false(b: &bool) -> bool {
    !*b
}

/// Aggregate statistics about the memory store.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StatsResult {
    pub total: u64,
    pub by_scope: HashMap<String, u64>,
    pub by_category: HashMap<String, u64>,
    pub by_root_path: HashMap<String, u64>,
}

// ─── Graph Types ─────────────────────────────────────────────────────────────

/// A directed edge between two memory entries in the memory graph.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemoryEdge {
    /// Source memory ID
    pub source_id: String,
    /// Target memory ID
    pub target_id: String,
    /// Relationship type: "causes", "supports", "contradicts", "follows", "related_to"
    pub relation: String,
    /// Edge weight (0.0–1.0)
    #[serde(default = "default_edge_weight")]
    pub weight: f64,
    /// Optional JSON metadata (confidence, timespan, etc.)
    #[serde(default = "default_metadata")]
    pub metadata: serde_json::Value,
    /// When the edge was created
    #[serde(default)]
    pub created_at: String,
    /// When the edge becomes valid (temporal validity start)
    #[serde(default)]
    pub valid_from: String,
    /// When the edge expires (temporal validity end); None = no expiry
    #[serde(default)]
    pub valid_to: Option<String>,
}

fn default_edge_weight() -> f64 {
    1.0
}

/// Result of a graph expansion query
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GraphExpandResult {
    /// Memory entries found via graph traversal
    pub entries: Vec<MemoryEntry>,
    /// Edges traversed during expansion
    pub edges: Vec<MemoryEdge>,
    /// Hop distance for each entry ID
    pub distances: HashMap<String, u32>,
}
