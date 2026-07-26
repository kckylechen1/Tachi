//! Shared vector embedding backfill for CLI and daemon sweep.

use std::path::Path;
use std::time::Duration;

use memcore::{MemoryStore, VectorBackfillScope};
use rusqlite::OptionalExtension;
use serde::{Deserialize, Serialize};

use tachi_llm::LlmClient;

pub(crate) const AUTO_BACKFILL_COVERAGE_THRESHOLD: f64 = 0.99;
const DEFAULT_AUTO_BACKFILL_PENDING_THRESHOLD: usize = 0;
const VECTOR_SWEEP_STATE_TABLE: &str = "vector_sweep_state";
const VECTOR_SWEEP_STATE_NAMESPACE: &str = "vector_sweep_state";
const VECTOR_SWEEP_STATE_KEY: &str = "default";

#[derive(Debug, Clone)]
pub(crate) struct VectorSweepStateUpdate {
    pub(crate) enabled: bool,
    pub(crate) disabled_reason: Option<String>,
    pub(crate) skip_recall_cache: bool,
    pub(crate) embedded_count: usize,
    pub(crate) failed_count: usize,
    pub(crate) last_error: Option<String>,
    pub(crate) last_provider_error: Option<String>,
    pub(crate) interval_secs: Option<u64>,
    /// Keep existing interval_secs/next_run_after when this update leaves them unset.
    pub(crate) preserve_schedule: bool,
    /// Keep existing embedded/failed/error fields (no-op daemon tick).
    pub(crate) preserve_outcome: bool,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct VectorSweepState {
    pub(crate) enabled: bool,
    pub(crate) disabled_reason: Option<String>,
    pub(crate) skip_recall_cache: bool,
    pub(crate) last_run_at: String,
    pub(crate) embedded_count: usize,
    pub(crate) failed_count: usize,
    pub(crate) last_error: Option<String>,
    pub(crate) last_provider_error: Option<String>,
    pub(crate) next_run_after: Option<String>,
    pub(crate) interval_secs: Option<u64>,
    pub(crate) updated_at: String,
    /// Current status-time count using the same predicate as sweep selection.
    pub(crate) current_total_count: usize,
    pub(crate) current_with_vector_count: usize,
    pub(crate) current_pending_count: usize,
    pub(crate) current_pending_threshold: usize,
    pub(crate) current_backfill_needed: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct StoredVectorSweepState {
    enabled: bool,
    disabled_reason: Option<String>,
    skip_recall_cache: bool,
    last_run_at: String,
    embedded_count: usize,
    failed_count: usize,
    last_error: Option<String>,
    last_provider_error: Option<String>,
    next_run_after: Option<String>,
    interval_secs: Option<u64>,
    updated_at: String,
}

impl StoredVectorSweepState {
    fn from_update(
        update: VectorSweepStateUpdate,
        previous: Option<&Self>,
        now: chrono::DateTime<chrono::Utc>,
    ) -> Self {
        let interval_secs = if update.preserve_schedule {
            update
                .interval_secs
                .or_else(|| previous.and_then(|state| state.interval_secs))
        } else {
            update.interval_secs
        };
        let next_run_after = if update.preserve_schedule {
            update
                .interval_secs
                .map(|secs| {
                    (now + chrono::Duration::seconds(secs.min(i64::MAX as u64) as i64)).to_rfc3339()
                })
                .or_else(|| previous.and_then(|state| state.next_run_after.clone()))
        } else {
            update.interval_secs.map(|secs| {
                (now + chrono::Duration::seconds(secs.min(i64::MAX as u64) as i64)).to_rfc3339()
            })
        };
        let preserve_outcome = update.preserve_outcome && previous.is_some();
        let timestamp = now.to_rfc3339();

        Self {
            enabled: update.enabled,
            disabled_reason: update.disabled_reason,
            skip_recall_cache: update.skip_recall_cache,
            last_run_at: timestamp.clone(),
            embedded_count: if preserve_outcome {
                previous.expect("checked above").embedded_count
            } else {
                update.embedded_count
            },
            failed_count: if preserve_outcome {
                previous.expect("checked above").failed_count
            } else {
                update.failed_count
            },
            last_error: if preserve_outcome {
                previous.expect("checked above").last_error.clone()
            } else {
                update.last_error
            },
            last_provider_error: if preserve_outcome {
                previous.expect("checked above").last_provider_error.clone()
            } else {
                update.last_provider_error
            },
            next_run_after,
            interval_secs,
            updated_at: timestamp,
        }
    }

    fn into_status(self) -> VectorSweepState {
        VectorSweepState {
            enabled: self.enabled,
            disabled_reason: self.disabled_reason,
            skip_recall_cache: self.skip_recall_cache,
            last_run_at: self.last_run_at,
            embedded_count: self.embedded_count,
            failed_count: self.failed_count,
            last_error: self.last_error,
            last_provider_error: self.last_provider_error,
            next_run_after: self.next_run_after,
            interval_secs: self.interval_secs,
            updated_at: self.updated_at,
            current_total_count: 0,
            current_with_vector_count: 0,
            current_pending_count: 0,
            current_pending_threshold: 0,
            current_backfill_needed: false,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct VectorSweepOutcome {
    pub(crate) embedded_count: usize,
    pub(crate) attempted_count: usize,
    pub(crate) skipped_count: usize,
    pub(crate) failed_count: usize,
    /// Remaining rows from this sweep attempt's selected worklist, not a status read model.
    pub(crate) remaining_count: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct VectorSweepError {
    pub(crate) embedded_count: usize,
    pub(crate) attempted_count: usize,
    pub(crate) skipped_count: usize,
    pub(crate) failed_count: usize,
    /// Remaining rows from this sweep attempt's selected worklist, not a status read model.
    pub(crate) remaining_count: usize,
    pub(crate) message: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct VectorBatchWriteOutcome {
    pub(crate) written_count: usize,
    pub(crate) skipped_count: usize,
    pub(crate) failed_count: usize,
}

fn embedding_input(text: &str, summary: &str) -> String {
    let t = text.trim();
    let summary = summary.trim();
    let s = if t.chars().count() > 500 && !summary.is_empty() {
        summary
    } else if t.len() < 10 && !summary.is_empty() {
        summary
    } else {
        t
    };
    if s.len() > 8000 {
        s.chars().take(8000).collect()
    } else {
        s.to_string()
    }
}

/// Load entries missing vectors, optionally skipping ephemeral recall-cache rows.
pub(crate) fn list_missing_vector_entries(
    store: &MemoryStore,
    skip_recall_cache: bool,
    limit: Option<usize>,
) -> Result<Vec<(String, String, String, i64)>, String> {
    store
        .vector_backfill_entries(
            VectorBackfillScope {
                include_cache: !skip_recall_cache,
            },
            limit,
        )
        .map(|entries| {
            entries
                .into_iter()
                .map(|entry| (entry.id, entry.text, entry.summary, entry.revision))
                .collect()
        })
        .map_err(|e| format!("list missing vectors: {e}"))
}

pub(crate) fn auto_backfill_pending_threshold() -> usize {
    std::env::var("TACHI_VECTOR_SWEEP_PENDING_THRESHOLD")
        .ok()
        .and_then(|value| value.parse::<usize>().ok())
        .unwrap_or(DEFAULT_AUTO_BACKFILL_PENDING_THRESHOLD)
}

pub(crate) fn auto_backfill_needed(
    total: usize,
    with_vec: usize,
    pending_enrichment: usize,
    pending_threshold: usize,
) -> bool {
    if total == 0 || pending_enrichment == 0 {
        return false;
    }
    let coverage = with_vec as f64 / total as f64;
    pending_enrichment > pending_threshold || coverage < AUTO_BACKFILL_COVERAGE_THRESHOLD
}

fn vector_sweep_state_table_exists(conn: &rusqlite::Connection) -> bool {
    conn.query_row(
        "SELECT 1 FROM sqlite_master WHERE type='table' AND name=?1",
        [VECTOR_SWEEP_STATE_TABLE],
        |_| Ok(()),
    )
    .is_ok()
}

pub(crate) fn record_vector_sweep_state(
    db_path: &Path,
    update: VectorSweepStateUpdate,
) -> Result<(), String> {
    let db_str = db_path
        .to_str()
        .ok_or_else(|| format!("non-utf8 path: {}", db_path.display()))?;
    let store =
        MemoryStore::open(db_str).map_err(|e| format!("open {}: {e}", db_path.display()))?;
    let now = chrono::Utc::now();
    let previous = read_stored_vector_sweep_state(&store)?;
    let state = StoredVectorSweepState::from_update(update, previous.as_ref(), now);
    let payload =
        serde_json::to_string(&state).map_err(|e| format!("serialize vector sweep state: {e}"))?;
    store
        .set_state(
            VECTOR_SWEEP_STATE_NAMESPACE,
            VECTOR_SWEEP_STATE_KEY,
            &payload,
        )
        .map_err(|e| format!("write vector sweep state: {e}"))?;
    Ok(())
}

pub(crate) fn read_vector_sweep_state_for_status(
    db_path: &Path,
) -> Result<Option<VectorSweepState>, String> {
    let db_str = db_path
        .to_str()
        .ok_or_else(|| format!("non-utf8 path: {}", db_path.display()))?;
    let store = MemoryStore::open_read_only(db_str)
        .map_err(|e| format!("open {} read-only: {e}", db_path.display()))?;
    let Some(stored) = read_stored_vector_sweep_state(&store)? else {
        return Ok(None);
    };
    let mut state = stored.into_status();
    populate_current_sweep_counts(&store, &mut state)?;
    Ok(Some(state))
}

fn read_stored_vector_sweep_state(
    store: &MemoryStore,
) -> Result<Option<StoredVectorSweepState>, String> {
    if let Some((raw, _)) = store
        .get_state_kv(VECTOR_SWEEP_STATE_NAMESPACE, VECTOR_SWEEP_STATE_KEY)
        .map_err(|e| format!("read vector sweep state: {e}"))?
    {
        return serde_json::from_str(&raw)
            .map(Some)
            .map_err(|e| format!("read vector sweep state: decode hard_state payload: {e}"));
    }
    read_legacy_vector_sweep_state(store.connection())
}

fn read_legacy_vector_sweep_state(
    conn: &rusqlite::Connection,
) -> Result<Option<StoredVectorSweepState>, String> {
    if !vector_sweep_state_table_exists(conn) {
        return Ok(None);
    }
    conn.query_row(
        &format!(
            "SELECT enabled, disabled_reason, skip_recall_cache, last_run_at,
                    embedded_count, failed_count, last_error, last_provider_error,
                    next_run_after, interval_secs, updated_at
             FROM {VECTOR_SWEEP_STATE_TABLE}
             WHERE key=?1"
        ),
        [VECTOR_SWEEP_STATE_KEY],
        |row| {
            Ok(StoredVectorSweepState {
                enabled: row.get::<_, bool>(0)?,
                disabled_reason: row.get(1)?,
                skip_recall_cache: row.get::<_, bool>(2)?,
                last_run_at: row.get(3)?,
                embedded_count: row.get::<_, i64>(4)?.max(0) as usize,
                failed_count: row.get::<_, i64>(5)?.max(0) as usize,
                last_error: row.get(6)?,
                last_provider_error: row.get(7)?,
                next_run_after: row.get(8)?,
                interval_secs: row.get::<_, Option<i64>>(9)?.map(|n| n.max(0) as u64),
                updated_at: row.get(10)?,
            })
        },
    )
    .optional()
    .map_err(|e| format!("read vector sweep state: {e}"))
}

fn populate_current_sweep_counts(
    store: &MemoryStore,
    state: &mut VectorSweepState,
) -> Result<(), String> {
    let (total, with_vec) = vector_counts_filtered(store, state.skip_recall_cache)?;
    let pending = total.saturating_sub(with_vec);
    let threshold = auto_backfill_pending_threshold();
    state.current_total_count = total;
    state.current_with_vector_count = with_vec;
    state.current_pending_count = pending;
    state.current_pending_threshold = threshold;
    state.current_backfill_needed = auto_backfill_needed(total, with_vec, pending, threshold);
    Ok(())
}

fn vector_counts_filtered(
    store: &MemoryStore,
    skip_recall_cache: bool,
) -> Result<(usize, usize), String> {
    let counts = store
        .vector_backfill_counts(VectorBackfillScope {
            include_cache: !skip_recall_cache,
        })
        .map_err(|e| format!("vector backfill counts: {e}"))?;
    Ok((counts.total, counts.with_vector))
}

/// Embed and persist up to `batch_size` rows; partitions written, skipped, and failed rows.
pub(crate) async fn embed_and_write_batch(
    store: &mut MemoryStore,
    llm: &LlmClient,
    entries: &[(String, String, String, i64)],
) -> Result<VectorBatchWriteOutcome, String> {
    if entries.is_empty() {
        return Ok(VectorBatchWriteOutcome {
            written_count: 0,
            skipped_count: 0,
            failed_count: 0,
        });
    }
    #[cfg(test)]
    if let Some(result) = next_test_embed_batch_result() {
        match result {
            Ok(outcome) => {
                let dummy_vec = vec![0.0_f32; 1024];
                let mut actual = 0usize;
                for (id, _, _, revision) in entries.iter().take(outcome.written_count) {
                    let Some(mut entry) = store
                        .get(id)
                        .map_err(|e| format!("test vector read failed: {e}"))?
                    else {
                        continue;
                    };
                    if entry.revision == *revision {
                        entry.vector = Some(dummy_vec.clone());
                        store
                            .upsert(&entry)
                            .map_err(|e| format!("test vector write failed: {e}"))?;
                        actual += 1;
                    }
                }
                return Ok(VectorBatchWriteOutcome {
                    written_count: actual,
                    skipped_count: outcome.skipped_count,
                    failed_count: outcome.failed_count,
                });
            }
            Err(message) => return Err(message),
        }
    }

    let texts: Vec<String> = entries
        .iter()
        .map(|(_, text, summary, _)| embedding_input(text, summary))
        .collect();
    let texts: Vec<String> = texts
        .iter()
        .map(|t| crate::memory_search_ops::scrub_secrets(t).0)
        .collect();

    let vecs = llm
        .embed_voyage_batch(&texts, "document")
        .await
        .map_err(|e| format!("Voyage embed batch failed: {e}"))?;

    let mut written = 0usize;
    let mut skipped = 0usize;
    let mut failed = 0usize;
    for (i, (id, _, _, revision)) in entries.iter().enumerate() {
        if i >= vecs.len() {
            let dropped = entries.len().saturating_sub(i);
            failed += dropped;
            tracing::warn!(
                "[vector-backfill] provider returned {} vector(s) for {} row(s); {dropped} row(s) failed",
                vecs.len(),
                entries.len()
            );
            break;
        }
        match store.update_enrichment_fields(id, None, Some(&vecs[i]), None, None, *revision) {
            Ok(true) => written += 1,
            Ok(false) => {
                skipped += 1;
                tracing::debug!("[vector-backfill] revision mismatch for {id}, skipped");
            }
            Err(e) => {
                failed += 1;
                tracing::warn!("[vector-backfill] DB write failed for {id}: {e}");
            }
        }
    }
    Ok(VectorBatchWriteOutcome {
        written_count: written,
        skipped_count: skipped,
        failed_count: failed,
    })
}

/// Sweep one DB: at most `max_entries` missing rows embedded.
pub(crate) async fn sweep_db_vectors(
    db_path: &Path,
    llm: &LlmClient,
    max_entries: usize,
    skip_recall_cache: bool,
) -> Result<VectorSweepOutcome, VectorSweepError> {
    let db_str = db_path.to_str().ok_or_else(|| VectorSweepError {
        embedded_count: 0,
        attempted_count: 0,
        skipped_count: 0,
        failed_count: 0,
        remaining_count: 0,
        message: format!("non-utf8 path: {}", db_path.display()),
    })?;

    let mut store = MemoryStore::open(db_str).map_err(|e| VectorSweepError {
        embedded_count: 0,
        attempted_count: 0,
        skipped_count: 0,
        failed_count: 0,
        remaining_count: 0,
        message: format!("open {}: {e}", db_path.display()),
    })?;
    let (total, with_vec) =
        vector_counts_filtered(&store, skip_recall_cache).map_err(|message| VectorSweepError {
            embedded_count: 0,
            attempted_count: 0,
            skipped_count: 0,
            failed_count: 0,
            remaining_count: 0,
            message,
        })?;
    let pending = total.saturating_sub(with_vec);
    if !auto_backfill_needed(total, with_vec, pending, auto_backfill_pending_threshold()) {
        return Ok(VectorSweepOutcome {
            embedded_count: 0,
            attempted_count: 0,
            skipped_count: 0,
            failed_count: 0,
            remaining_count: pending,
        });
    }
    let missing = list_missing_vector_entries(&store, skip_recall_cache, Some(max_entries))
        .map_err(|message| VectorSweepError {
            embedded_count: 0,
            attempted_count: 0,
            skipped_count: 0,
            failed_count: 0,
            remaining_count: 0,
            message,
        })?;
    if missing.is_empty() {
        return Ok(VectorSweepOutcome {
            embedded_count: 0,
            attempted_count: 0,
            skipped_count: 0,
            failed_count: 0,
            remaining_count: 0,
        });
    }
    let todo = missing.len();

    let batch_size = 32usize.min(todo.max(1));
    let mut done = 0usize;
    let mut skipped = 0usize;
    let mut failed = 0usize;
    for chunk in missing.chunks(batch_size) {
        match embed_and_write_batch(&mut store, llm, chunk).await {
            Ok(outcome) => {
                done += outcome.written_count;
                skipped += outcome.skipped_count;
                failed += outcome.failed_count;
            }
            Err(message) => {
                return Err(VectorSweepError {
                    embedded_count: done,
                    attempted_count: todo,
                    skipped_count: skipped,
                    failed_count: failed + 1,
                    remaining_count: todo.saturating_sub(done),
                    message,
                });
            }
        }
        if done + skipped + failed < todo {
            tokio::time::sleep(Duration::from_millis(200)).await;
        }
    }
    Ok(VectorSweepOutcome {
        embedded_count: done,
        attempted_count: todo,
        skipped_count: skipped,
        failed_count: failed,
        remaining_count: todo.saturating_sub(done),
    })
}

#[cfg(test)]
static TEST_EMBED_BATCH_RESULTS: std::sync::OnceLock<
    std::sync::Mutex<std::collections::VecDeque<Result<VectorBatchWriteOutcome, String>>>,
> = std::sync::OnceLock::new();

#[cfg(test)]
pub(crate) fn set_test_embed_batch_results(results: Vec<Result<VectorBatchWriteOutcome, String>>) {
    let mut guard = TEST_EMBED_BATCH_RESULTS
        .get_or_init(|| std::sync::Mutex::new(std::collections::VecDeque::new()))
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    *guard = results.into();
}

#[cfg(test)]
fn next_test_embed_batch_result() -> Option<Result<VectorBatchWriteOutcome, String>> {
    TEST_EMBED_BATCH_RESULTS
        .get_or_init(|| std::sync::Mutex::new(std::collections::VecDeque::new()))
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .pop_front()
}

#[cfg(test)]
mod tests {
    use super::{
        auto_backfill_needed, embedding_input, list_missing_vector_entries,
        read_vector_sweep_state_for_status, vector_counts_filtered,
    };
    use crate::test_support::EnvRestore;
    use memcore::{MemoryEntry, MemoryStore};
    use serde_json::json;

    #[test]
    fn embedding_input_prefers_summary_for_long_text() {
        let long_text = "runtime details ".repeat(60);
        assert!(long_text.chars().count() > 500);

        assert_eq!(
            embedding_input(&long_text, "compact incident summary"),
            "compact incident summary"
        );
    }

    #[test]
    fn embedding_input_keeps_short_text_without_summary() {
        assert_eq!(
            embedding_input("short exact marker", ""),
            "short exact marker"
        );
        assert_eq!(
            embedding_input("tiny", "fallback summary"),
            "fallback summary"
        );
    }

    #[test]
    fn auto_backfill_starts_when_pending_or_coverage_crosses_threshold() {
        assert!(auto_backfill_needed(100, 99, 1, 0));
        assert!(auto_backfill_needed(200, 197, 3, 10));
        assert!(!auto_backfill_needed(100, 100, 0, 0));
        assert!(!auto_backfill_needed(1000, 990, 10, 10));
    }

    fn insert_memory(store: &mut MemoryStore, id: &str, source: &str, topic: &str) {
        let now = chrono::Utc::now().to_rfc3339();
        store
            .upsert(&MemoryEntry {
                id: id.to_string(),
                path: "/p".to_string(),
                summary: String::new(),
                text: "body".to_string(),
                importance: 0.5,
                timestamp: now.clone(),
                valid_from: now,
                valid_until: None,
                category: "fact".to_string(),
                topic: topic.to_string(),
                keywords: Vec::new(),
                persons: Vec::new(),
                entities: Vec::new(),
                location: String::new(),
                source: source.to_string(),
                scope: "project".to_string(),
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
            })
            .expect("insert typed memory fixture");
    }

    #[test]
    fn vector_selection_matches_status_recall_cache_predicate() {
        let dir = tempfile::tempdir().expect("tmp");
        let db_path = dir.path().join("selection.db");
        let mut store = MemoryStore::open(db_path.to_str().unwrap()).expect("open store");

        insert_memory(&mut store, "durable-1", "manual", "note");
        insert_memory(&mut store, "cache-topic", "auto", "recall_rerank_cache");

        let (total, with_vec) = vector_counts_filtered(&store, true).expect("counts");
        let selected =
            list_missing_vector_entries(&store, true, None).expect("list missing vectors");
        let selected_ids: Vec<_> = selected.iter().map(|(id, _, _, _)| id.as_str()).collect();

        assert_eq!(
            total, 1,
            "count basis must exclude recall-cache-shaped rows"
        );
        assert_eq!(with_vec, 0);
        assert_eq!(
            selected_ids,
            ["durable-1"],
            "selection basis must match the durable count basis"
        );
    }

    /// #744 / #1242: count↔selection membership must include archived rows that
    /// lack vectors. RED against a selection path that adds `archived = 0`
    /// while counts still include archived (pre-review-fix PR state).
    #[test]
    fn vector_selection_includes_archived_missing_vectors_like_counts() {
        let dir = tempfile::tempdir().expect("tmp");
        let db_path = dir.path().join("archived-selection.db");
        let mut store = MemoryStore::open(db_path.to_str().unwrap()).expect("open store");

        insert_memory(&mut store, "active-missing", "manual", "note");
        insert_memory(&mut store, "archived-missing", "manual", "note");
        store
            .archive_memory("archived-missing")
            .expect("archive typed memory fixture");

        let (total, with_vec) = vector_counts_filtered(&store, true).expect("counts");
        let selected =
            list_missing_vector_entries(&store, true, None).expect("list missing vectors");
        let mut selected_ids: Vec<_> = selected.iter().map(|(id, _, _, _)| id.as_str()).collect();
        selected_ids.sort();

        assert_eq!(total, 2, "counts include archived rows lacking vectors");
        assert_eq!(with_vec, 0);
        assert_eq!(
            selected_ids,
            ["active-missing", "archived-missing"],
            "selection must include archived missing-vector rows so status pending can clear"
        );
    }

    #[test]
    fn malformed_vector_sweep_state_is_error_not_absent() {
        let dir = tempfile::tempdir().expect("tmp");
        let db_path = dir.path().join("malformed-sweep-state.db");
        let store = MemoryStore::open(db_path.to_str().unwrap()).expect("open store");
        store
            .set_state(
                super::VECTOR_SWEEP_STATE_NAMESPACE,
                super::VECTOR_SWEEP_STATE_KEY,
                r#"{"enabled":"yes"}"#,
            )
            .expect("seed malformed typed state fixture");

        let err = read_vector_sweep_state_for_status(&db_path)
            .expect_err("malformed state must be surfaced");
        assert!(
            err.contains("read vector sweep state"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn status_sweep_state_recomputes_current_pending_after_threshold_change() {
        let _lock = crate::utils::global_test_lock()
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let _initial_threshold = EnvRestore::set("TACHI_VECTOR_SWEEP_PENDING_THRESHOLD", "10");

        let dir = tempfile::tempdir().expect("tmp");
        let db_path = dir.path().join("threshold-drift.db");
        let mut store = MemoryStore::open(db_path.to_str().unwrap()).expect("open store");
        insert_memory(&mut store, "threshold-pending-1", "manual", "note");
        insert_memory(&mut store, "threshold-pending-2", "manual", "note");
        insert_memory(&mut store, "threshold-pending-3", "manual", "note");
        super::record_vector_sweep_state(
            &db_path,
            super::VectorSweepStateUpdate {
                enabled: true,
                disabled_reason: None,
                skip_recall_cache: true,
                embedded_count: 0,
                failed_count: 0,
                last_error: None,
                last_provider_error: None,
                interval_secs: Some(1800),
                preserve_schedule: false,
                preserve_outcome: false,
            },
        )
        .expect("seed state");
        drop(_initial_threshold);

        let _raised_threshold = EnvRestore::set("TACHI_VECTOR_SWEEP_PENDING_THRESHOLD", "1");
        let state = read_vector_sweep_state_for_status(&db_path)
            .expect("read state")
            .expect("state exists");

        assert_eq!(state.embedded_count, 0, "last attempt count is preserved");
        assert_eq!(
            state.failed_count, 0,
            "last attempt failure count is preserved"
        );
        assert_eq!(state.current_total_count, 3);
        assert_eq!(state.current_with_vector_count, 0);
        assert_eq!(state.current_pending_count, 3);
        assert_eq!(state.current_pending_threshold, 1);
        assert!(
            state.current_backfill_needed,
            "status read model must reflect current threshold, not stale last attempt"
        );
    }
}
