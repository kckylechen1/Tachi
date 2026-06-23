use memory_core::{MemoryEntry, MemoryStore};
use serde_json::json;

use crate::server_state::{DbScope, MemoryServer};
use crate::tool_params::{
    fact_to_entry, ExtractFactsParams, IngestEventParams, IngestParams, IngestSourceParams,
};
use crate::utils::{stable_hash, value_to_template_text};

use super::audit::{
    claim_ingest_event, enqueue_dead_letter, insert_ingest_audit, insert_ingest_skip_audit,
    release_ingest_claim,
};
use super::auto_ingest::build_similarity_edges;
use super::helpers::{
    build_ingest_entry, chunk_text, default_event_path_prefix, default_source_path_prefix,
    is_lazy_source, merge_optional_metadata, resolve_domain, serialize_json,
    should_enqueue_enrichment,
};

async fn ingest_structured_event(
    server: &MemoryServer,
    params: IngestEventParams,
) -> Result<String, String> {
    let domain = resolve_domain(params.domain.clone());
    let named_project = params.project.clone();
    let (target_db, warning) = if named_project.is_some() {
        (DbScope::Project, None)
    } else {
        server.resolve_write_scope(&params.scope)
    };

    let payload = params.content.unwrap_or_else(|| json!({}));
    let event_type = params
        .event_type
        .clone()
        .unwrap_or_else(|| "event".to_string());
    let text = if payload.is_null() {
        params
            .messages
            .iter()
            .map(|message| format!("{}: {}", message.role, message.content))
            .collect::<Vec<_>>()
            .join("\n")
    } else {
        format!(
            "event_type: {}\n{}",
            event_type,
            serde_json::to_string_pretty(&payload).unwrap_or_else(|_| payload.to_string())
        )
    };
    if text.trim().is_empty() {
        eprintln!(
            "[ingest_event] skipped empty structured event (event_type={}, conversation_id={}, turn_id={})",
            event_type, params.conversation_id, params.turn_id
        );
        insert_ingest_skip_audit(
            server,
            "ingest_event",
            "empty_structured_event",
            &format!("{}:{}", params.conversation_id, params.turn_id),
        );
        return serialize_json(json!({
            "status": "skipped",
            "reason": "No structured event content to persist"
        }));
    }

    let event_hash = stable_hash(&format!(
        "structured-event:{}:{}:{}",
        event_type, params.conversation_id, text,
    ));
    let event_id = format!(
        "{}:{}",
        if params.conversation_id.trim().is_empty() {
            "event"
        } else {
            params.conversation_id.as_str()
        },
        if params.turn_id.trim().is_empty() {
            event_hash.as_str()
        } else {
            params.turn_id.as_str()
        }
    );

    let claimed = claim_ingest_event(
        server,
        target_db,
        named_project.as_deref(),
        "ingest_event",
        &event_hash,
        &event_id,
    )?;
    if !claimed {
        return serialize_json(json!({
            "status": "skipped",
            "reason": "Event already processed",
            "hash": event_hash
        }));
    }

    let path_prefix = params.path_prefix.clone().unwrap_or_else(|| {
        default_event_path_prefix(
            domain.as_deref(),
            Some(event_type.as_str()),
            Some(&payload),
            &params.conversation_id,
        )
    });
    let entry_id = uuid::Uuid::new_v4().to_string();
    let metadata = crate::provenance::inject_provenance(
        server,
        merge_optional_metadata(params.metadata.clone()),
        "ingest_event",
        "event_ingest",
        Some(params.scope.as_str()),
        target_db,
        json!({
            "conversation_id": params.conversation_id,
            "turn_id": params.turn_id,
            "event_type": event_type,
            "event_hash": event_hash,
        }),
    );
    let entry = build_ingest_entry(
        entry_id.clone(),
        path_prefix,
        text,
        params.importance.unwrap_or(0.75).clamp(0.0, 1.0),
        "ingest_event".to_string(),
        params.scope,
        metadata,
        None,
        domain,
        true,
    );

    let save_action = |store: &mut MemoryStore| {
        store
            .upsert(&entry)
            .map_err(|e| format!("Failed to save structured event: {e}"))
    };
    let save_result = if let Some(project_name) = named_project.as_deref() {
        server.with_named_project_store(project_name, save_action)
    } else {
        server.with_store_for_scope(target_db, save_action)
    };

    if let Err(error) = save_result {
        release_ingest_claim(
            server,
            target_db,
            named_project.as_deref(),
            "ingest_event",
            &event_hash,
        );
        return Err(error);
    }

    if should_enqueue_enrichment(&entry) {
        let _ =
            server
                .enrichment_lock()
                .enrich_tx
                .try_send(crate::enrichment::build_enrichment_item(
                    &entry,
                    true,
                    true,
                    target_db,
                    named_project,
                    None,
                    None,
                    None,
                    1,
                ));
    }

    insert_ingest_audit(server, "ingest_event", &event_hash);

    let mut response = serde_json::Map::new();
    response.insert("status".into(), json!("completed"));
    response.insert("hash".into(), json!(event_hash));
    response.insert("id".into(), json!(entry_id));
    response.insert("db".into(), json!(target_db.as_str()));
    if let Some(warning) = warning {
        response.insert("warning".into(), json!(warning));
    }

    serde_json::to_string(&serde_json::Value::Object(response))
        .map_err(|e| format!("Failed to serialize: {e}"))
}

