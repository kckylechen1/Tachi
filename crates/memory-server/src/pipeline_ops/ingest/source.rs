use super::*;

pub(crate) async fn handle_ingest_source(
    server: &MemoryServer,
    params: IngestSourceParams,
) -> Result<String, String> {
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
        );
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
    let event_hash = stable_hash(&format!("{}:{}:{}", source_label, path_prefix, content,));

    let claimed = claim_ingest_event(
        server,
        target_db,
        named_project.as_deref(),
        "ingest_source",
        &event_hash,
        &path_prefix,
    )?;
    if !claimed {
        return serialize_json(json!({
            "status": "skipped",
            "reason": "Source already processed",
            "hash": event_hash,
        }));
    }

    let chunks = if params.auto_chunk {
        chunk_text(content, params.chunk_size_chars, params.chunk_overlap_chars)
    } else {
        vec![content.to_string()]
    };
    let chunk_total = chunks.len();
    let base_metadata = merge_optional_metadata(params.metadata.clone());
    let mut saved_entries: Vec<MemoryEntry> = Vec::new();

    let persist_result = {
        let action = |store: &mut MemoryStore| {
            for (index, chunk) in chunks.iter().enumerate() {
                let entry_id = uuid::Uuid::new_v4().to_string();
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
                    }
                }
                let entry = build_ingest_entry(
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
                );
                store
                    .upsert(&entry)
                    .map_err(|e| format!("Failed to save ingested chunk: {e}"))?;
                saved_entries.push(entry);
            }
            Ok(())
        };

        if let Some(project_name) = named_project.as_deref() {
            server.with_named_project_store(project_name, action)
        } else {
            server.with_store_for_scope(target_db, action)
        }
    };

    if let Err(error) = persist_result {
        release_ingest_claim(
            server,
            target_db,
            named_project.as_deref(),
            "ingest_source",
            &event_hash,
        );
        return Err(error);
    }

    for entry in &saved_entries {
        if should_enqueue_enrichment(entry) {
            let _ = server.enrichment_lock().enrich_tx.try_send(
                crate::enrichment::build_enrichment_item(
                    entry,
                    true,
                    params.auto_summarize,
                    target_db,
                    named_project.clone(),
                    None,
                    None,
                    None,
                    1,
                ),
            );
        }
    }

    if params.auto_link {
        build_similarity_edges(
            server,
            target_db,
            named_project.as_deref(),
            domain.as_deref(),
            &saved_entries,
        )
        .await;
    }

    insert_ingest_audit(server, "ingest_source", &event_hash);

    let mut response = serde_json::Map::new();
    response.insert("status".into(), json!("completed"));
    response.insert("hash".into(), json!(event_hash));
    response.insert("db".into(), json!(target_db.as_str()));
    response.insert("path_prefix".into(), json!(path_prefix));
    response.insert("chunks_saved".into(), json!(saved_entries.len()));
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
