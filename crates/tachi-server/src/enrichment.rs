use crate::foundry_runtime_ops::enqueue_foundry_capture_maintenance;
use crate::server_state::{DbScope, MemoryServer};
use memcore::{MemoryEntry, MemoryStore};
use std::collections::HashSet;
use std::path::PathBuf;
use std::time::Duration;
use tokio::sync::mpsc;

// ─── Enrichment Batcher ──────────────────────────────────────────────────────

/// Cap on merged write-side keyword sets to bound FTS index bloat (#921).
pub(crate) const MAX_ENRICHED_KEYWORDS: usize = 24;

/// Per-keyword max length at the enrichment boundary (#943 / FTS safety).
pub(crate) const MAX_KEYWORD_LEN: usize = 64;

/// Env flag for write-side synonym/bilingual keyword enrichment (default OFF).
pub(crate) const WRITE_ENRICH_KEYWORDS_ENV: &str = "TACHI_WRITE_ENRICH_KEYWORDS";

/// An item queued for background embedding + summary enrichment.
#[derive(Debug, Clone)]
pub(crate) struct EnrichmentItem {
    pub(crate) id: String,
    pub(crate) text: String,
    pub(crate) summary: String,
    pub(crate) keywords: Vec<String>,
    pub(crate) entities: Vec<String>,
    pub(crate) needs_embedding: bool,
    pub(crate) needs_summary: bool,
    pub(crate) needs_metadata: bool,
    /// Synonym + bilingual keyword expansion (#921). Gated by
    /// [`WRITE_ENRICH_KEYWORDS_ENV`]; default off ⇒ always false.
    pub(crate) needs_keyword_enrichment: bool,
    pub(crate) target_db: DbScope,
    pub(crate) named_project: Option<String>,
    pub(crate) db_path: Option<PathBuf>,
    pub(crate) foundry_agent_id: Option<String>,
    pub(crate) foundry_path_prefix: Option<String>,
    pub(crate) revision: i64,
}

pub(crate) fn needs_metadata_enrichment(keywords: &[String], _entities: &[String]) -> bool {
    keywords.is_empty()
}

/// Feature flag for write-side keyword enrichment. Default OFF.
pub(crate) fn write_keyword_enrichment_enabled() -> bool {
    std::env::var(WRITE_ENRICH_KEYWORDS_ENV)
        .map(|value| {
            let value = value.trim();
            value == "1" || value.eq_ignore_ascii_case("true") || value.eq_ignore_ascii_case("yes")
        })
        .unwrap_or(false)
}

fn keywords_status_from_entry(entry: &MemoryEntry) -> Option<&str> {
    entry
        .metadata
        .get("enrichment")
        .and_then(|value| value.get("keywords_status"))
        .and_then(|value| value.as_str())
}

/// Whether this entry still needs write-side synonym/bilingual keyword expansion.
///
/// Fail-closed: when the flag is off, always false (zero behavior change).
pub(crate) fn needs_keyword_enrichment(entry: &MemoryEntry) -> bool {
    if !write_keyword_enrichment_enabled() {
        return false;
    }
    match keywords_status_from_entry(entry) {
        Some("enriched") | Some("skipped") => false,
        // pending/failed/absent: eligible when there is text to enrich
        _ => !entry.text.trim().is_empty(),
    }
}

/// Sanitize one keyword at the enrichment write boundary (#943).
///
/// - strips C0/C1 control characters (U+0000–U+001F, U+007F–U+009F)
/// - trims whitespace
/// - rejects empty / pure-punctuation tokens
/// - truncates to [`MAX_KEYWORD_LEN`]
///
/// FTS insert path (`update_enrichment_fields`) writes keywords via parameterized
/// SQL and copies column content into `memories_fts` with `WHERE id = ?1` — the
/// keyword text is document content, not a MATCH expression — so FTS-operator
/// tokens (AND/OR/NEAR) are not injection vectors. Still drop control noise and
/// bound length so a hostile LLM cannot bloat or corrupt the index surface.
pub(crate) fn sanitize_enriched_keyword(raw: &str) -> Option<String> {
    let stripped: String = raw
        .chars()
        .filter(|c| {
            let u = *c as u32;
            !(u <= 0x1F || (0x7F..=0x9F).contains(&u))
        })
        .collect();
    let trimmed = stripped.trim();
    if trimmed.is_empty() {
        return None;
    }
    // Reject pure punctuation/whitespace (no letter or digit content).
    if !trimmed.chars().any(|c| c.is_alphanumeric()) {
        return None;
    }
    let truncated: String = trimmed.chars().take(MAX_KEYWORD_LEN).collect();
    if truncated.is_empty() {
        return None;
    }
    Some(truncated)
}

/// Merge existing + expanded keywords with sanitization, case-insensitive dedupe, and a hard cap.
pub(crate) fn merge_enriched_keywords(existing: &[String], expanded: &[String]) -> Vec<String> {
    let mut out = Vec::new();
    let mut seen = HashSet::new();
    for raw in existing.iter().chain(expanded.iter()) {
        let Some(clean) = sanitize_enriched_keyword(raw) else {
            continue;
        };
        let key = clean.to_ascii_lowercase();
        if !seen.insert(key) {
            continue;
        }
        out.push(clean);
        if out.len() >= MAX_ENRICHED_KEYWORDS {
            break;
        }
    }
    out
}

