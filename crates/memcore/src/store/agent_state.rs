//! Agent known-state (context diffing) methods on [`MemoryStore`].

use std::collections::HashMap;

use crate::db;
use crate::error::MemoryError;
use crate::MemoryStore;

impl MemoryStore {
    /// Get the known revisions for a set of memory IDs for a given agent.
    pub fn get_agent_known_revisions(
        &self,
        agent_id: &str,
        memory_ids: &[String],
    ) -> Result<HashMap<String, i64>, MemoryError> {
        db::get_agent_known_revisions(&self.conn, agent_id, memory_ids)
    }

    /// Update the agent's known state for a set of memory entries.
    pub fn update_agent_known_state(
        &self,
        agent_id: &str,
        entries: &[(String, i64)],
    ) -> Result<(), MemoryError> {
        db::update_agent_known_state(&self.conn, agent_id, entries)
    }
}
