use super::*;

// ─── Enrichment Batcher ──────────────────────────────────────────────────────

/// An item queued for background embedding + summary enrichment.
#[derive(Debug, Clone)]
pub(super) struct EnrichmentItem {
    pub(super) id: String,
    pub(super) text: String,
    pub(super) summary: String,
    pub(super) keywords: Vec<String>,
    pub(super) needs_embedding: bool,
    pub(super) needs_summary: bool,
    pub(super) target_db: DbScope,
    pub(super) named_project: Option<String>,
    pub(super) db_path: Option<PathBuf>,
    pub(super) foundry_agent_id: Option<String>,
    pub(super) foundry_path_prefix: Option<String>,
    pub(super) revision: i64,
}

fn embedding_input_for_item(item: &EnrichmentItem, generated_summary: Option<&str>) -> String {
    if item.text.len() <= 500 {
        return item.text.clone();
    }

    let summary = generated_summary
        .filter(|value| !value.trim().is_empty())
        .unwrap_or(item.summary.as_str())
        .trim();
    let keywords = item.keywords.join(", ");
    let condensed = format!("{summary}\n{keywords}").trim().to_string();
    if condensed.is_empty() {
        item.text.clone()
    } else {
        condensed
    }
}

/// Batch enrichment queue configuration.
pub(super) const ENRICH_BATCH_MAX: usize = 32;
pub(super) const ENRICH_FLUSH_INTERVAL_MS: u64 = 500;

impl MemoryServer {
    pub(super) fn enqueue_enrichment(&self, item: EnrichmentItem) {
        if let Err(err) = self.enrich_tx.try_send(item) {
            eprintln!("[enrichment-batcher] failed to queue enrichment item: {err}");
        }
    }

    /// Background worker that batches enrichment requests (embedding + summary).
    /// Flushes every ENRICH_FLUSH_INTERVAL_MS or when ENRICH_BATCH_MAX items accumulate.
    pub(super) async fn run_enrichment_batcher(
        server: MemoryServer,
        mut rx: mpsc::Receiver<EnrichmentItem>,
    ) {
        let mut batch: Vec<EnrichmentItem> = Vec::with_capacity(ENRICH_BATCH_MAX);
        let flush_interval = Duration::from_millis(ENRICH_FLUSH_INTERVAL_MS);

        loop {
            // Wait for first item or channel close
            let item = if batch.is_empty() {
                match rx.recv().await {
                    Some(item) => Some(item),
                    None => break, // channel closed
                }
            } else {
                None
            };

            if let Some(item) = item {
                batch.push(item);
            }

            // Drain more items until batch is full or timeout expires
            let deadline = tokio::time::Instant::now() + flush_interval;
            while batch.len() < ENRICH_BATCH_MAX {
                match tokio::time::timeout_at(deadline, rx.recv()).await {
                    Ok(Some(item)) => batch.push(item),
                    Ok(None) => {
                        // Channel closed; flush remaining and exit
                        if !batch.is_empty() {
                            server.flush_enrichment_batch(&mut batch).await;
                        }
                        return;
                    }
                    Err(_timeout) => break, // timer expired, flush what we have
                }
            }

            if !batch.is_empty() {
                server.flush_enrichment_batch(&mut batch).await;
            }
        }

        eprintln!("[enrichment-batcher] channel closed, worker exiting");
    }