pub(crate) fn build_enrichment_item(
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
        needs_keyword_enrichment: needs_keyword_enrichment(entry),
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

fn external_llm_input(text: &str) -> String {
    crate::memory_search_ops::scrub_secrets(text).0
}

/// Batch enrichment queue configuration.
pub(super) const ENRICH_BATCH_MAX: usize = 32;
pub(super) const ENRICH_FLUSH_INTERVAL_MS: u64 = 500;

type MetadataExtractionResult = (usize, Result<(Vec<String>, Vec<String>), String>);

impl MemoryServer {
    pub(crate) fn enqueue_enrichment(&self, item: EnrichmentItem) -> bool {
        if let Err(err) = self.enrichment_lock().enrich_tx.try_send(item) {
            tracing::warn!("[enrichment-batcher] failed to queue enrichment item: {err}");
            return false;
        }
        true
    }

    pub(crate) fn requeue_auth_failed_enrichment_retries(&self, trigger: &str) -> usize {
        const LIMIT_PER_DB: usize = 64;
        let mut total = 0usize;

        match self.with_global_store(|store| {
            let candidates = store
                .claim_auth_failed_enrichment_retries(LIMIT_PER_DB)
                .map_err(|e| format!("claim global auth-failed enrichment retries: {e}"))?;
            Ok(self.enqueue_retry_candidates(candidates, DbScope::Global, None, None))
        }) {
            Ok(count) => total += count,
            Err(err) => tracing::warn!(
                "[enrichment-batcher] failed to requeue global auth-failed rows after {trigger}: {err}"
            ),
        }

        if self.project_db_path_buf().is_some() {
            match self.with_project_store(|store| {
                let candidates = store
                    .claim_auth_failed_enrichment_retries(LIMIT_PER_DB)
                    .map_err(|e| format!("claim project auth-failed enrichment retries: {e}"))?;
                Ok(self.enqueue_retry_candidates(candidates, DbScope::Project, None, None))
            }) {
                Ok(count) => total += count,
                Err(err) => tracing::warn!(
                    "[enrichment-batcher] failed to requeue project auth-failed rows after {trigger}: {err}"
                ),
            }
        }

        if total > 0 {
            tracing::info!(
                "[enrichment-batcher] requeued {total} auth-failed enrichment row(s) after {trigger}"
            );
        }
        total
    }

    fn enqueue_retry_candidates(
        &self,
        candidates: Vec<memcore::store::enrichment::EnrichmentRetryCandidate>,
        target_db: DbScope,
        named_project: Option<String>,
        db_path: Option<PathBuf>,
    ) -> usize {
        let mut queued = 0usize;
        let keyword_flag = write_keyword_enrichment_enabled();
        for candidate in candidates {
            let needs_keyword_enrichment = keyword_flag
                && (candidate.failed_stage == "keywords"
                    || candidate.needs_metadata
                    || candidate.keywords.is_empty());
            if !candidate.needs_embedding
                && !candidate.needs_summary
                && !candidate.needs_metadata
                && !needs_keyword_enrichment
            {
                continue;
            }
            let item = EnrichmentItem {
                id: candidate.id,
                text: candidate.text,
                summary: candidate.summary,
                keywords: candidate.keywords,
                entities: candidate.entities,
                needs_embedding: candidate.needs_embedding,
                needs_summary: candidate.needs_summary,
                needs_metadata: candidate.needs_metadata,
                needs_keyword_enrichment,
                target_db,
                named_project: named_project.clone(),
                db_path: db_path.clone(),
                foundry_agent_id: None,
                foundry_path_prefix: None,
                revision: candidate.revision,
            };
            if self.enqueue_enrichment(item) {
                queued += 1;
            }
        }
        queued
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
    pub(crate) async fn flush_enrichment_batch(&self, batch: &mut Vec<EnrichmentItem>) {
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
                let text = external_llm_input(&item.text);
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
                let text = external_llm_input(&item.text);
                async move { (i, llm.extract_metadata(&text).await) }
            })
            .collect();

        let metadata_results: Vec<MetadataExtractionResult> =
            futures::future::join_all(metadata_futures).await;

        let mut keywords_out: Vec<Option<Vec<String>>> = vec![None; items.len()];
        let mut entities_out: Vec<Option<Vec<String>>> = vec![None; items.len()];
        // Operator-visible keyword enrichment status per item (enriched/skipped/failed).
        let mut keyword_status_out: Vec<Option<&'static str>> = vec![None; items.len()];
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

        // 2b. Write-side synonym + bilingual keyword expansion (#921).
        // Gated by TACHI_WRITE_ENRICH_KEYWORDS (default off). Reuses the
        // extract-lane provider path and external_llm_input scrub (#568).
        let keyword_futures: Vec<_> = items
            .iter()
            .enumerate()
            .filter(|(_, item)| item.needs_keyword_enrichment)
            .filter_map(|(i, item)| {
                if item.text.trim().is_empty() {
                    keyword_status_out[i] = Some("skipped");
                    return None;
                }
                let llm = self.llm.clone();
                let text = external_llm_input(&item.text);
                let mut seed = item.keywords.clone();
                if let Some(kws) = keywords_out[i].as_ref() {
                    for kw in kws {
                        if !seed
                            .iter()
                            .any(|existing| existing.eq_ignore_ascii_case(kw))
                        {
                            seed.push(kw.clone());
                        }
                    }
                }
                // Scrub seed keywords with the same external_llm_input path as
                // memory text so credentials in existing_keywords cannot egress
                // into the extract-lane prompt (#943 / #568).
                let seed_for_llm: Vec<String> =
                    seed.iter().map(|kw| external_llm_input(kw)).collect();
                Some(async move {
                    (
                        i,
                        llm.expand_search_keywords(&text, &seed_for_llm).await,
                        seed,
                    )
                })
            })
            .collect();

        type KeywordEnrichmentResult = (usize, Result<Vec<String>, String>, Vec<String>);
        let keyword_results: Vec<KeywordEnrichmentResult> =
            futures::future::join_all(keyword_futures).await;

        for (idx, result, seed) in keyword_results {
            match result {
                Ok(expanded) => {
                    let merged = merge_enriched_keywords(&seed, &expanded);
                    if merged.is_empty() {
                        keyword_status_out[idx] = Some("skipped");
                    } else {
                        keywords_out[idx] = Some(merged);
                        keyword_status_out[idx] = Some("enriched");
                    }
                }
                Err(e) => {
                    tracing::warn!(
                        "[enrichment-batcher] keyword enrichment failed for {}: {e}",
                        items[idx].id
                    );
                    keyword_status_out[idx] = Some("failed");
                    record_enrichment_failure(self, &items[idx], "keywords", &e);
                }
            }
        }

        // Persist skipped status for items that never entered the LLM path.
        for (i, status) in keyword_status_out.iter().enumerate() {
            if *status == Some("skipped") {
                write_keyword_enrichment_status(self, &items[i], "skipped");
            }
        }

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
            let embed_texts: Vec<_> = embed_texts
                .iter()
                .map(|t| crate::memory_search_ops::scrub_secrets(t).0)
                .collect();
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
        //
        // tachi#1435 slice 4 / #2059 codex round 2 (BUG fix): enrichment can
        // add a vector/keywords/summary to an entry that was already
        // searchable when it was first saved — the FTS/keyword content a
        // subsequent search reads changes here, so a stale cached search
        // result must not survive this flush. Tracked once per BATCH (not
        // per item — see `any_field_write` below) and invalidated once after
        // the whole loop, so a fully no-op batch (e.g. every item's keyword
        // stage was `skipped` with no other field write) never pays for a
        // DELETE against a table that could not possibly be stale.
        let mut any_field_write = false;
        for (i, item) in items.iter().enumerate() {
            let new_vec = embed_results[i].as_deref();
            let new_summary = summaries[i].as_deref();
            let new_keywords = keywords_out[i].as_deref();
            let new_entities = entities_out[i].as_deref();
            let keyword_status = keyword_status_out[i];

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
                        // This item's enriched fields (vector/summary/keywords/
                        // entities) actually landed — searchable content
                        // changed, so this batch must invalidate below.
                        any_field_write = true;
                        // Re-apply keyword status after field update: success path
                        // clears failure metadata, so keyword stage outcome must
                        // land after that write (including failed keyword stage
                        // when other stages still succeeded).
                        if let Some(status) = keyword_status {
                            if status != "skipped" {
                                write_keyword_enrichment_status(self, item, status);
                            }
                        }
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
            } else if let Some(status) = keyword_status {
                // Keyword-only outcome with no field writes (failed already
                // recorded; enriched/skipped still need status visibility when
                // expand returned nothing new or pure skip).
                if status == "enriched" || status == "failed" {
                    write_keyword_enrichment_status(self, item, status);
                }
            }
        }

        // Batch-granularity recall-cache bust (not per item — see the
        // comment at this loop's start). Shares the same choke point +
        // epoch bump as `save_memory`'s and `contradiction`'s invalidation.
        if any_field_write {
            crate::memory_search_ops::invalidate_recall_cache_after_write(self, "enrichment_flush");
        }

        tracing::info!("[enrichment-batcher] batch of {batch_size} complete");
    }
}

