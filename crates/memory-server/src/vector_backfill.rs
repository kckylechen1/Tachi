//! Shared vector embedding backfill for CLI and daemon sweep.

use std::path::Path;
use std::time::Duration;

use memory_core::{
    MemoryStore, FOUNDRY_RECALL_CACHE_SOURCE, RECALL_CACHE_SQL_WHERE, RECALL_CACHE_SQL_WHERE_M,
};

use tachi_llm::LlmClient;

pub(crate) const AUTO_BACKFILL_COVERAGE_THRESHOLD: f64 = 0.99;
const DEFAULT_AUTO_BACKFILL_PENDING_THRESHOLD: usize = 0;

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
) -> Result<(usize, usize), String> {
    let db_str = db_path
        .to_str()
        .ok_or_else(|| format!("non-utf8 path: {}", db_path.display()))?;

    let mut store =
        MemoryStore::open(db_str).map_err(|e| format!("open {}: {e}", db_path.display()))?;
    let (total, with_vec) = vector_counts_filtered(&store, skip_recall_cache)?;
    let pending = total.saturating_sub(with_vec);
    if !auto_backfill_needed(total, with_vec, pending, auto_backfill_pending_threshold()) {
        return Ok((0, pending));
    }
    let missing = list_missing_vector_entries(&store, skip_recall_cache, Some(max_entries))?;
    if missing.is_empty() {
        return Ok((0, 0));
    }
    let todo = missing.len();

    let batch_size = 32usize.min(todo.max(1));
    let mut done = 0usize;
    for chunk in missing.chunks(batch_size) {
        done += embed_and_write_batch(&mut store, llm, chunk).await?;
        if done < todo {
            tokio::time::sleep(Duration::from_millis(200)).await;
        }
    }
    Ok((done, todo))
}

#[cfg(test)]
mod tests {
    use super::{
        auto_backfill_needed, embedding_input, list_missing_vector_entries, vector_counts_filtered,
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
}
