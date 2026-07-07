//! Shared vector embedding backfill for CLI and daemon sweep.

use std::path::Path;
use std::time::Duration;

use memory_core::{
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
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct VectorSweepOutcome {
    pub(crate) embedded_count: usize,
    pub(crate) attempted_count: usize,
    pub(crate) remaining_count: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct VectorSweepError {
    pub(crate) embedded_count: usize,
    pub(crate) attempted_count: usize,
    pub(crate) remaining_count: usize,
    pub(crate) message: String,
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
                embedded_count=excluded.embedded_count,
                failed_count=excluded.failed_count,
                last_error=excluded.last_error,
                last_provider_error=excluded.last_provider_error,
                next_run_after=excluded.next_run_after,
                interval_secs=excluded.interval_secs,
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
            })
        },
    )
    .optional()
    .map_err(|e| format!("read vector sweep state: {e}"))
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

/// Embed and persist up to `batch_size` rows; returns count written.
pub(crate) async fn embed_and_write_batch(
    store: &mut MemoryStore,
    llm: &LlmClient,
    entries: &[(String, String, String, i64)],
) -> Result<usize, String> {
    if entries.is_empty() {
        return Ok(0);
    }
    #[cfg(test)]
    if let Some(result) = next_test_embed_batch_result() {
        match result {
            Ok(written) => {
                let dummy_vec = vec![0.0_f32; 1024];
                let mut actual = 0usize;
                for (id, _, _, revision) in entries.iter().take(written) {
                    if store
                        .update_enrichment_fields(id, None, Some(&dummy_vec), None, None, *revision)
                        .map_err(|e| format!("test vector write failed: {e}"))?
                    {
                        actual += 1;
                    }
                }
                return Ok(actual);
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
    for (i, (id, _, _, revision)) in entries.iter().enumerate() {
        if i >= vecs.len() {
            break;
        }
        match store.update_enrichment_fields(id, None, Some(&vecs[i]), None, None, *revision) {
            Ok(true) => written += 1,
            Ok(false) => tracing::debug!("[vector-backfill] revision mismatch for {id}, skipped"),
            Err(e) => tracing::warn!("[vector-backfill] DB write failed for {id}: {e}"),
        }
    }
    Ok(written)
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
        remaining_count: 0,
        message: format!("non-utf8 path: {}", db_path.display()),
    })?;

    let mut store = MemoryStore::open(db_str).map_err(|e| VectorSweepError {
        embedded_count: 0,
        attempted_count: 0,
        remaining_count: 0,
        message: format!("open {}: {e}", db_path.display()),
    })?;
    let (total, with_vec) =
        vector_counts_filtered(&store, skip_recall_cache).map_err(|message| VectorSweepError {
            embedded_count: 0,
            attempted_count: 0,
            remaining_count: 0,
            message,
        })?;
    let pending = total.saturating_sub(with_vec);
    if !auto_backfill_needed(total, with_vec, pending, auto_backfill_pending_threshold()) {
        return Ok(VectorSweepOutcome {
            embedded_count: 0,
            attempted_count: pending,
            remaining_count: pending,
        });
    }
    let missing = list_missing_vector_entries(&store, skip_recall_cache, Some(max_entries))
        .map_err(|message| VectorSweepError {
            embedded_count: 0,
            attempted_count: 0,
            remaining_count: 0,
            message,
        })?;
    if missing.is_empty() {
        return Ok(VectorSweepOutcome {
            embedded_count: 0,
            attempted_count: 0,
            remaining_count: 0,
        });
    }
    let todo = missing.len();

    let batch_size = 32usize.min(todo.max(1));
    let mut done = 0usize;
    for chunk in missing.chunks(batch_size) {
        match embed_and_write_batch(&mut store, llm, chunk).await {
            Ok(written) => done += written,
            Err(message) => {
                return Err(VectorSweepError {
                    embedded_count: done,
                    attempted_count: todo,
                    remaining_count: todo.saturating_sub(done),
                    message,
                });
            }
        }
        if done < todo {
            tokio::time::sleep(Duration::from_millis(200)).await;
        }
    }
    Ok(VectorSweepOutcome {
        embedded_count: done,
        attempted_count: todo,
        remaining_count: todo.saturating_sub(done),
    })
}

#[cfg(test)]
static TEST_EMBED_BATCH_RESULTS: std::sync::OnceLock<
    std::sync::Mutex<std::collections::VecDeque<Result<usize, String>>>,
> = std::sync::OnceLock::new();

#[cfg(test)]
pub(crate) fn set_test_embed_batch_results(results: Vec<Result<usize, String>>) {
    let mut guard = TEST_EMBED_BATCH_RESULTS
        .get_or_init(|| std::sync::Mutex::new(std::collections::VecDeque::new()))
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    *guard = results.into();
}

#[cfg(test)]
fn next_test_embed_batch_result() -> Option<Result<usize, String>> {
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
    use memory_core::MemoryStore;
    use rusqlite::params;

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
}