pub(crate) async fn handle_extract_facts(
    server: &MemoryServer,
    params: ExtractFactsParams,
) -> Result<String, String> {
    let (target_db, _warning) = server.resolve_write_scope("project");
    let source = params.source.clone();

    let facts = match server.llm.extract_facts(&params.text).await {
        Ok(facts) => facts,
        Err(err) => {
            return serialize_json(serde_json::json!({
                "status": "failed",
                "reason": "llm_extraction_failed",
                "error": err,
                "source": source,
                "facts_extracted": 0,
                "facts_saved": 0
            }));
        }
    };

    if facts.is_empty() {
        return serialize_json(serde_json::json!({
            "status": "completed",
            "source": source,
            "facts_extracted": 0,
            "facts_saved": 0
        }));
    }

    let count = facts.len();
    let mut saved_facts = Vec::new();
    let saved = server
        .with_store_for_scope(target_db, |store| {
            let mut saved = 0;
            for fact in &facts {
                let metadata = crate::provenance::inject_provenance(
                    server,
                    serde_json::json!({"source": source.clone()}),
                    "extract_facts",
                    "fact_extraction",
                    Some("project"),
                    target_db,
                    serde_json::json!({
                        "extract_source": source.clone(),
                    }),
                );
                // Apply capture_gate filters (min-length and noise assessment) via fact_to_entry
                let Some(mut entry) = fact_to_entry(fact, "extraction", metadata) else {
                    continue;
                };
                if is_lazy_source(&entry.source) && entry.importance < 0.5 {
                    entry.retention_policy = Some("ephemeral".to_string());
                }
                let fact_summary = serde_json::json!({
                    "id": entry.id.clone(),
                    "path": entry.path.clone(),
                    "topic": entry.topic.clone(),
                    "summary": entry.summary.clone(),
                    "importance": entry.importance,
                });
                if store.upsert(&entry).is_ok() {
                    saved += 1;
                    saved_facts.push(fact_summary);
                }
            }
            Ok(saved)
        })
        .map_err(|e| format!("DB write failed: {e}"))?;

    serialize_json(serde_json::json!({
        "status": "completed",
        "source": source,
        "facts_extracted": count,
        "facts_saved": saved,
        "facts": saved_facts,
    }))
}

