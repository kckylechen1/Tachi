//! Kanban/handoff task-card status lookups on [`MemoryStore`].

use serde_json::{json, Value};

use crate::{error::MemoryError, MemoryStore};

#[derive(Debug, Clone)]
pub struct TaskCardStatus {
    pub category: String,
    pub archived: bool,
    pub metadata: Value,
}

impl MemoryStore {
    /// Load kanban/handoff cards whose summary equals either candidate text.
    pub fn task_cards_by_summary(
        &self,
        clean_text: &str,
        raw_text: &str,
    ) -> Result<Vec<TaskCardStatus>, MemoryError> {
        let mut stmt = self.conn.prepare(
            "SELECT category, archived, metadata FROM memories WHERE category IN ('kanban', 'handoff') AND (summary = ?1 OR summary = ?2)",
        )?;
        let rows = stmt.query_map(rusqlite::params![clean_text, raw_text], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, bool>(1)?,
                row.get::<_, String>(2)?,
            ))
        })?;
        let mut cards = Vec::new();
        for row in rows {
            let (category, archived, metadata_str) = row?;
            cards.push(TaskCardStatus {
                category,
                archived,
                metadata: serde_json::from_str(&metadata_str).unwrap_or(json!({})),
            });
        }
        Ok(cards)
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use crate::types::MemoryEntry;
    use crate::MemoryStore;

    fn card(id: &str, category: &str, summary: &str, metadata: serde_json::Value) -> MemoryEntry {
        MemoryEntry {
            id: id.to_string(),
            path: "/test".to_string(),
            summary: summary.to_string(),
            text: format!("{id} text"),
            importance: 0.5,
            timestamp: "2026-07-05T00:00:00Z".to_string(),
            valid_from: String::new(),
            valid_until: None,
            category: category.to_string(),
            topic: String::new(),
            keywords: Vec::new(),
            persons: Vec::new(),
            entities: Vec::new(),
            location: String::new(),
            source: "test".to_string(),
            scope: "general".to_string(),
            archived: false,
            access_count: 0,
            scored_count: 0,
            last_access: None,
            last_use_at: None,
            revision: 1,
            metadata,
            vector: None,
            retention_policy: None,
            domain: None,
            recall_count: 0,
            query_diversity: 0,
            tier: "raw".to_string(),
        }
    }

    #[test]
    fn task_cards_by_summary_matches_kanban_and_handoff_summaries() {
        let mut store = MemoryStore::open_in_memory().expect("open test store");
        store
            .upsert(&card(
                "kanban-card",
                "kanban",
                "fix flaky queue",
                json!({ "status": "resolved" }),
            ))
            .expect("seed kanban");
        store
            .upsert(&card(
                "handoff-card",
                "handoff",
                "P0: fix flaky queue",
                json!({ "acknowledged": true }),
            ))
            .expect("seed handoff");
        store
            .upsert(&card(
                "plain-fact",
                "fact",
                "fix flaky queue",
                json!({ "status": "resolved" }),
            ))
            .expect("seed fact");

        let mut cards = store
            .task_cards_by_summary("fix flaky queue", "P0: fix flaky queue")
            .expect("query cards");
        cards.sort_by(|a, b| a.category.cmp(&b.category));

        assert_eq!(cards.len(), 2);
        assert_eq!(cards[0].category, "handoff");
        assert_eq!(cards[0].metadata["acknowledged"], true);
        assert_eq!(cards[1].category, "kanban");
        assert!(!cards[1].archived);
        assert_eq!(cards[1].metadata["status"], "resolved");
    }
}
