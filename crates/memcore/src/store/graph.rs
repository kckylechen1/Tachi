//! Memory graph methods on [`MemoryStore`].

use rusqlite::{Transaction, TransactionBehavior};

use crate::{
    db,
    db::AnchorKind,
    error::MemoryError,
    relation_ontology::ComponentGovernanceRelation,
    types::{GraphExpandResult, MemoryEdge},
    MemoryStore,
};

impl MemoryStore {
    fn refuse_generic_supersession_edge(edge: &MemoryEdge) -> Result<(), MemoryError> {
        if edge.relation == "supersedes" {
            return Err(MemoryError::InvalidArg(
                "supersedes edges are reserved for canonical immutable-supersession claims"
                    .to_string(),
            ));
        }
        Ok(())
    }

    fn retired_sticky_edge_preflight(
        &self,
        source_id: &str,
        target_id: &str,
        operation: &str,
    ) -> Result<(), MemoryError> {
        db::refuse_retired_sticky_row_within_tx(&self.conn, source_id, operation)?;
        db::refuse_retired_sticky_row_within_tx(&self.conn, target_id, operation)
    }

    fn with_retired_sticky_edge_preflight<T>(
        &self,
        source_id: &str,
        target_id: &str,
        operation: &str,
        write: impl FnOnce(&rusqlite::Connection) -> Result<T, MemoryError>,
    ) -> Result<T, MemoryError> {
        let tx = Transaction::new_unchecked(&self.conn, TransactionBehavior::Immediate)?;
        db::refuse_retired_sticky_row_within_tx(&tx, source_id, operation)?;
        db::refuse_retired_sticky_row_within_tx(&tx, target_id, operation)?;
        let result = write(&tx)?;
        tx.commit()?;
        Ok(result)
    }

    /// Add or update an edge in the memory graph. Generic path: the relation
    /// must be admissible on ontology-v1 (the #772 grandfathered relations are
    /// rejected here — use [`MemoryStore::add_component_governance_edge`]).
    pub fn add_edge(&self, edge: &MemoryEdge) -> Result<(), MemoryError> {
        Self::refuse_generic_supersession_edge(edge)?;
        self.with_retired_sticky_edge_preflight(
            &edge.source_id,
            &edge.target_id,
            "given a graph edge",
            |conn| db::add_edge(conn, edge),
        )
    }

    /// [`MemoryStore::add_edge`] plus explicit provenance for the appended
    /// `edge_observations` row (#774).
    pub fn add_edge_with_provenance(
        &self,
        edge: &MemoryEdge,
        provenance: &db::EdgeProvenance,
    ) -> Result<(), MemoryError> {
        Self::refuse_generic_supersession_edge(edge)?;
        self.with_retired_sticky_edge_preflight(
            &edge.source_id,
            &edge.target_id,
            "given a provenance graph edge",
            |conn| db::add_edge_with_provenance(conn, edge, provenance),
        )
    }

    /// Add an edge under a transaction/savepoint already owned by the caller.
    pub fn add_edge_with_provenance_within_tx(
        &self,
        edge: &MemoryEdge,
        provenance: &db::EdgeProvenance,
    ) -> Result<(), MemoryError> {
        Self::refuse_generic_supersession_edge(edge)?;
        self.retired_sticky_edge_preflight(
            &edge.source_id,
            &edge.target_id,
            "given a provenance graph edge",
        )?;
        db::add_edge_with_provenance(&self.conn, edge, provenance)
    }

    /// Typed, caller-scoped write door for the #772 component-governance
    /// grandfathered relations. Only `component_governance_ops` seeding should
    /// call this; the closed [`ComponentGovernanceRelation`] enum is the
    /// authoritative relation (and the caller-scoping mechanism).
    pub fn add_component_governance_edge(
        &self,
        edge: &MemoryEdge,
        relation: ComponentGovernanceRelation,
    ) -> Result<(), MemoryError> {
        self.with_retired_sticky_edge_preflight(
            &edge.source_id,
            &edge.target_id,
            "given a component-governance edge",
            |conn| db::add_component_governance_edge(conn, edge, relation),
        )
    }

