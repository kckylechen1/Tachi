//! Event claim and continuity-event methods on [`MemoryStore`].

use crate::{
    db,
    error::MemoryError,
    types::{ContinuityMetrics, TachiEventQuery, TachiEventRecord},
    MemoryStore,
};

impl MemoryStore {
    /// Atomically try to claim an event for processing.
    /// Returns true if claimed (first processor), false if already processed.
    pub fn try_claim_event(
        &self,
        event_hash: &str,
        event_id: &str,
        worker: &str,
    ) -> Result<bool, MemoryError> {
        db::try_claim_event(&self.conn, event_hash, event_id, worker)
    }

    /// Release a claimed event on processing failure (at-least-once delivery).
    pub fn release_event_claim(&self, event_hash: &str, worker: &str) -> Result<(), MemoryError> {
        db::release_event_claim(&self.conn, event_hash, worker)
    }

    /// Append a domain-neutral continuity event for typed projectors.
    pub fn insert_tachi_event(&self, event: &TachiEventRecord) -> Result<(), MemoryError> {
        db::insert_tachi_event(&self.conn, event)
    }

    /// List recent continuity events with optional metadata filters.
    pub fn list_tachi_events(
        &self,
        query: &TachiEventQuery,
    ) -> Result<Vec<TachiEventRecord>, MemoryError> {
        db::list_tachi_events(&self.conn, query)
    }

    /// Compute read-only continuity metrics over recent append-only events.
    pub fn continuity_metrics(
        &self,
        window_event_limit: usize,
    ) -> Result<ContinuityMetrics, MemoryError> {
        db::continuity_metrics(&self.conn, window_event_limit)
    }
}
