//! Shared vector embedding backfill for CLI and daemon sweep.

use std::path::Path;
use std::time::Duration;

use memcore::{
    MemoryStore, FOUNDRY_RECALL_CACHE_SOURCE, RECALL_CACHE_SQL_WHERE, RECALL_CACHE_SQL_WHERE_M,
};
use rusqlite::OptionalExtension;
use serde::Serialize;

use tachi_llm::LlmClient;

pub(crate) const AUTO_BACKFILL_COVERAGE_THRESHOLD: f64 = 0.99;
const DEFAULT_AUTO_BACKFILL_PENDING_THRESHOLD: usize = 0;
const VECTOR_SWEEP_STATE_TABLE: &str = "vector_sweep_state";
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
        .entries_missing_vectors_filtered(
            if skip_recall_cache {
                Some(FOUNDRY_RECALL_CACHE_SOURCE)
            } else {
                None
            },
            limit,
        )
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

fn ensure_vector_sweep_state_table(conn: &rusqlite::Connection) -> Result<(), rusqlite::Error> {
    conn.execute(
        &format!(
            "CREATE TABLE IF NOT EXISTS {VECTOR_SWEEP_STATE_TABLE} (
                key TEXT PRIMARY KEY,
                enabled INTEGER NOT NULL,
                disabled_reason TEXT,
                skip_recall_cache INTEGER NOT NULL,
                last_run_at TEXT NOT NULL,
                embedded_count INTEGER NOT NULL,
                failed_count INTEGER NOT NULL,
                last_error TEXT,
                last_provider_error TEXT,
                next_run_after TEXT,
                interval_secs INTEGER,
                updated_at TEXT NOT NULL
            )"
        ),
        [],
    )?;
    Ok(())
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
    let conn = store.connection();
    ensure_vector_sweep_state_table(conn)
        .map_err(|e| format!("ensure vector sweep state table: {e}"))?;

    let now = chrono::Utc::now();
    let last_run_at = now.to_rfc3339();
    let next_run_after = update.interval_secs.map(|secs| {
        (now + chrono::Duration::seconds(secs.min(i64::MAX as u64) as i64)).to_rfc3339()
    });
    let schedule_clause = if update.preserve_schedule {
        "next_run_after=COALESCE(excluded.next_run_after, vector_sweep_state.next_run_after),
                interval_secs=COALESCE(excluded.interval_secs, vector_sweep_state.interval_secs)"
    } else {
        "next_run_after=excluded.next_run_after,
                interval_secs=excluded.interval_secs"
    };
    let outcome_clause = if update.preserve_outcome {
        "embedded_count=vector_sweep_state.embedded_count,
                failed_count=vector_sweep_state.failed_count,
                last_error=vector_sweep_state.last_error,
                last_provider_error=vector_sweep_state.last_provider_error"
    } else {
        "embedded_count=excluded.embedded_count,
                failed_count=excluded.failed_count,
                last_error=excluded.last_error,
                last_provider_error=excluded.last_provider_error"
    };
    conn.execute(
        &format!(
            "INSERT INTO {VECTOR_SWEEP_STATE_TABLE} (
                key, enabled, disabled_reason, skip_recall_cache, last_run_at,
                embedded_count, failed_count, last_error, last_provider_error,
                next_run_after, interval_secs, updated_at
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)
             ON CONFLICT(key) DO UPDATE SET
                enabled=excluded.enabled,
                disabled_reason=excluded.disabled_reason,
                skip_recall_cache=excluded.skip_recall_cache,
                last_run_at=excluded.last_run_at,
                {outcome_clause},
                {schedule_clause},
                updated_at=excluded.updated_at"
        ),
        rusqlite::params![
            VECTOR_SWEEP_STATE_KEY,
            update.enabled,
            update.disabled_reason,
            update.skip_recall_cache,
            last_run_at,
            update.embedded_count as i64,
            update.failed_count as i64,
            update.last_error,
            update.last_provider_error,
            next_run_after,
            update.interval_secs.map(|n| n as i64),
            chrono::Utc::now().to_rfc3339(),
        ],
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
    let conn = store.connection();
    if !vector_sweep_state_table_exists(conn) {
        return Ok(None);
    }
    let mut state = conn
        .query_row(
            &format!(
                "SELECT enabled, disabled_reason, skip_recall_cache, last_run_at,
                    embedded_count, failed_count, last_error, last_provider_error,
                    next_run_after, interval_secs, updated_at
             FROM {VECTOR_SWEEP_STATE_TABLE}
             WHERE key=?1"
            ),
            [VECTOR_SWEEP_STATE_KEY],
            |row| {
                Ok(VectorSweepState {
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
                    current_total_count: 0,
                    current_with_vector_count: 0,
                    current_pending_count: 0,
                    current_pending_threshold: 0,
                    current_backfill_needed: false,
                })
            },
        )
        .optional()
        .map_err(|e| format!("read vector sweep state: {e}"))?;
    if let Some(state) = state.as_mut() {
        populate_current_sweep_counts(&store, state)?;
    }
    Ok(state)
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
    if !skip_recall_cache {
        let (total, with_vec) = store
            .vector_stats()
            .map_err(|e| format!("vector stats: {e}"))?;
        return Ok((total.max(0) as usize, with_vec.max(0) as usize));
    }

    let total: i64 = store
        .connection()
        .query_row(
            &format!("SELECT COUNT(*) FROM memories WHERE NOT ({RECALL_CACHE_SQL_WHERE})"),
            [],
            |row| row.get(0),
        )
        .map_err(|e| format!("vector total stats: {e}"))?;
    let with_vec: i64 = store
        .connection()
        .query_row(
            &format!(
                "SELECT COUNT(DISTINCT v.id)
                 FROM memories_vec v
                 JOIN memories m ON m.id = v.id
                 WHERE NOT ({RECALL_CACHE_SQL_WHERE_M})"
            ),
            [],
            |row| row.get(0),
        )
        .map_err(|e| format!("vector populated stats: {e}"))?;
    Ok((total.max(0) as usize, with_vec.max(0) as usize))
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
                    if store
                        .update_enrichment_fields(id, None, Some(&dummy_vec), None, None, *revision)
                        .map_err(|e| format!("test vector write failed: {e}"))?
                    {
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
    use memcore::MemoryStore;
    use rusqlite::params;

    struct EnvGuard {
        key: &'static str,
        previous: Option<String>,
    }

    impl EnvGuard {
        fn set(key: &'static str, value: &str) -> Self {
            let previous = std::env::var(key).ok();
            std::env::set_var(key, value);
            Self { key, previous }
        }
    }

    impl Drop for EnvGuard {
        fn drop(&mut self) {
            if let Some(previous) = &self.previous {
                std::env::set_var(self.key, previous);
            } else {
                std::env::remove_var(self.key);
            }
        }
    }

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

    fn insert_memory(store: &MemoryStore, id: &str, source: &str, topic: &str) {
        let now = chrono::Utc::now().to_rfc3339();
        store
            .connection()
            .execute(
                "INSERT INTO memories (
                    id, path, summary, text, importance, timestamp, category, topic,
                    keywords, entities, source, scope, archived,
                    created_at, updated_at, access_count, revision, metadata
                 ) VALUES (?1, '/p', '', 'body', 0.5, ?2, 'fact', ?3,
                           '[]', '[]', ?4, 'project', 0,
                           ?2, ?2, 0, 1, '{}')",
                params![id, now, topic, source],
            )
            .expect("insert memory");
    }

    #[test]
    fn vector_selection_matches_status_recall_cache_predicate() {
        let dir = tempfile::tempdir().expect("tmp");
        let db_path = dir.path().join("selection.db");
        let store = MemoryStore::open(db_path.to_str().unwrap()).expect("open store");

        insert_memory(&store, "durable-1", "manual", "note");
        insert_memory(&store, "cache-topic", "auto", "recall_rerank_cache");

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

    #[test]
    fn malformed_vector_sweep_state_is_error_not_absent() {
        let dir = tempfile::tempdir().expect("tmp");
        let db_path = dir.path().join("malformed-sweep-state.db");
        let store = MemoryStore::open(db_path.to_str().unwrap()).expect("open store");
        store
            .connection()
            .execute(
                "CREATE TABLE vector_sweep_state (
                    key TEXT PRIMARY KEY,
                    enabled TEXT NOT NULL
                )",
                [],
            )
            .expect("create malformed state table");
        store
            .connection()
            .execute(
                "INSERT INTO vector_sweep_state (key, enabled) VALUES ('default', 'yes')",
                [],
            )
            .expect("insert malformed state row");

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
        let _initial_threshold = EnvGuard::set("TACHI_VECTOR_SWEEP_PENDING_THRESHOLD", "10");

        let dir = tempfile::tempdir().expect("tmp");
        let db_path = dir.path().join("threshold-drift.db");
        let store = MemoryStore::open(db_path.to_str().unwrap()).expect("open store");
        insert_memory(&store, "threshold-pending-1", "manual", "note");
        insert_memory(&store, "threshold-pending-2", "manual", "note");
        insert_memory(&store, "threshold-pending-3", "manual", "note");
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

        let _raised_threshold = EnvGuard::set("TACHI_VECTOR_SWEEP_PENDING_THRESHOLD", "1");
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
