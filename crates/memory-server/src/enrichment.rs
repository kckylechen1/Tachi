use super::*;

// ─── Enrichment Batcher ──────────────────────────────────────────────────────

/// An item queued for background embedding + summary enrichment.
#[derive(Debug, Clone)]
pub(super) struct EnrichmentItem {
    pub(super) id: String,
    pub(super) text: String,
    pub(super) summary: String,
    pub(super) keywords: Vec<String>,
    pub(super) entities: Vec<String>,
    pub(super) needs_embedding: bool,
    pub(super) needs_summary: bool,
    pub(super) needs_metadata: bool,
    pub(super) target_db: DbScope,
    pub(super) named_project: Option<String>,
    pub(super) db_path: Option<PathBuf>,
    pub(super) foundry_agent_id: Option<String>,
    pub(super) foundry_path_prefix: Option<String>,
    pub(super) revision: i64,
}

pub(super) fn needs_metadata_enrichment(keywords: &[String], entities: &[String]) -> bool {
    keywords.is_empty() || entities.is_empty()
}

pub(super) fn build_enrichment_item(
    entry: &MemoryEntry,
    needs_embedding: bool,
    needs_summary: bool,
    target_db: DbScope,
    named_project: Option<String>,
    db_path: Option<PathBuf>,
    foundry_agent_id: Option<String>,
    foundry_path_prefix: Option<String>,
    revision: i64,
) -> EnrichmentItem {
    EnrichmentItem {
        id: entry.id.clone(),
        text: entry.text.clone(),
        summary: entry.summary.clone(),
        keywords: entry.keywords.clone(),
        entities: entry.entities.clone(),
        needs_embedding,
        needs_summary,
        needs_metadata: needs_metadata_enrichment(&entry.keywords, &entry.entities),
        target_db,
        named_project,
        db_path,
        foundry_agent_id,
        foundry_path_prefix,
        revision,
    }
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
    let entities = item.entities.join(", ");
    let condensed = format!("{summary}\n{keywords}\n{entities}")
        .trim()
        .to_string();
    if condensed.is_empty() {
        item.text.clone()
    } else {
        condensed
    }
}

/// Batch enrichment queue configuration.
pub(super) const ENRICH_BATCH_MAX: usize = 32;
pub(super) const ENRICH_FLUSH_INTERVAL_MS: u64 = 500;

type MetadataExtractionResult = (usize, Result<(Vec<String>, Vec<String>), String>);

