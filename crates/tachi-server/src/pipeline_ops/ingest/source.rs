use super::super::audit::{
    claim_retryable_ingest_event, ingest_audit_key, insert_ingest_skip_audit, RetryableIngestLease,
};
use super::*;

const SOURCE_INGEST_WORKER: &str = "ingest_source";

#[cfg(test)]
static FORCE_NEXT_ADMITTED_ENRICHMENT_OWNERSHIP_LOSS: std::sync::OnceLock<
    std::sync::Mutex<Option<std::path::PathBuf>>,
> = std::sync::OnceLock::new();

#[cfg(test)]
pub(crate) fn force_next_admitted_enrichment_ownership_loss_for_test(server: &MemoryServer) {
    *FORCE_NEXT_ADMITTED_ENRICHMENT_OWNERSHIP_LOSS
        .get_or_init(|| std::sync::Mutex::new(None))
        .lock()
        .expect("lock admitted ownership-loss hook") = Some(server.global_db_path_buf());
}

#[cfg(test)]
fn take_admitted_enrichment_ownership_loss_for_test(server: &MemoryServer) -> bool {
    let mut target = FORCE_NEXT_ADMITTED_ENRICHMENT_OWNERSHIP_LOSS
        .get_or_init(|| std::sync::Mutex::new(None))
        .lock()
        .expect("lock admitted ownership-loss hook");
    if target.as_deref() == Some(server.global_db_path_buf().as_path()) {
        *target = None;
        true
    } else {
        false
    }
}

pub(crate) async fn handle_ingest_source(
    server: &MemoryServer,
    params: IngestSourceParams,
) -> Result<String, String> {
    // Transitional Memory `ingest_source` owns no MCP staging admission.
    // #1689 may retire that public action; until then it remains deliberately
    // on the legacy behavior path rather than accepting a free-standing flag.
    handle_ingest_source_with_admission(server, params, None).await
}

pub(in crate::pipeline_ops) async fn handle_admitted_ingest_source(
    server: &MemoryServer,
    request: super::super::auto_ingest::AdmittedIngestRequest,
) -> Result<String, String> {
    let (mut params, job_id, idempotency_key) = request.into_parts();
    let metadata = params.metadata.get_or_insert_with(|| json!({}));
    let metadata = metadata
        .as_object_mut()
        .ok_or_else(|| "admitted ingest metadata must be a JSON object".to_string())?;
    metadata.insert("admitted_job_id".to_string(), json!(job_id));
    metadata.insert(
        "admitted_idempotency_key".to_string(),
        json!(&idempotency_key),
    );
    handle_ingest_source_with_admission(
        server,
        params,
        Some(AdmittedSourceContext {
            job_id,
            idempotency_key,
        }),
    )
    .await
}

struct AdmittedSourceContext {
    job_id: String,
    idempotency_key: String,
}

