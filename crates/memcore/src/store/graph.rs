//! Memory graph methods on [`MemoryStore`].

use crate::{
    db,
    db::AnchorKind,
    error::MemoryError,
    types::{GraphExpandResult, MemoryEdge},
    MemoryStore,
};

impl MemoryStore {
    /// Add or update an edge in the memory graph.
    pub fn add_edge(&self, edge: &MemoryEdge) -> Result<(), MemoryError> {
        db::add_edge(&self.conn, edge)
    }

    /// Remove a specific edge.
    pub fn remove_edge(
        &self,
        source_id: &str,
        target_id: &str,
        relation: &str,
    ) -> Result<bool, MemoryError> {
        db::remove_edge(&self.conn, source_id, target_id, relation)
    }

    /// Get edges connected to a memory entry.
    pub fn get_edges(
        &self,
        memory_id: &str,
        direction: &str,
        relation_filter: Option<&str>,
    ) -> Result<Vec<MemoryEdge>, MemoryError> {
        db::get_edges(&self.conn, memory_id, direction, relation_filter)
    }

    /// BFS expansion from seed IDs through the memory graph.
    pub fn graph_expand(
        &self,
        seed_ids: &[String],
        max_hops: u32,
        relation_filter: Option<&str>,
    ) -> Result<GraphExpandResult, MemoryError> {
        db::graph_expand(&self.conn, seed_ids, max_hops, relation_filter)
    }

    /// Count active contradiction edges connected to a memory entry.
    pub fn get_contradiction_count(&self, memory_id: &str) -> Result<u32, MemoryError> {
        db::get_contradiction_count(&self.conn, memory_id)
    }

    /// Count memories with the same topic.
    pub fn count_same_topic(&self, topic: &str) -> Result<u32, MemoryError> {
        db::count_same_topic(&self.conn, topic)
    }

    /// Average importance across non-archived memories.
    pub fn avg_importance(&self) -> Result<f64, MemoryError> {
        db::avg_importance(&self.conn)
    }

    /// Close `valid_to` on every still-open `related_to` edge (tachi#773
    /// item 3: legacy fog retirement). Idempotent — safe to call repeatedly
    /// from a maintenance sweep. Returns the number of rows closed.
    pub fn close_related_to_fog(&self) -> Result<usize, MemoryError> {
        db::close_related_to_fog(&self.conn)
    }

    /// Ensure a deterministic anchor row exists for `(kind, key)` (tachi#773
    /// item 4). `INSERT OR IGNORE` semantics — idempotent, fails closed on a
    /// kind/key mismatch at an existing id. Returns the anchor's
    /// deterministic id.
    pub fn ensure_anchor(&self, kind: AnchorKind, key: &str) -> Result<String, MemoryError> {
        db::ensure_anchor(&self.conn, kind, key)
    }
}
