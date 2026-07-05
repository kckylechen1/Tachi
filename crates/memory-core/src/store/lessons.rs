//! Lesson memory helpers on [`MemoryStore`].

use std::collections::HashSet;

use serde_json::json;

use crate::{error::MemoryError, MemoryStore};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LessonDedupUpdate {
    pub id: String,
    pub count: u64,
}

fn lesson_task_matches(text: &str, expected_task: &str) -> bool {
    let expected = expected_task.trim();
    if expected.is_empty() {
        return false;
    }

    text.lines()
        .next()
        .and_then(|line| line.trim().strip_prefix("Task:"))
        .map(|task| task.trim().eq_ignore_ascii_case(expected))
        .unwrap_or(false)
}

impl MemoryStore {
    /// Find a recent lesson duplicate and update its seen count in one store boundary.
    pub fn record_lesson_dedup_seen(
        &self,
        expected_task: &str,
        outcome: &str,
        skills_used: &[String],
        last_seen: &str,
        recent_limit: usize,
    ) -> Result<Option<LessonDedupUpdate>, MemoryError> {
        if recent_limit == 0 {
            return Ok(None);
        }

        let skills_set: HashSet<&str> = skills_used.iter().map(String::as_str).collect();
        let candidate = {
            let mut stmt = self.conn.prepare(
                "SELECT id, text, metadata FROM memories \
                 WHERE path LIKE '/eval/lessons/%' \
                   AND id NOT LIKE 'foundry:%' \
                 ORDER BY created_at DESC LIMIT ?1",
            )?;
            let mut rows = stmt.query([recent_limit as i64])?;
            let mut candidate = None;
            while let Some(row) = rows.next()? {
                let id: String = row.get(0)?;
                let text: String = row.get(1)?;
                let meta_str: String = row.get(2)?;
                let meta: serde_json::Value = serde_json::from_str(&meta_str).unwrap_or(json!({}));
                let same_outcome = meta
                    .get("outcome")
                    .and_then(|v| v.as_str())
                    .map(|o| o == outcome)
                    .unwrap_or(false);
                let task_match = lesson_task_matches(&text, expected_task);
                let skill_overlap = same_outcome
                    && !skills_set.is_empty()
                    && meta
                        .get("skills_used")
                        .and_then(|v| v.as_array())
                        .map(|arr| {
                            arr.iter()
                                .filter_map(|v| v.as_str())
                                .any(|skill| skills_set.contains(skill))
                        })
                        .unwrap_or(false);
                if task_match || skill_overlap {
                    candidate = Some((id, meta));
                    break;
                }
            }
            candidate
        };

        let Some((id, mut metadata)) = candidate else {
            return Ok(None);
        };

        let count = metadata.get("count").and_then(|v| v.as_u64()).unwrap_or(1) + 1;
        metadata["count"] = json!(count);
        metadata["last_seen"] = json!(last_seen);
        let metadata_json = serde_json::to_string(&metadata)?;
        self.conn.execute(
            "UPDATE memories SET metadata = ?1 WHERE id = ?2",
            rusqlite::params![metadata_json, id],
        )?;

        Ok(Some(LessonDedupUpdate { id, count }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::MemoryEntry;

    fn test_entry(id: &str, text: &str, metadata: serde_json::Value) -> MemoryEntry {
        MemoryEntry {
            id: id.to_string(),
            path: format!("/eval/lessons/2026-07-05/{id}"),
            summary: "lesson fixture".to_string(),
            text: text.to_string(),
            importance: 0.75,
            timestamp: "2026-07-05T00:00:00Z".to_string(),
            valid_from: String::new(),
            valid_until: None,
            category: "lesson".to_string(),
            topic: "fixture".to_string(),
            keywords: vec!["lesson".to_string()],
            persons: Vec::new(),
            entities: Vec::new(),
            location: String::new(),
            source: "test".to_string(),
            scope: "project".to_string(),
            archived: false,
            access_count: 0,
            last_access: None,
            revision: 1,
            metadata,
            vector: None,
            retention_policy: Some("durable".to_string()),
            domain: None,
            recall_count: 0,
            query_diversity: 0,
            tier: "raw".to_string(),
        }
    }

    #[test]
    fn record_lesson_dedup_seen_matches_task_line_and_updates_metadata() {
        let mut store = MemoryStore::open_in_memory().expect("open test store");
        store
            .upsert(&test_entry(
                "task-match",
                "Task: Fix flaky queue\nOutcome: failure",
                json!({
                    "outcome": "failure",
                    "count": 1,
                    "last_seen": "2026-07-04T00:00:00Z",
                    "skills_used": ["debug"]
                }),
            ))
            .expect("seed lesson");

        let update = store
            .record_lesson_dedup_seen(
                "fix flaky queue",
                "partial",
                &[],
                "2026-07-05T00:00:00Z",
                30,
            )
            .expect("dedup update")
            .expect("duplicate found");

        assert_eq!(
            update,
            LessonDedupUpdate {
                id: "task-match".to_string(),
                count: 2,
            }
        );
        let stored = store
            .get("task-match")
            .expect("read lesson")
            .expect("lesson exists");
        assert_eq!(stored.metadata["count"], 2);
        assert_eq!(stored.metadata["last_seen"], "2026-07-05T00:00:00Z");
    }

    #[test]
    fn record_lesson_dedup_seen_matches_skill_overlap_only_when_outcome_matches() {
        let mut store = MemoryStore::open_in_memory().expect("open test store");
        store
            .upsert(&test_entry(
                "skill-match",
                "Task: Different task\nOutcome: failure",
                json!({
                    "outcome": "failure",
                    "count": 4,
                    "skills_used": ["debug", "rust"]
                }),
            ))
            .expect("seed matching lesson");
        store
            .upsert(&test_entry(
                "wrong-outcome",
                "Task: Another task\nOutcome: partial",
                json!({
                    "outcome": "partial",
                    "count": 9,
                    "skills_used": ["debug"]
                }),
            ))
            .expect("seed nonmatching lesson");

        let update = store
            .record_lesson_dedup_seen(
                "unrelated",
                "failure",
                &["rust".to_string()],
                "2026-07-05T01:00:00Z",
                30,
            )
            .expect("dedup update")
            .expect("duplicate found");

        assert_eq!(update.id, "skill-match");
        assert_eq!(update.count, 5);
        let wrong_outcome = store
            .get("wrong-outcome")
            .expect("read wrong outcome")
            .expect("wrong outcome exists");
        assert_eq!(wrong_outcome.metadata["count"], 9);
    }
}
