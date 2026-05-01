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

    /// Enable or disable a pack.
    pub fn pack_set_enabled(&self, id: &str, enabled: bool) -> Result<bool, MemoryError> {
        db::pack_set_enabled(&self.conn, id, enabled)
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

    /// Delete an agent projection.
    pub fn projection_delete(&self, agent: &str, pack_id: &str) -> Result<bool, MemoryError> {
        db::projection_delete(&self.conn, agent, pack_id)
    }
}
