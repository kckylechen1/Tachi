//! Pack registry + per-agent projection methods on [`MemoryStore`].

use crate::db;
use crate::error::MemoryError;
use crate::pack::{AgentProjection, Pack};
use crate::MemoryStore;

impl MemoryStore {
    /// Register or update a pack in the registry.
    pub fn pack_register(&self, pack: &Pack) -> Result<(), MemoryError> {
        db::pack_upsert(&self.conn, pack)
    }

    /// Get a pack by ID.
    pub fn pack_get(&self, id: &str) -> Result<Option<Pack>, MemoryError> {
        db::pack_get(&self.conn, id)
    }

    /// List all packs, optionally filtering by enabled status.
    pub fn pack_list(&self, enabled_only: bool) -> Result<Vec<Pack>, MemoryError> {
        db::pack_list(&self.conn, enabled_only)
    }

    /// Delete a pack by ID (also removes associated projections).
    pub fn pack_delete(&self, id: &str) -> Result<bool, MemoryError> {
        db::pack_delete(&self.conn, id)
    }

    /// Upsert an agent projection record.
    pub fn projection_upsert(&self, proj: &AgentProjection) -> Result<(), MemoryError> {
        db::projection_upsert(&self.conn, proj)
    }

    /// List agent projections, optionally filtering by agent and/or pack_id.
    pub fn projection_list(
        &self,
        agent: Option<&str>,
        pack_id: Option<&str>,
    ) -> Result<Vec<AgentProjection>, MemoryError> {
        db::projection_list(&self.conn, agent, pack_id)
    }
}
