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