impl MemoryServer {
    pub(super) fn enqueue_enrichment(&self, item: EnrichmentItem) {
        if let Err(err) = self.enrichment_lock().enrich_tx.try_send(item) {
            tracing::warn!("[enrichment-batcher] failed to queue enrichment item: {err}");
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

        tracing::debug!("[enrichment-batcher] channel closed, worker exiting");
    }

    /// Flush a batch: batch-embed all texts needing embedding, then update DB.
    pub(super) async fn flush_enrichment_batch(&self, batch: &mut Vec<EnrichmentItem>) {
        let items: Vec<EnrichmentItem> = std::mem::take(batch);
        let batch_size = items.len();
        tracing::info!("[enrichment-batcher] flushing batch of {batch_size} items");

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
                    tracing::warn!(
                        "[enrichment-batcher] summary failed for {}: {e}",
                        items[idx].id
                    );
                    record_enrichment_failure(self, &items[idx], "summary", &e);
                }
            }
        }

        // 2. Extract keywords + entities for items missing structured metadata.
        let metadata_futures: Vec<_> = items
            .iter()
            .enumerate()
            .filter(|(_, item)| item.needs_metadata)
            .map(|(i, item)| {
                let llm = self.llm.clone();
                let text = item.text.clone();
                async move { (i, llm.extract_metadata(&text).await) }
            })
            .collect();

        let metadata_results: Vec<MetadataExtractionResult> =
            futures::future::join_all(metadata_futures).await;

        let mut keywords_out: Vec<Option<Vec<String>>> = vec![None; items.len()];
        let mut entities_out: Vec<Option<Vec<String>>> = vec![None; items.len()];
        for (idx, result) in metadata_results {
            match result {
                Ok((keywords, entities)) => {
                    if items[idx].keywords.is_empty() && !keywords.is_empty() {
                        keywords_out[idx] = Some(keywords);
                    }
                    if items[idx].entities.is_empty() && !entities.is_empty() {
                        entities_out[idx] = Some(entities);
                    }
                }
                Err(e) => {
                    tracing::warn!(
                        "[enrichment-batcher] metadata failed for {}: {e}",
                        items[idx].id
                    );
                    record_enrichment_failure(self, &items[idx], "metadata", &e);
                }
            }
        }

        // Deterministic domain-specific metadata (e.g. A-share ticker tagging)
        // is no longer derived in the generic engine — that domain logic lives
        // in the host project. LLM enrichment above already populated
        // keywords_out / entities_out.

        // 3. Batch embedding for items that need it
        let embed_indices: Vec<usize> = items
            .iter()
            .enumerate()
            .filter(|(_, item)| item.needs_embedding)
            .map(|(i, _)| i)
            .collect();

        let embed_texts: Vec<String> = embed_indices
            .iter()
            .map(|&i| {
                let generated_summary = summaries[i].as_deref();
                let mut item = items[i].clone();
                if let Some(kws) = keywords_out[i].as_ref() {
                    item.keywords = kws.clone();
                }
                if let Some(ents) = entities_out[i].as_ref() {
                    item.entities = ents.clone();
                }
                embedding_input_for_item(&item, generated_summary)
            })
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
                    tracing::info!(
                        "[enrichment-batcher] batch embedded {} texts in 1 API call",
                        embed_texts.len()
                    );
                }
                Err(e) => {
                    tracing::warn!("[enrichment-batcher] batch embedding failed: {e}");
                    for &item_idx in &embed_indices {
                        record_enrichment_failure(self, &items[item_idx], "embedding", &e);
                    }
                }
            }
        }

        // 4. Write results back to DB
        for (i, item) in items.iter().enumerate() {
            let new_vec = embed_results[i].as_deref();
            let new_summary = summaries[i].as_deref();
            let new_keywords = keywords_out[i].as_deref();
            let new_entities = entities_out[i].as_deref();

            if new_vec.is_some()
                || new_summary.is_some()
                || new_keywords.is_some()
                || new_entities.is_some()
            {
                let update_action = |store: &mut MemoryStore| {
                    let updated = store
                        .update_enrichment_fields(
                            &item.id,
                            new_summary,
                            new_vec,
                            new_keywords,
                            new_entities,
                            item.revision,
                        )
                        .map_err(|e| format!("Failed to update enriched entry: {e}"))?;
                    if updated && new_vec.is_some() {
                        if let Some(entry) = store
                            .get(&item.id)
                            .map_err(|e| format!("load enriched entry: {e}"))?
                        {
                            if let Err(err) =
                                crate::memory_search_ops::apply_confidence_reinforcement_links(
                                    store, &entry,
                                )
                            {
                                tracing::warn!(
                                    "[enrichment-batcher] confidence reinforcement failed for {}: {err}",
                                    item.id
                                );
                            }
                        }
                    }
                    Ok(updated)
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
                            let contradiction_server = self.clone();
                            let contradiction_id = item.id.clone();
                            let contradiction_db = item.target_db;
                            let contradiction_project = item.named_project.clone();
                            let contradiction_path = item.db_path.clone();
                            tokio::spawn(async move {
                                if let Err(err) =
                                    crate::memory_search_ops::apply_auto_contradiction_detection(
                                        &contradiction_server,
                                        &contradiction_id,
                                        contradiction_db,
                                        contradiction_project.as_deref(),
                                        contradiction_path.as_ref(),
                                    )
                                    .await
                                {
                                    tracing::warn!(
                                        "[enrichment-batcher] auto contradiction detection failed for {contradiction_id}: {err}"
                                    );
                                }
                            });

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
                                tracing::warn!(
                                    "[enrichment-batcher] failed to enqueue foundry maintenance for {}: {err}",
                                    item.id
                                );
                            }
                        }
                    }
                    Ok(false) => tracing::debug!(
                        "[enrichment-batcher] discarded {} (revision changed)",
                        item.id
                    ),
                    Err(e) => {
                        tracing::warn!(
                            "[enrichment-batcher] DB update failed for {}: {e}",
                            item.id
                        );
                        record_enrichment_failure(self, item, "db_update", &e);
                    }
                }
            }
        }

        tracing::info!("[enrichment-batcher] batch of {batch_size} complete");
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
        tracing::warn!(
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
    } else if let Some(db_path) = item.db_path.as_ref() {
        server.with_path_store_read(db_path, lookup).ok()?
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