pub(crate) async fn handle_ingest(
    server: &MemoryServer,
    params: IngestParams,
) -> Result<String, String> {
    match params.ingest_type.trim().to_ascii_lowercase().as_str() {
        "event" => {
            handle_ingest_event(
                server,
                IngestEventParams {
                    conversation_id: params.conversation_id.unwrap_or_default(),
                    turn_id: params.turn_id.unwrap_or_default(),
                    event_type: params.event_type,
                    content: params.content,
                    messages: params.messages,
                    path_prefix: params.path_prefix,
                    importance: Some(params.importance),
                    scope: params.scope,
                    project: params.project,
                    domain: params.domain,
                    metadata: params.metadata,
                },
            )
            .await
        }
        "source" => {
            let content = match params.content {
                Some(serde_json::Value::String(text)) => text,
                Some(other) => value_to_template_text(&other),
                None => String::new(),
            };
            handle_ingest_source(
                server,
                IngestSourceParams {
                    content,
                    source_url: params.source_url,
                    source: params.source,
                    path_prefix: params.path_prefix,
                    auto_chunk: params.auto_chunk,
                    auto_summarize: params.auto_summarize,
                    auto_link: params.auto_link,
                    importance: params.importance,
                    scope: params.scope,
                    project: params.project,
                    domain: params.domain,
                    chunk_size_chars: params.chunk_size_chars,
                    chunk_overlap_chars: params.chunk_overlap_chars,
                    metadata: params.metadata,
                },
            )
            .await
        }
        other => Err(format!(
            "Unsupported ingest_type '{}'. Expected 'event' or 'source'.",
            other
        )),
    }
}