async fn handle_ingest_source_with_admission(
    server: &MemoryServer,
    params: IngestSourceParams,
    admitted_context: Option<AdmittedSourceContext>,
) -> Result<String, String> {
    let admitted = admitted_context.is_some();
    let content = params.content.trim();
    if content.is_empty() {
        eprintln!(
            "[ingest_source] skipped empty source ingest (source={}, path_prefix={:?})",
            params
                .source
                .as_deref()
                .or(params.source_url.as_deref())
                .unwrap_or("ingest_source"),
            params.path_prefix
        );
        insert_ingest_skip_audit(
            server,
            "ingest_source",
            "empty_source_content",
            params
                .path_prefix
                .as_deref()
                .or(params.source_url.as_deref())
                .unwrap_or("/"),
        )?;
        return serialize_json(json!({
            "status": "skipped",
            "reason": "No source content to ingest"
        }));
    }

    let domain = resolve_domain(params.domain.clone());
    let named_project = params.project.clone();
    let (target_db, warning) = if named_project.is_some() {
        (DbScope::Project, None)
    } else {
        server.resolve_write_scope(&params.scope)
    };
    let path_prefix = params.path_prefix.clone().unwrap_or_else(|| {
        default_source_path_prefix(
            params.source_url.as_deref(),
            params.source.as_deref(),
            domain.as_deref(),
        )
    });
    let source_label = params
        .source
        .clone()
        .or_else(|| params.source_url.clone())
        .unwrap_or_else(|| "ingest_source".to_string());
    let event_hash = admitted_context
        .as_ref()
        .map(|context| context.idempotency_key.clone())
        .unwrap_or_else(|| stable_hash(&format!("{}:{}:{}", source_label, path_prefix, content,)));
    let audit_key = ingest_audit_key(
        "ingest_source",
        target_db,
        named_project.as_deref(),
        &event_hash,
    );

    let claim = claim_retryable_ingest_event(
        server,
        target_db,
        named_project.as_deref(),
        "ingest_source",
        &audit_key,
        SOURCE_INGEST_WORKER,
        &event_hash,
        &path_prefix,
    )?;
    let Some(claim) = claim else {
        if admitted {
            let chunks = if params.auto_chunk {
                chunk_text(content, params.chunk_size_chars, params.chunk_overlap_chars)
            } else {
                vec![content.to_string()]
            };
            let ids = chunks
                .iter()
                .enumerate()
                .map(|(index, chunk)| {
                    format!("ingest-source:{event_hash}:{index}:{}", stable_hash(chunk))
                })
                .collect::<Vec<_>>();
            return serialize_json(json!({
                "status": "replayed",
                "hash": event_hash,
                "db": target_db.as_str(),
                "path_prefix": path_prefix,
                "chunks_saved": ids.len(),
                "ids": ids,
                "enrichments_enqueued": 0,
                "edges_written": 0,
            }));
        }
        return serialize_json(json!({
            "status": "skipped",
            "reason": "Source already processed",
            "hash": event_hash,
        }));
    };
    let lease = RetryableIngestLease::start(
        server,
        target_db,
        named_project.as_deref(),
        SOURCE_INGEST_WORKER,
        &event_hash,
        claim,
    );

    let chunks = if params.auto_chunk {
        chunk_text(content, params.chunk_size_chars, params.chunk_overlap_chars)
    } else {
        vec![content.to_string()]
    };
    let chunk_total = chunks.len();
    let base_metadata = merge_optional_metadata(params.metadata.clone());
    let mut saved_entries: Vec<MemoryEntry> = Vec::new();

    for (index, chunk) in chunks.iter().enumerate() {
        let entry_id = format!("ingest-source:{event_hash}:{index}:{}", stable_hash(chunk));
        let chunk_path = if chunk_total <= 1 {
            path_prefix.clone()
        } else {
            format!("{}/{}", path_prefix, index)
        };
        let mut metadata = crate::provenance::inject_provenance(
            server,
            base_metadata.clone(),
            "ingest_source",
            "source_ingest",
            Some(params.scope.as_str()),
            target_db,
            json!({
                "source": params.source,
                "source_url": params.source_url,
                "path_prefix": path_prefix,
                "chunk_index": index,
                "chunk_total": chunk_total,
                "event_hash": event_hash,
            }),
        );
        // Wiki-pathed ingests opt into cross-project routing so they
        // can land in the global DB when no wiki project is configured
        // (audit B11 — preferred home is the wiki project DB).
        if chunk_path.starts_with("/wiki/") {
            if let Some(obj) = metadata.as_object_mut() {
                obj.insert("allow_cross_project".to_string(), json!(true));
                // #1072 fix-round (#1215 BUG 6): `ingest_source` is a
                // generic write path that can target ANY
                // `path_prefix`, including `/wiki/...`, entirely
                // outside `wiki_layer_metadata`'s lifecycle stamping
                // — a second, independent bypass alongside
                // `wiki_ops::ingest` the cross-vendor review named
                // ("generic source ingest can bypass dual-write for
                // /wiki prefix"). Without an explicit marker,
                // `derive_wiki_lifecycle`'s no-marker-present default
                // (`Active`, kept for pre-#1072 back-compat) would
                // silently promote arbitrary ingested content to
                // reviewed truth. Stamp it honestly: ingested content
                // has no review/approval step here.
                if !obj.contains_key("lifecycle") {
                    obj.insert(
                        "lifecycle".to_string(),
                        json!(WikiLifecycleV1::PendingReview.as_str()),
                    );
                }
                obj.entry("authority")
                    .or_insert_with(|| json!(WikiAuthorityV1::Advisory.as_str()));
                obj.entry("artifact_kind")
                    .or_insert_with(|| json!(WikiArtifactKindV1::Wiki.as_str()));
            }
        }
        saved_entries.push(build_ingest_entry(
            entry_id,
            chunk_path,
            chunk.clone(),
            params.importance.clamp(0.0, 1.0),
            source_label.clone(),
            params.scope.clone(),
            metadata,
            None,
            domain.clone(),
            params.auto_summarize,
        ));
    }

    let mut chunks_written = 0usize;
    for entry in &saved_entries {
        if let Err(error) = lease
            .write_idempotent(|store: &mut MemoryStore| {
                store
                    .insert_if_absent(entry)
                    .map_err(|e| format!("Failed to save ingested chunk: {e}"))
            })
            .await
        {
            let error = lease
                .fail("ingest_source", &audit_key, "durable_write_failed", error)
                .await;
            if admitted {
                return serialize_json(json!({
                    "status": "partial",
                    "failed_stage": "chunk_write",
                    "chunks_saved": chunks_written,
                    "enrichments_enqueued": 0,
                    "edges_written": 0,
                    "ids": saved_entries
                        .iter()
                        .take(chunks_written)
                        .map(|entry| entry.id.clone())
                        .collect::<Vec<_>>(),
                    "error": error,
                }));
            }
            return Err(error);
        }
        chunks_written += 1;
    }

    let mut enrichments_enqueued = 0usize;
    let mut enrichment_pending = false;
    for entry in &saved_entries {
        if should_enqueue_enrichment(entry) || (admitted && params.auto_summarize) {
            #[cfg(test)]
            if admitted && take_admitted_enrichment_ownership_loss_for_test(server) {
                let delete_claim = |store: &mut MemoryStore| {
                    store
                        .connection()
                        .execute(
                            "DELETE FROM processed_events WHERE event_hash = ?1 AND worker = ?2",
                            rusqlite::params![event_hash, SOURCE_INGEST_WORKER],
                        )
                        .map(|_| ())
                        .map_err(|error| {
                            format!("inject admitted enrichment ownership loss: {error}")
                        })
                };
                if let Some(project_name) = named_project.as_deref() {
                    server.with_named_project_store(project_name, delete_claim)?;
                } else {
                    server.with_store_for_scope(target_db, delete_claim)?;
                }
            }
            if let Err(error) = lease.ensure_owned().await {
                let error = lease
                    .fail("ingest_source", &audit_key, "claim_ownership_lost", error)
                    .await;
                if admitted {
                    return serialize_json(json!({
                        "status": "partial",
                        "failed_stage": "enrichment_ownership",
                        "chunks_saved": chunks_written,
                        "enrichments_enqueued": enrichments_enqueued,
                        "edges_written": 0,
                        "ids": saved_entries
                            .iter()
                            .map(|entry| entry.id.clone())
                            .collect::<Vec<_>>(),
                        "error": error,
                    }));
                }
                return Err(error);
            }
            let item = crate::enrichment::build_enrichment_item(
                entry,
                true,
                params.auto_summarize,
                target_db,
                named_project.clone(),
                None,
                None,
                None,
                1,
            );
            if admitted {
                let context = admitted_context
                    .as_ref()
                    .expect("admitted context exists when admitted is true");
                let dispatch = server.prepare_durable_admitted_enrichment(
                    item,
                    &context.job_id,
                    &context.idempotency_key,
                )?;
                let crate::enrichment::DurableEnrichmentDispatch::Pending {
                    item,
                    should_enqueue,
                } = dispatch
                else {
                    enrichments_enqueued += 1;
                    continue;
                };
                let item = *item;
                enrichment_pending = true;
                if should_enqueue && !server.enqueue_enrichment(item.clone()) {
                    server.mark_admitted_enrichment_pending(&item)?;
                    let error = "admitted ingest enrichment enqueue unavailable".to_string();
                    let error = lease
                        .fail(
                            "ingest_source",
                            &audit_key,
                            "enrichment_enqueue_failed",
                            error,
                        )
                        .await;
                    return serialize_json(json!({
                        "status": "partial",
                        "failed_stage": "enrichment_enqueue",
                        "chunks_saved": chunks_written,
                        "enrichments_enqueued": enrichments_enqueued,
                        "edges_written": 0,
                        "ids": saved_entries
                            .iter()
                            .map(|entry| entry.id.clone())
                            .collect::<Vec<_>>(),
                        "error": error,
                    }));
                }
                if should_enqueue {
                    enrichments_enqueued += 1;
                }
            } else {
                let _ = server.enrichment_lock().enrich_tx.try_send(item);
            }
        }
    }

    if admitted && enrichment_pending {
        let recovery_message = "admitted ingest enrichment remains durably pending".to_string();
        let error = lease
            .fail(
                "ingest_source",
                &audit_key,
                "enrichment_pending",
                recovery_message,
            )
            .await;
        return serialize_json(json!({
            "status": "partial",
            "failed_stage": "enrichment_pending",
            "chunks_saved": chunks_written,
            "enrichments_enqueued": enrichments_enqueued,
            "edges_written": 0,
            "ids": saved_entries.iter().map(|entry| entry.id.clone()).collect::<Vec<_>>(),
            "error": error,
        }));
    }

    let mut edges_written = 0usize;
    if params.auto_link {
        if let Err(error) = build_similarity_edges(
            server,
            target_db,
            named_project.as_deref(),
            domain.as_deref(),
            &saved_entries,
            &lease,
            &mut edges_written,
        )
        .await
        {
            let error = lease
                .fail(
                    "ingest_source",
                    &audit_key,
                    "durable_link_write_failed",
                    error,
                )
                .await;
            if admitted {
                return serialize_json(json!({
                    "status": "partial",
                    "failed_stage": "edge_write",
                    "chunks_saved": chunks_written,
                    "enrichments_enqueued": enrichments_enqueued,
                    "edges_written": edges_written,
                    "ids": saved_entries.iter().map(|entry| entry.id.clone()).collect::<Vec<_>>(),
                    "error": error,
                }));
            }
            return Err(error);
        }
    }

    if let Err(error) = lease.complete("ingest_source", &audit_key).await {
        if admitted {
            return serialize_json(json!({
                "status": "partial",
                "failed_stage": "completion_receipt",
                "chunks_saved": chunks_written,
                "enrichments_enqueued": enrichments_enqueued,
                "edges_written": edges_written,
                "ids": saved_entries.iter().map(|entry| entry.id.clone()).collect::<Vec<_>>(),
                "error": error,
            }));
        }
        return Err(error);
    }

    let mut response = serde_json::Map::new();
    response.insert("status".into(), json!("completed"));
    response.insert("hash".into(), json!(event_hash));
    response.insert("db".into(), json!(target_db.as_str()));
    response.insert("path_prefix".into(), json!(path_prefix));
    response.insert("chunks_saved".into(), json!(saved_entries.len()));
    if admitted {
        response.insert("enrichments_enqueued".into(), json!(enrichments_enqueued));
        response.insert("edges_written".into(), json!(edges_written));
    }
    response.insert(
        "ids".into(),
        json!(saved_entries
            .iter()
            .map(|entry| entry.id.clone())
            .collect::<Vec<_>>()),
    );
    if let Some(warning) = warning {
        response.insert("warning".into(), json!(warning));
    }

    serde_json::to_string(&serde_json::Value::Object(response))
        .map_err(|e| format!("Failed to serialize: {e}"))
}