    /// Flush a batch: batch-embed all texts needing embedding, then update DB.
    pub(super) async fn flush_enrichment_batch(&self, batch: &mut Vec<EnrichmentItem>) {
        let items: Vec<EnrichmentItem> = std::mem::take(batch);
        let batch_size = items.len();
        eprintln!("[enrichment-batcher] flushing batch of {batch_size} items");

        // 1. Generate summaries first so long-memory embeddings use condensed
        // semantic text instead of noisy full sessions.
        let summary_futures: Vec<_> = items
            .iter()
            .enumerate()
            .filter(|(_, item)| item.needs_summary)
            .map(|(i, item)| {
                let llm = self.llm.clone();
                let text = item.text.clone();
                async move { (i, llm.generate_summary(&text).await) }
            })
            .collect();

        let summary_results: Vec<(usize, Result<String, String>)> =
            futures::future::join_all(summary_futures).await;

        let mut summaries: Vec<Option<String>> = vec![None; items.len()];
        for (idx, result) in summary_results {
            match result {
                Ok(s) => summaries[idx] = Some(s),
                Err(e) => {
                    eprintln!(
                        "[enrichment-batcher] summary failed for {}: {e}",
                        items[idx].id
                    );
                    record_enrichment_failure(self, &items[idx], "summary", &e);
                }
            }
        }

        // 2. Batch embedding for items that need it
        let embed_indices: Vec<usize> = items
            .iter()
            .enumerate()
            .filter(|(_, item)| item.needs_embedding)
            .map(|(i, _)| i)
            .collect();

        let embed_texts: Vec<String> = embed_indices
            .iter()
            .map(|&i| embedding_input_for_item(&items[i], summaries[i].as_deref()))
            .collect();

        let mut embed_results: Vec<Option<Vec<f32>>> = vec![None; items.len()];

        if !embed_texts.is_empty() {
            match self.llm.embed_voyage_batch(&embed_texts, "document").await {
                Ok(vecs) => {
                    for (vec_idx, &item_idx) in embed_indices.iter().enumerate() {
                        if vec_idx < vecs.len() {
                            embed_results[item_idx] = Some(vecs[vec_idx].clone());
                        }
                    }
                    eprintln!(
                        "[enrichment-batcher] batch embedded {} texts in 1 API call",
                        embed_texts.len()
                    );
                }
                Err(e) => {
                    eprintln!("[enrichment-batcher] batch embedding failed: {e}");
                    for &item_idx in &embed_indices {
                        record_enrichment_failure(self, &items[item_idx], "embedding", &e);
                    }
                }
            }
        }

        // 3. Write results back to DB
        for (i, item) in items.iter().enumerate() {
            let new_vec = embed_results[i].as_deref();
            let new_summary = summaries[i].as_deref();

            if new_vec.is_some() || new_summary.is_some() {
                let update_action = |store: &mut MemoryStore| {
                    store
                        .update_enrichment_fields(&item.id, new_summary, new_vec, item.revision)
                        .map_err(|e| format!("Failed to update enriched entry: {e}"))
                };

                let res = if let Some(ref project_name) = item.named_project {
                    self.with_named_project_store(project_name, update_action)
                } else if let Some(ref db_path) = item.db_path {
                    self.with_path_store(db_path, update_action)
                } else {
                    self.with_store_for_scope(item.target_db, update_action)
                };

                match res {
                    Ok(true) => {
                        if new_vec.is_some() {
                            // PR-4: always-on save_memory enrichment.
                            //
                            // Previously the foundry maintenance enqueue only
                            // fired when both `foundry_agent_id` and
                            // `foundry_path_prefix` were set by the caller.
                            // The `handle_save_memory` path (and several
                            // pipeline call sites) passed None for both,
                            // which meant memories saved via save_memory
                            // never reached the foundry pipeline (no
                            // distill, no rerank). We now fall back to:
                            //   - agent_id: the server's current agent
                            //     profile id, else "system"
                            //   - path_prefix: derived from the entry's
                            //     stored path (parent directory), or "/"
                            //     when the path has no parent
                            // so every embedded memory becomes a
                            // foundry candidate. The dedup gate inside
                            // try_claim_event still suppresses no-op
                            // double-enqueues.
                            let agent_id_owned = item
                                .foundry_agent_id
                                .clone()
                                .or_else(|| {
                                    let guard = self.agent_runtime_read();
                                    guard.agent_profile.as_ref().map(|p| p.agent_id.clone())
                                })
                                .unwrap_or_else(|| "system".to_string());

                            let path_prefix_owned = match item.foundry_path_prefix.clone() {
                                Some(p) => p,
                                None => derive_path_prefix(self, item)
                                    .unwrap_or_else(|| "/".to_string()),
                            };

                            if let Err(err) = enqueue_foundry_capture_maintenance(
                                self,
                                item.target_db,
                                item.named_project.clone(),
                                item.db_path.clone(),
                                &agent_id_owned,
                                &path_prefix_owned,
                                &[item.id.clone()],
                            ) {
                                eprintln!(
                                    "[enrichment-batcher] failed to enqueue foundry maintenance for {}: {err}",
                                    item.id
                                );
                            }
                        }
                    }
                    Ok(false) => eprintln!(
                        "[enrichment-batcher] discarded {} (revision changed)",
                        item.id
                    ),
                    Err(e) => {
                        eprintln!("[enrichment-batcher] DB update failed for {}: {e}", item.id);
                        record_enrichment_failure(self, item, "db_update", &e);
                    }
                }
            }
        }

        eprintln!("[enrichment-batcher] batch of {batch_size} complete");
    }
}

fn record_enrichment_failure(
    server: &MemoryServer,
    item: &EnrichmentItem,
    stage: &str,
    error: &str,
) {
    let action = |store: &mut MemoryStore| {
        store
            .record_enrichment_failure(&item.id, stage, error)
            .map_err(|e| format!("record enrichment failure: {e}"))
    };
    let res = if let Some(ref project_name) = item.named_project {
        server.with_named_project_store(project_name, action)
    } else if let Some(ref db_path) = item.db_path {
        server.with_path_store(db_path, action)
    } else {
        server.with_store_for_scope(item.target_db, action)
    };
    if let Err(err) = res {
        eprintln!(
            "[enrichment-batcher] failed to record enrichment failure for {}: {err}",
            item.id
        );
    }
}

/// Look up the saved memory's `path` and return its parent directory as a
/// foundry path prefix. Used by the always-on enrichment fallback when the
/// caller did not pass an explicit `foundry_path_prefix`. Returns None if
/// the entry cannot be loaded; the caller falls back to "/".
fn derive_path_prefix(server: &MemoryServer, item: &EnrichmentItem) -> Option<String> {
    let lookup = |store: &mut MemoryStore| {
        store
            .get(&item.id)
            .map_err(|e| format!("derive path prefix get: {e}"))
    };
    let entry = if let Some(name) = item.named_project.as_deref() {
        server.with_named_project_store_read(name, lookup).ok()?
    } else {
        server
            .with_store_for_scope_read(item.target_db, lookup)
            .ok()?
    }?;
    let path = entry.path;
    if path.is_empty() {
        return Some("/".to_string());
    }

    let parent = std::path::Path::new(&path)
        .parent()
        .and_then(|p| p.to_str())
        .unwrap_or("");
    if parent.is_empty() || parent == "." {
        Some("/".to_string())
    } else {
        Some(parent.to_string())
    }
}
