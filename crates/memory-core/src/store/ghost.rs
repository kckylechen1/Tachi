//! Ghost message-bus persistence + per-agent context-diff state on
//! [`MemoryStore`].

use std::collections::HashMap;

use crate::db;
use crate::error::MemoryError;
use crate::MemoryStore;

impl MemoryStore {
    // ─── Ghost Persistence ───────────────────────────────────────────────────

    /// Persist one ghost message and advance topic index.
    pub fn ghost_publish_message(
        &mut self,
        id: &str,
        topic: &str,
        payload_json: &str,
        publisher: &str,
        timestamp: &str,
    ) -> Result<u64, MemoryError> {
        db::ghost_publish_message(
            &mut self.conn,
            id,
            topic,
            payload_json,
            publisher,
            timestamp,
        )
    }

    /// Fetch topic messages after cursor index.
    pub fn ghost_fetch_messages_since(
        &self,
        topic: &str,
        after_index: u64,
        limit: usize,
    ) -> Result<Vec<serde_json::Value>, MemoryError> {
        db::ghost_fetch_messages_since(&self.conn, topic, after_index, limit)
    }

    /// Upsert one subscription row.
    pub fn ghost_upsert_subscription(
        &self,
        agent_id: &str,
        topic: &str,
    ) -> Result<(), MemoryError> {
        db::ghost_upsert_subscription(&self.conn, agent_id, topic)
    }

    /// Read one agent-topic cursor.
    pub fn ghost_get_cursor(&self, agent_id: &str, topic: &str) -> Result<u64, MemoryError> {
        db::ghost_get_cursor(&self.conn, agent_id, topic)
    }

    /// Upsert one agent-topic cursor.
    pub fn ghost_set_cursor(
        &self,
        agent_id: &str,
        topic: &str,
        last_seen_index: u64,
    ) -> Result<(), MemoryError> {
        db::ghost_set_cursor(&self.conn, agent_id, topic, last_seen_index)
    }

    /// Resolve one message id to topic/index.
    pub fn ghost_get_message_topic_index(
        &self,
        message_id: &str,
    ) -> Result<Option<(String, u64)>, MemoryError> {
        db::ghost_get_message_topic_index(&self.conn, message_id)
    }

    /// Read total published count for topic.
    pub fn ghost_get_topic_total(&self, topic: &str) -> Result<u64, MemoryError> {
        db::ghost_get_topic_total(&self.conn, topic)
    }

    /// List active ghost topics.
    pub fn ghost_list_topics(&self, limit: usize) -> Result<Vec<serde_json::Value>, MemoryError> {
        db::ghost_list_topics(&self.conn, limit)
    }

    /// Insert one reflection row.
    pub fn ghost_insert_reflection(
        &self,
        id: &str,
        agent_id: &str,
        topic: Option<&str>,
        summary: &str,
        metadata_json: &str,
        timestamp: &str,
    ) -> Result<(), MemoryError> {
        db::ghost_insert_reflection(
            &self.conn,
            id,
            agent_id,
            topic,
            summary,
            metadata_json,
            timestamp,
        )
    }

    /// Fetch one ghost message by id.
    pub fn ghost_get_message(
        &self,
        message_id: &str,
    ) -> Result<Option<serde_json::Value>, MemoryError> {
        db::ghost_get_message(&self.conn, message_id)
    }

    /// Mark ghost message promoted.
    pub fn ghost_mark_message_promoted(
        &self,
        message_id: &str,
        importance: Option<f64>,
    ) -> Result<bool, MemoryError> {
        db::ghost_mark_message_promoted(&self.conn, message_id, importance)
    }

    // ─── Agent Known State (Context Diffing) ─────────────────────────────────

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