fn write_keyword_enrichment_status(server: &MemoryServer, item: &EnrichmentItem, status: &str) {
    let action = |store: &mut MemoryStore| {
        store
            .set_keyword_enrichment_status(&item.id, status)
            .map_err(|e| format!("set keyword enrichment status: {e}"))
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
            "[enrichment-batcher] failed to set keywords_status={status} for {}: {err}",
            item.id
        );
    }
}

fn record_enrichment_failure(
    server: &MemoryServer,
    item: &EnrichmentItem,
    stage: &str,
    error: &str,
) {
    if should_defer_enrichment_failure(stage, error) {
        tracing::warn!(
            "[enrichment-batcher] deferring transient enrichment failure for {} at stage={stage}: {error}",
            item.id
        );
        // Transient defer skips durable aggregate failure, but write-side keyword
        // enrichment must still leave an operator-visible keywords_status so the
        // attempt is not silent (#943).
        if stage == "keywords" {
            write_keyword_enrichment_status(server, item, "failed");
        }
        return;
    }

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

fn should_defer_enrichment_failure(stage: &str, error: &str) -> bool {
    if stage == "db_update" {
        return false;
    }

    // Vault lock states (incl. auto-lock, previously missed) are transient and
    // deferrable; classification comes from the shared resolver. NotAuthorized/
    // Missing are intentionally NOT deferred here (auth fails closed; a missing
    // secret will not become present by retrying).
    let lower = error.to_ascii_lowercase();
    matches!(
        crate::vault_ops::classify_vault_read_error(error),
        crate::vault_ops::VaultReadState::Locked | crate::vault_ops::VaultReadState::AutoLocked
    ) || lower.contains("secret materialization failed")
        || lower.contains("temporarily unavailable")
        || lower.contains("retry after")
        || lower.contains("missing api key")
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::EnvRestore;
    use crate::tests::make_server;
    use crate::tool_params::SearchMemoryParams;
    use chrono::Utc;
    use memcore::MemoryEntry;
    use rmcp::handler::server::wrapper::Parameters;
    use serde_json::json;

    #[test]
    fn external_llm_input_scrubs_secret_patterns() {
        let input = "extract metadata for api_key=sk-abcdefghijklmnopqrstuvwxyz123456";
        let output = external_llm_input(input);

        assert!(output.contains("[REDACTED]"));
        assert!(!output.contains("sk-abcdefghijklmnopqrstuvwxyz123456"));
    }

    #[test]
    fn transient_provider_enrichment_errors_are_deferred() {
        assert!(should_defer_enrichment_failure(
            "embedding",
            "Missing API key. Add one to Tachi Vault or set the appropriate env var."
        ));
        assert!(should_defer_enrichment_failure(
            "summary",
            "secret materialization failed: Vault is locked"
        ));
        assert!(should_defer_enrichment_failure(
            "metadata",
            "API key unavailable: all configured provider keys are temporarily unavailable; retry after about 30s"
        ));
    }

    #[test]
    fn durable_enrichment_failures_are_recorded() {
        assert!(!should_defer_enrichment_failure(
            "embedding",
            "Voyage batch auth failure 401 Unauthorized"
        ));
        assert!(!should_defer_enrichment_failure(
            "embedding",
            "API key unavailable for [VOYAGE_API_KEY]: all configured provider keys are unusable (auth_failed: 2)"
        ));
        assert!(!should_defer_enrichment_failure(
            "db_update",
            "retry after lock contention"
        ));
    }

    #[test]
    fn merge_enriched_keywords_dedupes_and_caps() {
        let existing = vec!["Recall".into(), "pipeline".into()];
        let expanded = vec![
            "recall".into(), // case-insensitive dupe
            "检索".into(),
            "synapse".into(),
        ];
        let merged = merge_enriched_keywords(&existing, &expanded);
        assert_eq!(merged, vec!["Recall", "pipeline", "检索", "synapse"]);

        let many: Vec<String> = (0..(MAX_ENRICHED_KEYWORDS + 5))
            .map(|i| format!("kw{i}"))
            .collect();
        let capped = merge_enriched_keywords(&[], &many);
        assert_eq!(capped.len(), MAX_ENRICHED_KEYWORDS);
    }

    #[test]
    fn sanitize_enriched_keyword_strips_controls_bounds_length_rejects_punct() {
        assert_eq!(
            sanitize_enriched_keyword("  hello\u{0001}world  ").as_deref(),
            Some("helloworld")
        );
        let oversized: String = "a".repeat(MAX_KEYWORD_LEN + 20);
        let truncated = sanitize_enriched_keyword(&oversized).expect("keep truncated");
        assert_eq!(truncated.chars().count(), MAX_KEYWORD_LEN);
        assert!(sanitize_enriched_keyword("   ").is_none());
        assert!(sanitize_enriched_keyword("!!!???").is_none());
        assert!(sanitize_enriched_keyword("\u{0007}\u{009F}").is_none());
        assert_eq!(sanitize_enriched_keyword("双语").as_deref(), Some("双语"));
    }

    #[test]
    fn merge_enriched_keywords_sanitizes_hostile_llm_output() {
        let existing = vec!["keep-me".into()];
        let expanded = vec![
            "a".repeat(MAX_KEYWORD_LEN + 10),
            "bad\u{0000}ctrl".into(),
            "!!!".into(),
            "  ".into(),
            "keep-me".into(), // dupe of existing
            "new-ok".into(),
        ];
        let merged = merge_enriched_keywords(&existing, &expanded);
        assert_eq!(
            merged,
            vec![
                "keep-me".to_string(),
                "a".repeat(MAX_KEYWORD_LEN),
                "badctrl".to_string(),
                "new-ok".to_string(),
            ]
        );
        assert!(!merged.iter().any(|k| k.contains('\0')));
        assert!(!merged.iter().any(|k| k == "!!!"));
    }

    #[test]
    fn write_keyword_enrichment_flag_defaults_off() {
        let _lock = crate::utils::global_test_lock()
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let _unset = EnvRestore::remove(WRITE_ENRICH_KEYWORDS_ENV);
        assert!(!write_keyword_enrichment_enabled());
        let _set = EnvRestore::set(WRITE_ENRICH_KEYWORDS_ENV, "true");
        assert!(write_keyword_enrichment_enabled());
    }

    fn seed_entry(id: &str, text: &str, keywords: Vec<String>) -> MemoryEntry {
        MemoryEntry {
            id: id.into(),
            path: "/test/keyword-enrich".into(),
            summary: "pre-seeded summary".into(),
            text: text.into(),
            importance: 0.8,
            timestamp: Utc::now().to_rfc3339(),
            valid_from: String::new(),
            valid_until: None,
            category: "fact".into(),
            topic: "keyword-enrich".into(),
            keywords,
            persons: vec![],
            entities: vec!["tachi".into()],
            location: String::new(),
            source: "test".into(),
            scope: "general".into(),
            archived: false,
            access_count: 0,
            last_access: None,
            revision: 1,
            metadata: json!({}),
            // Present so flush does not attempt Voyage embed.
            vector: Some(vec![0.0_f32; 1024]),
            retention_policy: None,
            domain: None,
            recall_count: 0,
            query_diversity: 0,
            tier: "raw".into(),
        }
    }

    async fn spawn_recording_mock_extract_llm(
        body: serde_json::Value,
    ) -> (
        u16,
        tokio::task::JoinHandle<()>,
        std::sync::Arc<std::sync::Mutex<Vec<serde_json::Value>>>,
    ) {
        use axum::{routing::post, Json, Router};

        let requests = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let app = Router::new().route(
            "/chat/completions",
            post({
                let body = body.clone();
                let requests = std::sync::Arc::clone(&requests);
                move |Json(request): Json<serde_json::Value>| {
                    let body = body.clone();
                    let requests = std::sync::Arc::clone(&requests);
                    async move {
                        requests.lock().expect("record mock request").push(request);
                        Json(serde_json::json!({
                            "choices": [{
                                "message": {
                                    "role": "assistant",
                                    "content": body.to_string()
                                },
                                "finish_reason": "stop"
                            }],
                            "usage": {"prompt_tokens": 1, "completion_tokens": 1, "total_tokens": 2}
                        }))
                    }
                }
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind mock extract llm");
        let port = listener.local_addr().expect("addr").port();
        let handle = tokio::spawn(async move {
            axum::serve(listener, app).await.expect("serve mock llm");
        });
        // Give the server a tick to accept.
        tokio::task::yield_now().await;
        (port, handle, requests)
    }

    async fn spawn_mock_extract_llm(body: serde_json::Value) -> (u16, tokio::task::JoinHandle<()>) {
        let (port, handle, _requests) = spawn_recording_mock_extract_llm(body).await;
        (port, handle)
    }

    fn assert_mock_saw_keyword_enrichment_request(
        requests: &std::sync::Arc<std::sync::Mutex<Vec<serde_json::Value>>>,
    ) {
        let requests = requests.lock().expect("read mock requests");
        assert!(
            requests
                .iter()
                .any(|request| request.to_string().contains("Existing keywords:")),
            "extract mock must receive a keyword-enrichment request; got {requests:?}"
        );
    }

    /// (a) Generated keywords are retrievable through the normal FTS search path.
    #[tokio::test]
    #[allow(clippy::await_holding_lock)]
    async fn write_side_keyword_enrichment_hits_search_via_synonym() {
        let _lock = crate::utils::global_test_lock()
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let _flag = EnvRestore::set(WRITE_ENRICH_KEYWORDS_ENV, "true");
        let _persist = EnvRestore::set("TACHI_TEST_DISABLE_PROVIDER_KEY_HEALTH_PERSIST", "1");

        let (port, mock, requests) = spawn_recording_mock_extract_llm(json!({
            "keywords": ["synapse-recall", "双语检索", "write-side-enrichment"]
        }))
        .await;
        let _base = EnvRestore::set(
            "EXTRACT_BASE_URL",
            &format!("http://127.0.0.1:{port}/chat/completions"),
        );
        let _model = EnvRestore::set("EXTRACT_MODEL", "mock-keyword-model");
        let _key = EnvRestore::set("EXTRACT_API_KEY", "test-key");

        let llm = tachi_llm::LlmClient::new().expect("construct mock extract lane");
        assert_eq!(
            llm.expand_search_keywords("provider/parser probe", &[])
                .await
                .expect("mock keyword response must parse"),
            vec![
                "synapse-recall".to_string(),
                "双语检索".to_string(),
                "write-side-enrichment".to_string(),
            ]
        );

        // Construct server AFTER extract env is pointed at the mock, then
        // inject the probed client so the flush uses the same extract lane.
        let mut server = make_server();
        server.replace_llm(llm);
        let id = format!("kw-search-{}", uuid::Uuid::new_v4());
        // Text deliberately omits the synonym; only enrichment adds it.
        let entry = seed_entry(
            &id,
            "Write-side enrichment widens FTS surface at save time without query-time LLM cost.",
            vec!["fts".into(), "enrichment".into()],
        );
        server
            .with_global_store(|store| store.upsert(&entry).map_err(|e| format!("upsert: {e}")))
            .expect("seed");

        // Precondition: synonym not in text/keywords → FTS miss.
        let before = server
            .with_global_store(|store| {
                memcore::db::search_fts(
                    store.connection(),
                    "synapse-recall",
                    10,
                    false,
                    false,
                    None,
                    None,
                    None,
                )
                .map_err(|e| format!("fts before: {e}"))
            })
            .expect("fts before");
        assert!(
            !before.contains_key(&id),
            "pre-enrichment search must miss synonym; got {before:?}"
        );

        let mut batch = vec![build_enrichment_item(
            &entry,
            false,
            false,
            DbScope::Global,
            None,
            None,
            None,
            None,
            entry.revision,
        )];
        assert!(
            batch[0].needs_keyword_enrichment,
            "flag-on seed must request keyword enrichment"
        );
        server.flush_enrichment_batch(&mut batch).await;

        assert_mock_saw_keyword_enrichment_request(&requests);

        let loaded = server
            .with_global_store(|store| {
                store
                    .get(&id)
                    .map_err(|e| format!("get: {e}"))
                    .map(|e| e.expect("entry exists"))
            })
            .expect("load");
        assert_eq!(
            loaded.metadata["enrichment"]["keywords_status"],
            json!("enriched"),
            "mock response must parse and the keyword stage must succeed: {:?}",
            loaded.metadata
        );
        assert!(
            loaded
                .keywords
                .iter()
                .any(|k| k.eq_ignore_ascii_case("synapse-recall")),
            "keywords must include expanded synonym: {:?}",
            loaded.keywords
        );

        let after = server
            .with_global_store(|store| {
                memcore::db::search_fts(
                    store.connection(),
                    "synapse-recall",
                    10,
                    false,
                    false,
                    None,
                    None,
                    None,
                )
                .map_err(|e| format!("fts after: {e}"))
            })
            .expect("fts after");
        assert!(
            after.contains_key(&id),
            "post-enrichment FTS must hit via generated synonym; got {after:?}"
        );

        mock.abort();
    }

    fn search_params_for(query: &str) -> SearchMemoryParams {
        SearchMemoryParams {
            query: query.to_string(),
            query_vec: None,
            top_k: 10,
            path_prefix: None,
            include_training: false,
            include_archived: false,
            candidates_per_channel: 20,
            mmr_threshold: None,
            graph_expand_hops: 0,
            graph_relation_filter: None,
            weights: None,
            context_symbols: Vec::new(),
            agent_role: None,
            project: None,
            domain: None,
            file_context: None,
            error_context: None,
            enable_rerank: false,
            as_of: None,
            include_metadata: false,
            // Explicit JSON so assertions can index rows by id instead of
            // parsing the markdown digest (tachi#1201 k3 default).
            format: Some("json".to_string()),
        }
    }

    /// tachi#1435 slice 4 / #2059 codex round 2, "tooth 1" (BUG, pre-fix RED):
    /// enrichment flush write-back can add FTS-visible content (a synonym
    /// keyword here) to an entry that was already searchable — a query this
    /// change newly matches must not keep serving a cached answer computed
    /// before the flush. Reuses this file's existing keyword-enrichment mock
    /// harness (see `write_side_keyword_enrichment_hits_search_via_synonym`
    /// above) rather than reinventing it.
    ///
    /// `git stash` this file's `any_field_write` tracking + its
    /// `invalidate_recall_cache_after_write` call at the end of
    /// `flush_enrichment_batch` and rerun this single test to see it fail
    /// pre-fix (the post-flush search still shows only the decoy row).
    #[tokio::test]
    #[allow(clippy::await_holding_lock)]
    async fn enrichment_flush_keyword_write_back_busts_a_warm_recall_cache() {
        let _lock = crate::utils::global_test_lock()
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let _flag = EnvRestore::set(WRITE_ENRICH_KEYWORDS_ENV, "true");
        let _cache_flag = EnvRestore::set("TACHI_ENABLE_RECALL_CACHE", "true");
        let _persist = EnvRestore::set("TACHI_TEST_DISABLE_PROVIDER_KEY_HEALTH_PERSIST", "1");

        let sentinel = format!("EnrichFlushSentinel{}", uuid::Uuid::new_v4().simple());

        let (port, mock, requests) =
            spawn_recording_mock_extract_llm(json!({ "keywords": [sentinel.clone()] })).await;
        let _base = EnvRestore::set(
            "EXTRACT_BASE_URL",
            &format!("http://127.0.0.1:{port}/chat/completions"),
        );
        let _model = EnvRestore::set("EXTRACT_MODEL", "mock-keyword-model");
        let _key = EnvRestore::set("EXTRACT_API_KEY", "test-key");

        let llm = tachi_llm::LlmClient::new().expect("construct mock extract lane");
        assert_eq!(
            llm.expand_search_keywords("provider/parser probe", &[])
                .await
                .expect("mock keyword response must parse"),
            vec![sentinel.clone()]
        );

        // Construct server AFTER extract env is pointed at the mock, then
        // inject the probed client so the flush uses the same extract lane.
        let mut server = make_server();
        server.replace_llm(llm);

        // Decoy: an unrelated, already-searchable memory that literally
        // contains the sentinel token — warms the cache for `sentinel`
        // pre-enrichment with a non-empty (thus cacheable) result.
        let decoy_id = format!("decoy-{}", uuid::Uuid::new_v4());
        let decoy = seed_entry(
            &decoy_id,
            &format!("{sentinel} already appears in this unrelated memory."),
            vec![],
        );
        server
            .with_global_store(|store| {
                store
                    .upsert(&decoy)
                    .map_err(|e| format!("upsert decoy: {e}"))
            })
            .expect("seed decoy");

        // Target: does NOT contain the sentinel yet — the keyword
        // enrichment stage will add it.
        let target_id = format!("target-{}", uuid::Uuid::new_v4());
        let target = seed_entry(
            &target_id,
            "Enrichment flush write-back cache-bust probe body.",
            vec![],
        );
        server
            .with_global_store(|store| {
                store
                    .upsert(&target)
                    .map_err(|e| format!("upsert target: {e}"))
            })
            .expect("seed target");

        // Warm the cache: pre-enrichment, the sentinel query hits only the
        // decoy.
        let first = server
            .search_memory(Parameters(search_params_for(&sentinel)))
            .await
            .expect("warm search");
        let first_rows: serde_json::Value = serde_json::from_str(&first).expect("warm search json");
        let first_rows = first_rows.as_array().expect("warm search rows array");
        assert!(
            first_rows.iter().any(|r| r["id"] == decoy_id),
            "decoy must be visible pre-enrichment: {first_rows:#?}"
        );
        assert!(
            !first_rows.iter().any(|r| r["id"] == target_id),
            "target must NOT match before enrichment: {first_rows:#?}"
        );

        // Flush enrichment on the target: the keyword stage adds `sentinel`.
        let mut batch = vec![build_enrichment_item(
            &target,
            false,
            false,
            DbScope::Global,
            None,
            None,
            None,
            None,
            target.revision,
        )];
        assert!(
            batch[0].needs_keyword_enrichment,
            "flag-on target must request keyword enrichment"
        );
        server.flush_enrichment_batch(&mut batch).await;

        assert_mock_saw_keyword_enrichment_request(&requests);
        let loaded = server
            .with_global_store(|store| {
                store
                    .get(&target_id)
                    .map_err(|e| format!("get enriched target: {e}"))
                    .map(|entry| entry.expect("enriched target exists"))
            })
            .expect("load enriched target");
        assert_eq!(
            loaded.metadata["enrichment"]["keywords_status"],
            json!("enriched"),
            "cache assertion requires keyword stage success: {:?}",
            loaded.metadata
        );
        assert!(
            loaded.keywords.iter().any(|keyword| keyword == &sentinel),
            "cache assertion requires a successful keyword write: {:?}",
            loaded.keywords
        );

        // Post-fix: the SAME query must now surface BOTH rows, not the
        // stale decoy-only answer cached before the flush.
        let second = server
            .search_memory(Parameters(search_params_for(&sentinel)))
            .await
            .expect("post-enrichment search");
        let second_rows: serde_json::Value =
            serde_json::from_str(&second).expect("post search json");
        let second_rows = second_rows.as_array().expect("post search rows array");
        assert!(
            second_rows.iter().any(|r| r["id"] == target_id),
            "the just-enriched target must appear in the very next identical \
             search instead of being masked by the pre-enrichment cached \
             answer: {second_rows:#?}"
        );

        mock.abort();
    }

    /// (b) Enrichment failure leaves the save intact with failed status.
    #[tokio::test]
    #[allow(clippy::await_holding_lock)]
    async fn write_side_keyword_enrichment_failure_leaves_save_intact() {
        let _lock = crate::utils::global_test_lock()
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let _flag = EnvRestore::set(WRITE_ENRICH_KEYWORDS_ENV, "true");
        let _persist = EnvRestore::set("TACHI_TEST_DISABLE_PROVIDER_KEY_HEALTH_PERSIST", "1");

        // Bind a port with no accept loop → connection errors → durable failure.
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind closed provider");
        let port = listener.local_addr().expect("addr").port();
        drop(listener);

        let _base = EnvRestore::set(
            "EXTRACT_BASE_URL",
            &format!("http://127.0.0.1:{port}/chat/completions"),
        );
        let _model = EnvRestore::set("EXTRACT_MODEL", "mock-keyword-model");
        let _key = EnvRestore::set("EXTRACT_API_KEY", "test-key");

        let server = make_server();
        let id = format!("kw-fail-{}", uuid::Uuid::new_v4());
        let original_text =
            "Save must survive keyword enrichment provider failures without data loss.";
        let entry = seed_entry(&id, original_text, vec!["intact".into()]);
        server
            .with_global_store(|store| store.upsert(&entry).map_err(|e| format!("upsert: {e}")))
            .expect("seed");

        let mut batch = vec![build_enrichment_item(
            &entry,
            false,
            false,
            DbScope::Global,
            None,
            None,
            None,
            None,
            entry.revision,
        )];
        server.flush_enrichment_batch(&mut batch).await;

        let loaded = server
            .with_global_store(|store| {
                store
                    .get(&id)
                    .map_err(|e| format!("get: {e}"))
                    .map(|e| e.expect("entry must still exist"))
            })
            .expect("load after failure");
        assert_eq!(
            loaded.text, original_text,
            "save payload must remain intact"
        );
        assert_eq!(loaded.keywords, vec!["intact".to_string()]);
        assert_eq!(
            loaded.metadata["enrichment"]["status"],
            json!("failed"),
            "overall enrichment status must be failed: {:?}",
            loaded.metadata
        );
        assert_eq!(
            loaded.metadata["enrichment"]["failed_stage"],
            json!("keywords")
        );
        assert_eq!(
            loaded.metadata["enrichment"]["keywords_status"],
            json!("failed"),
            "keywords_status must distinguish failed: {:?}",
            loaded.metadata
        );
    }

    /// (c) Flag off = zero behavior change (no keyword expansion, no status field).
    #[tokio::test]
    #[allow(clippy::await_holding_lock)]
    async fn write_side_keyword_enrichment_flag_off_is_noop() {
        let _lock = crate::utils::global_test_lock()
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let _flag = EnvRestore::remove(WRITE_ENRICH_KEYWORDS_ENV);
        let _persist = EnvRestore::set("TACHI_TEST_DISABLE_PROVIDER_KEY_HEALTH_PERSIST", "1");

        // Mock that would inject a synonym if the job ran.
        let (port, mock) = spawn_mock_extract_llm(json!({
            "keywords": ["should-not-appear-when-flag-off"]
        }))
        .await;
        let _base = EnvRestore::set(
            "EXTRACT_BASE_URL",
            &format!("http://127.0.0.1:{port}/chat/completions"),
        );
        let _model = EnvRestore::set("EXTRACT_MODEL", "mock-keyword-model");
        let _key = EnvRestore::set("EXTRACT_API_KEY", "test-key");

        let server = make_server();
        let id = format!("kw-off-{}", uuid::Uuid::new_v4());
        let entry = seed_entry(
            &id,
            "Flag-off path must not expand keywords or write keywords_status.",
            vec!["baseline".into()],
        );
        server
            .with_global_store(|store| store.upsert(&entry).map_err(|e| format!("upsert: {e}")))
            .expect("seed");

        let mut batch = vec![build_enrichment_item(
            &entry,
            false,
            false,
            DbScope::Global,
            None,
            None,
            None,
            None,
            entry.revision,
        )];
        assert!(
            !batch[0].needs_keyword_enrichment,
            "flag-off must not request keyword enrichment"
        );
        server.flush_enrichment_batch(&mut batch).await;

        let loaded = server
            .with_global_store(|store| {
                store
                    .get(&id)
                    .map_err(|e| format!("get: {e}"))
                    .map(|e| e.expect("entry exists"))
            })
            .expect("load");
        assert_eq!(loaded.keywords, vec!["baseline".to_string()]);
        assert!(
            loaded
                .metadata
                .get("enrichment")
                .and_then(|e| e.get("keywords_status"))
                .is_none(),
            "flag-off must not write keywords_status: {:?}",
            loaded.metadata
        );

        let hits = server
            .with_global_store(|store| {
                memcore::db::search_fts(
                    store.connection(),
                    "should-not-appear-when-flag-off",
                    10,
                    false,
                    false,
                    None,
                    None,
                    None,
                )
                .map_err(|e| format!("fts: {e}"))
            })
            .expect("fts");
        assert!(
            !hits.contains_key(&id),
            "flag-off must not index mock synonym"
        );

        mock.abort();
    }

    /// Hostile LLM keyword payload is sanitized at the write boundary before FTS.
    #[tokio::test]
    #[allow(clippy::await_holding_lock)]
    async fn write_side_keyword_enrichment_sanitizes_hostile_llm_keywords() {
        let _lock = crate::utils::global_test_lock()
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let _flag = EnvRestore::set(WRITE_ENRICH_KEYWORDS_ENV, "true");
        let _persist = EnvRestore::set("TACHI_TEST_DISABLE_PROVIDER_KEY_HEALTH_PERSIST", "1");

        let oversized = "x".repeat(MAX_KEYWORD_LEN + 32);
        let (port, mock, requests) = spawn_recording_mock_extract_llm(json!({
            "keywords": [
                oversized,
                "good\u{0001}keyword",
                "!!!",
                "  ",
                "dup",
                "DUP",
                "safe-term"
            ]
        }))
        .await;
        let _base = EnvRestore::set(
            "EXTRACT_BASE_URL",
            &format!("http://127.0.0.1:{port}/chat/completions"),
        );
        let _model = EnvRestore::set("EXTRACT_MODEL", "mock-keyword-model");
        let _key = EnvRestore::set("EXTRACT_API_KEY", "test-key");

        let llm = tachi_llm::LlmClient::new().expect("construct mock extract lane");
        assert_eq!(
            llm.expand_search_keywords("provider/parser probe", &[])
                .await
                .expect("mock keyword response must parse"),
            vec![
                "x".repeat(MAX_KEYWORD_LEN),
                "goodkeyword".to_string(),
                "dup".to_string(),
                "DUP".to_string(),
                "safe-term".to_string(),
            ]
        );

        let mut server = make_server();
        server.replace_llm(llm);
        let id = format!("kw-sanitize-{}", uuid::Uuid::new_v4());
        let entry = seed_entry(
            &id,
            "Sanitization must drop controls, pure punct, and bound length.",
            vec!["dup".into()],
        );
        server
            .with_global_store(|store| store.upsert(&entry).map_err(|e| format!("upsert: {e}")))
            .expect("seed");

        let mut batch = vec![build_enrichment_item(
            &entry,
            false,
            false,
            DbScope::Global,
            None,
            None,
            None,
            None,
            entry.revision,
        )];
        server.flush_enrichment_batch(&mut batch).await;

        assert_mock_saw_keyword_enrichment_request(&requests);

        let loaded = server
            .with_global_store(|store| {
                store
                    .get(&id)
                    .map_err(|e| format!("get: {e}"))
                    .map(|e| e.expect("entry exists"))
            })
            .expect("load");

        assert_eq!(
            loaded.metadata["enrichment"]["keywords_status"],
            json!("enriched"),
            "mock response must parse and the keyword stage must succeed: {:?}",
            loaded.metadata
        );

        assert!(
            loaded
                .keywords
                .iter()
                .all(|k| k.chars().count() <= MAX_KEYWORD_LEN),
            "no keyword may exceed max len: {:?}",
            loaded.keywords
        );
        assert!(
            loaded.keywords.iter().all(|k| !k.chars().any(|c| {
                let u = c as u32;
                u <= 0x1F || (0x7F..=0x9F).contains(&u)
            })),
            "control chars must be stripped: {:?}",
            loaded.keywords
        );
        assert!(
            !loaded.keywords.iter().any(|k| k == "!!!"),
            "pure punctuation must be dropped: {:?}",
            loaded.keywords
        );
        assert!(
            loaded.keywords.iter().any(|k| k == "goodkeyword"),
            "control-stripped token kept: {:?}",
            loaded.keywords
        );
        assert!(
            loaded.keywords.iter().any(|k| k == "safe-term"),
            "safe term kept: {:?}",
            loaded.keywords
        );
        // case-insensitive dedupe of dup/DUP
        assert_eq!(
            loaded
                .keywords
                .iter()
                .filter(|k| k.eq_ignore_ascii_case("dup"))
                .count(),
            1,
            "duplicates collapsed: {:?}",
            loaded.keywords
        );
        assert_eq!(
            loaded.metadata["enrichment"]["keywords_status"],
            json!("enriched")
        );

        mock.abort();
    }

    /// existing_keywords with secret-shaped tokens are scrubbed before LLM egress.
    #[tokio::test]
    #[allow(clippy::await_holding_lock)]
    async fn write_side_keyword_enrichment_scrubs_existing_keywords_in_prompt() {
        let _lock = crate::utils::global_test_lock()
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let _flag = EnvRestore::set(WRITE_ENRICH_KEYWORDS_ENV, "true");
        let _persist = EnvRestore::set("TACHI_TEST_DISABLE_PROVIDER_KEY_HEALTH_PERSIST", "1");

        use axum::{body::Bytes, routing::post, Json, Router};
        use std::sync::{Arc, Mutex};

        let captured: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
        let captured_for_handler = captured.clone();
        let app = Router::new().route(
            "/chat/completions",
            post(move |body: Bytes| {
                let captured = captured_for_handler.clone();
                async move {
                    let text = String::from_utf8_lossy(&body).into_owned();
                    captured.lock().expect("lock").push(text);
                    Json(serde_json::json!({
                        "choices": [{
                            "message": {
                                "role": "assistant",
                                "content": "{\"keywords\":[\"synonym-ok\"]}"
                            },
                            "finish_reason": "stop"
                        }],
                        "usage": {"prompt_tokens": 1, "completion_tokens": 1, "total_tokens": 2}
                    }))
                }
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind mock");
        let port = listener.local_addr().expect("addr").port();
        let mock = tokio::spawn(async move {
            axum::serve(listener, app).await.expect("serve");
        });
        tokio::task::yield_now().await;

        let _base = EnvRestore::set(
            "EXTRACT_BASE_URL",
            &format!("http://127.0.0.1:{port}/chat/completions"),
        );
        let _model = EnvRestore::set("EXTRACT_MODEL", "mock-keyword-model");
        let _key = EnvRestore::set("EXTRACT_API_KEY", "test-key");

        let server = make_server();
        let id = format!("kw-scrub-{}", uuid::Uuid::new_v4());
        let secret = "sk-abcdefghijklmnopqrstuvwxyz123456";
        let entry = seed_entry(
            &id,
            "Memory text without the secret token in body.",
            vec![format!("api_key={secret}"), "safe-seed".into()],
        );
        server
            .with_global_store(|store| store.upsert(&entry).map_err(|e| format!("upsert: {e}")))
            .expect("seed");

        let mut batch = vec![build_enrichment_item(
            &entry,
            false,
            false,
            DbScope::Global,
            None,
            None,
            None,
            None,
            entry.revision,
        )];
        server.flush_enrichment_batch(&mut batch).await;

        let bodies = captured.lock().expect("lock").clone();
        assert!(
            !bodies.is_empty(),
            "mock must have received at least one extract request"
        );
        for body in &bodies {
            assert!(
                !body.contains(secret),
                "secret must not egress in prompt body: {body}"
            );
            assert!(
                body.contains("[REDACTED]") || body.contains("safe-seed"),
                "scrubbed seed or safe keyword expected in body: {body}"
            );
        }

        mock.abort();
    }
}
