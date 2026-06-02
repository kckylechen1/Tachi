//! Shared vector embedding backfill for CLI and daemon sweep.

use std::path::Path;
use std::time::Duration;

use memory_core::MemoryStore;

use crate::llm::LlmClient;

const FOUNDRY_RECALL_CACHE_SOURCE: &str = "foundry_recall_rerank_cache";

fn embedding_input(text: &str, summary: &str) -> String {
    let t = text.trim();
    let s = if t.len() < 10 { summary } else { t };
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
) -> Result<Vec<(String, String, String, i64)>, String> {
    let mut out = Vec::new();
    for (id, text, summary, revision) in store
        .entries_missing_vectors()
        .map_err(|e| format!("list missing vectors: {e}"))?
    {
        if skip_recall_cache {
            if let Some(entry) = store.get(&id).map_err(|e| format!("load {id}: {e}"))? {
                if entry.source == FOUNDRY_RECALL_CACHE_SOURCE {
                    continue;
                }
            }
        }
        out.push((id, text, summary, revision));
    }
    Ok(out)
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

    let store = MemoryStore::open(db_str).map_err(|e| format!("open {}: {e}", db_path.display()))?;
    let mut missing = list_missing_vector_entries(&store, skip_recall_cache)?;
    if missing.is_empty() {
        return Ok((0, 0));
    }
    missing.truncate(max_entries);
    let todo = missing.len();

    drop(store);
    let mut store = MemoryStore::open(db_str).map_err(|e| format!("open mut: {e}"))?;

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