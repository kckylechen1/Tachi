//! Foundry distill candidate scans on [`MemoryStore`].

use std::collections::HashSet;

use serde_json::{json, Value};

use crate::{db, error::MemoryError, MemoryEntry, MemoryStore};

impl MemoryStore {
    /// Collect the `source_memory_ids` already covered by recent distill
    /// outputs written with `distill_source`.
    pub fn distill_processed_source_ids(
        &self,
        distill_source: &str,
        scan_limit: i64,
    ) -> Result<HashSet<String>, MemoryError> {
        let mut stmt = self.conn.prepare(
            "SELECT metadata FROM memories
             WHERE archived = 0 AND source = ?1
             ORDER BY timestamp DESC
             LIMIT ?2",
        )?;
        let rows = stmt.query_map(rusqlite::params![distill_source, scan_limit], |row| {
            row.get::<_, String>(0)
        })?;
        let mut processed_ids = HashSet::new();
        for row in rows {
            let raw = row?;
            let metadata: Value = serde_json::from_str(&raw).unwrap_or_else(|_| json!({}));
            processed_ids.extend(
                metadata
                    .get("source_memory_ids")
                    .and_then(|v| v.as_array())
                    .into_iter()
                    .flatten()
                    .filter_map(|v| v.as_str())
                    .map(ToOwned::to_owned),
            );
        }
        Ok(processed_ids)
    }

    /// Load active distill candidate entries oldest-first, excluding rows
    /// already produced by `exclude_source`.
    pub fn distill_candidate_entries(
        &self,
        exclude_source: &str,
        scan_limit: i64,
    ) -> Result<Vec<MemoryEntry>, MemoryError> {
        let mut stmt = self.conn.prepare(&format!(
            "SELECT {} FROM memories
             WHERE archived = 0 AND source != ?1
             ORDER BY timestamp ASC
             LIMIT ?2",
            db::MEMORY_SELECT_COLUMNS
        ))?;
        let rows = stmt.query_map(
            rusqlite::params![exclude_source, scan_limit],
            db::row_to_entry,
        )?;
        let mut entries = Vec::new();
        for row in rows {
            entries.push(row?);
        }
        Ok(entries)
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use crate::types::MemoryEntry;
    use crate::MemoryStore;

    fn test_entry(id: &str, source: &str, timestamp: &str) -> MemoryEntry {
        MemoryEntry {
            id: id.to_string(),
            path: "/test".to_string(),
            summary: format!("{id} summary"),
            text: format!("{id} text"),
            importance: 0.5,
            timestamp: timestamp.to_string(),
            valid_from: String::new(),
            valid_until: None,
            category: "fact".to_string(),
            topic: String::new(),
            keywords: Vec::new(),
            persons: Vec::new(),
            entities: Vec::new(),
            location: String::new(),
            source: source.to_string(),
            scope: "general".to_string(),
            archived: false,
            access_count: 0,
            last_access: None,
            last_use_at: None,
            revision: 1,
            metadata: json!({}),
            vector: None,
            retention_policy: None,
            domain: None,
            recall_count: 0,
            query_diversity: 0,
            tier: "raw".to_string(),
        }
    }

    #[test]
    fn distill_processed_source_ids_collects_ids_from_output_metadata() {
        let mut store = MemoryStore::open_in_memory().expect("open test store");
        let mut output = test_entry("output", "foundry_distill", "2026-07-05T00:00:00Z");
        output.metadata = json!({ "source_memory_ids": ["a", "b"] });
        store.upsert(&output).expect("seed output");
        let mut archived = test_entry("archived", "foundry_distill", "2026-07-04T00:00:00Z");
        archived.metadata = json!({ "source_memory_ids": ["c"] });
        archived.archived = true;
        store.upsert(&archived).expect("seed archived");

        let processed = store
            .distill_processed_source_ids("foundry_distill", 100)
            .expect("collect processed ids");
        assert_eq!(processed.len(), 2);
        assert!(processed.contains("a"));
        assert!(processed.contains("b"));
    }

    #[test]
    fn distill_candidate_entries_excludes_source_and_orders_oldest_first() {
        let mut store = MemoryStore::open_in_memory().expect("open test store");
        store
            .upsert(&test_entry("newer", "test", "2026-07-02T00:00:00Z"))
            .expect("seed newer");
        store
            .upsert(&test_entry("older", "test", "2026-07-01T00:00:00Z"))
            .expect("seed older");
        store
            .upsert(&test_entry(
                "distilled",
                "foundry_distill",
                "2026-06-30T00:00:00Z",
            ))
            .expect("seed distilled");
        let mut archived = test_entry("archived", "test", "2026-06-29T00:00:00Z");
        archived.archived = true;
        store.upsert(&archived).expect("seed archived");

        let entries = store
            .distill_candidate_entries("foundry_distill", 100)
            .expect("collect candidates");
        let ids: Vec<&str> = entries.iter().map(|entry| entry.id.as_str()).collect();
        assert_eq!(ids, vec!["older", "newer"]);
    }
}
