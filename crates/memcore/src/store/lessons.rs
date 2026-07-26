//! Lesson memory helpers on [`MemoryStore`].

use std::collections::HashSet;

use rusqlite::TransactionBehavior;
use serde_json::json;

use crate::{db, error::MemoryError, MemoryStore};

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

#[cfg(test)]
struct LessonDedupPause {
    entry_id: String,
    arrived: std::sync::Arc<std::sync::Barrier>,
    release: std::sync::Arc<std::sync::Barrier>,
}

#[cfg(test)]
static LESSON_DEDUP_PAUSE: std::sync::OnceLock<std::sync::Mutex<Option<LessonDedupPause>>> =
    std::sync::OnceLock::new();

#[cfg(test)]
fn pause_after_lesson_candidate_read(entry_id: &str) {
    let pause = LESSON_DEDUP_PAUSE.get().and_then(|slot| {
        slot.lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .as_ref()
            .filter(|pause| pause.entry_id == entry_id)
            .map(|pause| {
                (
                    std::sync::Arc::clone(&pause.arrived),
                    std::sync::Arc::clone(&pause.release),
                )
            })
    });
    if let Some((arrived, release)) = pause {
        arrived.wait();
        release.wait();
    }
}

impl MemoryStore {
    /// Find a recent lesson duplicate and update its seen count in one store boundary.
    pub fn record_lesson_dedup_seen(
        &mut self,
        expected_task: &str,
        outcome: &str,
        skills_used: &[String],
        last_seen: &str,
        recent_limit: usize,
    ) -> Result<Option<LessonDedupUpdate>, MemoryError> {
        if recent_limit == 0 {
            return Ok(None);
        }

        let _authorization =
            db::authorize_reserved_reference_write(&self.reserved_reference_write)?;
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let skills_set: HashSet<&str> = skills_used.iter().map(String::as_str).collect();
        let candidate = {
            let mut stmt = tx.prepare(
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

        let Some((id, metadata)) = candidate else {
            tx.commit()?;
            return Ok(None);
        };

        #[cfg(test)]
        pause_after_lesson_candidate_read(&id);

        let count = metadata.get("count").and_then(|v| v.as_u64()).unwrap_or(1) + 1;
        let count_sql = i64::try_from(count).map_err(|_| {
            MemoryError::InvalidArg("lesson dedup count exceeds SQLite integer range".to_string())
        })?;
        tx.execute(
            "UPDATE memories
             SET metadata = json_set(
                 CASE
                     WHEN json_valid(metadata) AND json_type(metadata) = 'object' THEN metadata
                     ELSE '{}'
                 END,
                 '$.count', ?1,
                 '$.last_seen', ?2
             )
             WHERE id = ?3",
            rusqlite::params![count_sql, last_seen, id],
        )?;
        tx.commit()?;

        Ok(Some(LessonDedupUpdate { id, count }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::MemoryEntry;
    use serde_json::Map;
    use std::sync::{mpsc, Arc, Barrier};
    use std::time::Duration;

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

    fn evidence(reference: &str) -> crate::db::ValidatedReferenceMutation {
        crate::db::ValidatedReferenceMutation::evidence(
            reference.to_string(),
            "2026-07-25T00:00:00Z".to_string(),
            None,
        )
        .expect("validated evidence")
    }

    #[test]
    fn lesson_dedup_serializes_with_trusted_reference_append() {
        let dir = tempfile::tempdir().expect("temp db dir");
        let path = dir.path().join("memory.db");
        let mut lesson_store = MemoryStore::open(&path.to_string_lossy()).unwrap();
        let mut trusted_store = MemoryStore::open(&path.to_string_lossy()).unwrap();
        let lesson = test_entry(
            "lesson-race",
            "Task: preserve provenance\nOutcome: failure",
            json!({
                "outcome": "failure",
                "skills_used": ["rust"],
                "count": 1,
            }),
        );
        let patch = lesson.metadata.as_object().cloned().unwrap_or_default();
        trusted_store
            .upsert_with_validated_reference_mutations(&lesson, None, &patch, &[evidence("#100")])
            .unwrap();

        let arrived = Arc::new(Barrier::new(2));
        let release = Arc::new(Barrier::new(2));
        let slot = LESSON_DEDUP_PAUSE.get_or_init(|| std::sync::Mutex::new(None));
        *slot.lock().unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(LessonDedupPause {
            entry_id: lesson.id.clone(),
            arrived: Arc::clone(&arrived),
            release: Arc::clone(&release),
        });

        let lesson_thread = std::thread::spawn(move || {
            lesson_store.record_lesson_dedup_seen(
                "preserve provenance",
                "failure",
                &["rust".to_string()],
                "2026-07-25T01:00:00Z",
                30,
            )
        });
        arrived.wait();

        let (done_tx, done_rx) = mpsc::channel();
        let trusted_lesson = lesson.clone();
        let append_thread = std::thread::spawn(move || {
            let result = trusted_store.upsert_with_validated_reference_mutations(
                &trusted_lesson,
                None,
                &Map::new(),
                &[evidence("#101")],
            );
            done_tx.send(()).unwrap();
            result
        });
        let append_finished_during_dedup = done_rx.recv_timeout(Duration::from_millis(300)).is_ok();
        release.wait();

        let update = lesson_thread.join().unwrap().unwrap().unwrap();
        append_thread.join().unwrap().unwrap();
        *slot.lock().unwrap_or_else(|poisoned| poisoned.into_inner()) = None;
        assert!(
            !append_finished_during_dedup,
            "trusted append committed between lesson read and metadata update"
        );
        let stored = MemoryStore::open(&path.to_string_lossy())
            .unwrap()
            .get(&lesson.id)
            .unwrap()
            .unwrap();
        let refs = stored.metadata["evidence_refs_v1"].as_array().unwrap();
        assert_eq!(
            refs.iter().map(|value| &value["ref"]).collect::<Vec<_>>(),
            vec![&json!("#100"), &json!("#101")]
        );
        assert_eq!(update.count, 2);
        assert_eq!(stored.metadata["count"], json!(2));
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
