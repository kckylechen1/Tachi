//! Agent known-state (context diffing) methods on [`MemoryStore`].

use std::collections::HashMap;

use crate::db;
use crate::error::MemoryError;
use crate::MemoryStore;

impl MemoryStore {
    /// `agent_known_state` is a product table (#1585 D4): absent on
    /// `PortableKernel` stores, so both wrappers refuse typed instead of
    /// surfacing `no such table` after data is already in the store.
    fn require_agent_state_table(&self) -> Result<(), MemoryError> {
        if self.profile.includes_product() {
            return Ok(());
        }
        Err(MemoryError::StoreProfileMismatch {
            required: db::StoreProfile::TachiFull.as_str().to_string(),
            stored: self.profile.as_str().to_string(),
            db_path: self.conn.path().unwrap_or(":memory:").to_string(),
        })
    }

    /// Get the known revisions for a set of memory IDs for a given agent.
    pub fn get_agent_known_revisions(
        &self,
        agent_id: &str,
        memory_ids: &[String],
    ) -> Result<HashMap<String, i64>, MemoryError> {
        self.require_agent_state_table()?;
        db::get_agent_known_revisions(&self.conn, agent_id, memory_ids)
    }

    /// Update the agent's known state for a set of memory entries.
    pub fn update_agent_known_state(
        &self,
        agent_id: &str,
        entries: &[(String, i64)],
    ) -> Result<(), MemoryError> {
        self.require_agent_state_table()?;
        db::update_agent_known_state(&self.conn, agent_id, entries)
    }
}