    /// [`MemoryStore::add_component_governance_edge`] plus explicit provenance
    /// for the appended `edge_observations` row (#774).
    pub fn add_component_governance_edge_with_provenance(
        &self,
        edge: &MemoryEdge,
        relation: ComponentGovernanceRelation,
        provenance: &db::EdgeProvenance,
    ) -> Result<(), MemoryError> {
        self.with_retired_sticky_edge_preflight(
            &edge.source_id,
            &edge.target_id,
            "given a provenance component-governance edge",
            |conn| {
                db::add_component_governance_edge_with_provenance(conn, edge, relation, provenance)
            },
        )
    }

    /// Remove a specific edge.
    pub fn remove_edge(
        &self,
        source_id: &str,
        target_id: &str,
        relation: &str,
    ) -> Result<bool, MemoryError> {
        self.with_retired_sticky_edge_preflight(
            source_id,
            target_id,
            "had a graph edge removed",
            |conn| db::remove_edge(conn, source_id, target_id, relation),
        )
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

    /// Get connected edges with a database-enforced row ceiling.
    pub fn get_edges_limited(
        &self,
        memory_id: &str,
        direction: &str,
        relation_filter: Option<&str>,
        limit: usize,
    ) -> Result<Vec<MemoryEdge>, MemoryError> {
        db::get_edges_limited(&self.conn, memory_id, direction, relation_filter, limit)
    }

    /// BFS expansion from seed IDs through the memory graph.
    ///
    /// tachi#1569: on the Wiki corpus store the expanded entries carry the
    /// internal-row exclusion. This surface has no post-expansion Rust filter
    /// of its own (unlike `hybrid_search`'s graph phase, which re-filters with
    /// `is_search_noise_entry`), so without the gate a public seed that
    /// neighbours a `wiki-rem:` draft or an operation-log row handed that row
    /// back verbatim.
    pub fn graph_expand(
        &self,
        seed_ids: &[String],
        max_hops: u32,
        relation_filter: Option<&str>,
    ) -> Result<GraphExpandResult, MemoryError> {
        db::graph_expand(
            &self.conn,
            seed_ids,
            max_hops,
            relation_filter,
            self.is_wiki_corpus_store(),
        )
    }

    /// Expand the graph with a global edge ceiling enforced in SQLite batches.
    pub fn graph_expand_limited(
        &self,
        seed_ids: &[String],
        max_hops: u32,
        relation_filter: Option<&str>,
        edge_limit: usize,
    ) -> Result<GraphExpandResult, MemoryError> {
        db::graph_expand_limited(
            &self.conn,
            seed_ids,
            max_hops,
            relation_filter,
            edge_limit,
            self.is_wiki_corpus_store(),
        )
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
        let tx = Transaction::new_unchecked(&self.conn, TransactionBehavior::Immediate)?;
        let ids = {
            let mut stmt = tx.prepare(
                "SELECT source_id FROM memory_edges WHERE relation='related_to' AND valid_to IS NULL
                 UNION SELECT target_id FROM memory_edges WHERE relation='related_to' AND valid_to IS NULL",
            )?;
            let ids = stmt
                .query_map([], |row| row.get::<_, String>(0))?
                .collect::<Result<Vec<_>, _>>()?;
            ids
        };
        for id in &ids {
            db::refuse_retired_sticky_row_within_tx(&tx, id, "had legacy graph fog closed")?;
        }
        let closed = db::close_related_to_fog(&tx)?;
        tx.commit()?;
        Ok(closed)
    }

    /// Ensure a deterministic anchor row exists for `(kind, key)` (tachi#773
    /// item 4). `INSERT OR IGNORE` semantics — idempotent, fails closed on a
    /// kind/key mismatch at an existing id. Returns the anchor's
    /// deterministic id.
    pub fn ensure_anchor(&self, kind: AnchorKind, key: &str) -> Result<String, MemoryError> {
        let _authorization =
            db::authorize_reserved_reference_write(&self.reserved_reference_write)?;
        db::ensure_anchor(&self.conn, kind, key)
    }
}