pub(crate) async fn handle_ingest_event(
    server: &MemoryServer,
    params: IngestEventParams,
) -> Result<String, String> {
    if params.content.is_some() || params.event_type.is_some() {
        return ingest_structured_event(server, params).await;
    }

    let event_hash = stable_hash(&format!("{}:{}", params.conversation_id, params.turn_id));
    let (target_db, _warning) = if params.project.is_some() {
        (DbScope::Project, None)
    } else {
        server.resolve_write_scope(&params.scope)
    };

    let combined_text: String = params
        .messages
        .iter()
        .map(|m| m.content.as_str())
        .collect::<Vec<&str>>()
        .join("\n");

    if combined_text.trim().is_empty() {
        eprintln!(
            "[ingest_event] skipped empty conversation event (conversation_id={}, turn_id={})",
            params.conversation_id, params.turn_id
        );
        insert_ingest_skip_audit(
            server,
            "ingest_event",
            "empty_event_content",
            &format!("{}:{}", params.conversation_id, params.turn_id),
        );
        return serialize_json(serde_json::json!({
            "status": "skipped",
            "reason": "No content to process"
        }));
    }

    let claimed = claim_ingest_event(
        server,
        target_db,
        params.project.as_deref(),
        "ingest",
        &event_hash,
        &format!("{}:{}", params.conversation_id, params.turn_id),
    )?;
    if !claimed {
        return serialize_json(serde_json::json!({
            "status": "skipped",
            "reason": "Event already processed",
            "hash": event_hash
        }));
    }

    let server = server.clone();
    let conversation_id = params.conversation_id.clone();
    let turn_id = params.turn_id.clone();
    let eh = event_hash.clone();
    let requested_scope = params.scope.clone();
    let named_project = params.project.clone();
    let domain = resolve_domain(params.domain.clone());
    let extra_metadata = params.metadata.clone();
    tokio::spawn(async move {
        match server.llm.extract_facts(&combined_text).await {
            Ok(facts) if facts.is_empty() => {
                eprintln!("[ingest_event] no facts extracted from {conversation_id}:{turn_id}");
            }
            Ok(facts) => {
                let count = facts.len();
                let saved = if let Some(ref project_name) = named_project {
                    server.with_named_project_store(project_name, |store| {
                        let mut saved = 0;
                        for fact in &facts {
                            let metadata = crate::provenance::inject_provenance(
                                &server,
                                merge_optional_metadata(extra_metadata.clone()),
                                "ingest_event",
                                "conversation_ingest",
                                Some(requested_scope.as_str()),
                                target_db,
                                serde_json::json!({
                                    "conversation_id": conversation_id.clone(),
                                    "turn_id": turn_id.clone(),
                                    "event_hash": eh.clone(),
                                    "domain": domain.clone(),
                                }),
                            );
                            // Apply capture_gate filters (min-length and noise assessment) via fact_to_entry
                            let Some(mut entry) = fact_to_entry(
                                fact,
                                &format!("conversation:{conversation_id}"),
                                metadata,
                            ) else {
                                continue;
                            };
                            entry.source = "ingest_event".to_string();
                            if is_lazy_source(&entry.source) && entry.importance < 0.5 {
                                entry.retention_policy = Some("ephemeral".to_string());
                            }
                            entry.domain = domain.clone();
                            if store.upsert(&entry).is_ok() {
                                saved += 1;
                            }
                        }
                        Ok(saved)
                    })
                } else {
                    server.with_store_for_scope(target_db, |store| {
                        let mut saved = 0;
                        for fact in &facts {
                            let metadata = crate::provenance::inject_provenance(
                                &server,
                                merge_optional_metadata(extra_metadata.clone()),
                                "ingest_event",
                                "conversation_ingest",
                                Some(requested_scope.as_str()),
                                target_db,
                                serde_json::json!({
                                    "conversation_id": conversation_id.clone(),
                                    "turn_id": turn_id.clone(),
                                    "event_hash": eh.clone(),
                                    "domain": domain.clone(),
                                }),
                            );
                            // Apply capture_gate filters (min-length and noise assessment) via fact_to_entry
                            let Some(mut entry) = fact_to_entry(
                                fact,
                                &format!("conversation:{conversation_id}"),
                                metadata,
                            ) else {
                                continue;
                            };
                            entry.source = "ingest_event".to_string();
                            if is_lazy_source(&entry.source) && entry.importance < 0.5 {
                                entry.retention_policy = Some("ephemeral".to_string());
                            }
                            entry.domain = domain.clone();
                            if store.upsert(&entry).is_ok() {
                                saved += 1;
                            }
                        }
                        Ok(saved)
                    })
                };
                match saved {
                    Ok(n) => {
                        insert_ingest_audit(&server, "ingest_event", &eh);
                        eprintln!("[ingest_event] saved {n}/{count} facts for {conversation_id}:{turn_id}")
                    }
                    Err(e) => {
                        eprintln!(
                            "[ingest_event] DB write failed: {e} — releasing claim for retry"
                        );
                        release_ingest_claim(
                            &server,
                            target_db,
                            named_project.as_deref(),
                            "ingest",
                            &eh,
                        );
                        enqueue_dead_letter(
                            &server,
                            "ingest_event",
                            Some(serde_json::Map::from_iter([
                                (
                                    "conversation_id".to_string(),
                                    serde_json::json!(conversation_id),
                                ),
                                ("turn_id".to_string(), serde_json::json!(turn_id)),
                            ])),
                            format!("DB write failed: {e}"),
                        );
                    }
                }
            }
            Err(e) => {
                eprintln!("[ingest_event] LLM extraction failed for {conversation_id}:{turn_id}: {e} — releasing claim for retry");
                release_ingest_claim(&server, target_db, named_project.as_deref(), "ingest", &eh);
                enqueue_dead_letter(
                    &server,
                    "ingest_event",
                    Some(serde_json::Map::from_iter([
                        (
                            "conversation_id".to_string(),
                            serde_json::json!(conversation_id),
                        ),
                        ("turn_id".to_string(), serde_json::json!(turn_id)),
                    ])),
                    format!("LLM extraction failed: {e}"),
                );
            }
        }
    });
    serialize_json(serde_json::json!({
        "status": "ingestion queued",
        "hash": event_hash
    }))
}

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
