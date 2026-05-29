use super::*;

fn merge_optional_metadata(metadata: Option<serde_json::Value>) -> serde_json::Value {
    match metadata {
        Some(serde_json::Value::Object(map)) => serde_json::Value::Object(map),
        Some(other) => json!({ "payload": other }),
        None => json!({}),
    }
}

fn serialize_json(value: serde_json::Value) -> Result<String, String> {
    serde_json::to_string(&value).map_err(|e| format!("Failed to serialize response: {e}"))
}

fn summary_from_text(text: &str) -> String {
    text.chars().take(100).collect()
}

fn default_ingest_chunk_size() -> usize {
    1200
}

fn default_ingest_chunk_overlap() -> usize {
    120
}

fn topic_from_path(path: &str) -> String {
    path.trim_matches('/')
        .rsplit('/')
        .find(|segment| !segment.is_empty())
        .unwrap_or("ingest")
        .replace('-', "_")
}

fn resolve_domain(domain: Option<String>) -> Option<String> {
    domain.filter(|value| !value.trim().is_empty()).or_else(|| {
        std::env::var("TACHI_DOMAIN")
            .ok()
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty())
    })
}

fn is_lazy_source(source: &str) -> bool {
    matches!(source, "extraction" | "auto" | "ingest_event")
}

fn should_enqueue_enrichment(_entry: &MemoryEntry) -> bool {
    true
}

fn days_since(timestamp: &str) -> f64 {
    chrono::DateTime::parse_from_rfc3339(timestamp)
        .map(|dt| (Utc::now() - dt.with_timezone(&Utc)).num_seconds().max(0) as f64 / 86_400.0)
        .unwrap_or(0.0)
}

/// Calculate whether an accessed ephemeral memory should be promoted.
pub(crate) fn calculate_promotion_score(entry: &MemoryEntry, access_days: usize) -> f64 {
    let frequency = (access_days as f64).ln_1p() / 6.0_f64.ln_1p();
    let age_days = days_since(&entry.timestamp);
    let recency = (-0.693 * age_days / 14.0).exp();
    let conceptual = (entry.keywords.len() as f64 / 5.0).min(1.0);

    (frequency * 0.30 + recency * 0.25 + entry.importance * 0.25 + conceptual * 0.20)
        .clamp(0.0, 1.0)
}

fn default_source_path_prefix(
    source_url: Option<&str>,
    source: Option<&str>,
    domain: Option<&str>,
) -> String {
    let domain_part = domain.unwrap_or("general");
    let source_hint = source
        .filter(|value| !value.trim().is_empty())
        .or(source_url)
        .unwrap_or("source");
    format!(
        "/wiki/{}/{}",
        sanitize_safe_path_name(domain_part),
        sanitize_safe_path_name(source_hint)
    )
}

fn default_event_path_prefix(
    domain: Option<&str>,
    event_type: Option<&str>,
    payload: Option<&serde_json::Value>,
    conversation_id: &str,
) -> String {
    if matches!(domain, Some("trading")) {
        if let Some(ticker) = payload
            .and_then(|value| value.get("ticker"))
            .and_then(|value| value.as_str())
            .filter(|value| !value.trim().is_empty())
        {
            return format!("/trading/journal/{}", sanitize_safe_path_name(ticker));
        }
        return "/trading/journal".to_string();
    }

    if !conversation_id.trim().is_empty() {
        return format!(
            "/events/{}",
            sanitize_safe_path_name(conversation_id.trim())
        );
    }

    format!(
        "/events/{}",
        sanitize_safe_path_name(event_type.unwrap_or("event"))
    )
}

fn chunk_text(content: &str, chunk_size_chars: usize, chunk_overlap_chars: usize) -> Vec<String> {
    let chars: Vec<char> = content.chars().collect();
    if chars.is_empty() {
        return Vec::new();
    }

    let chunk_size = chunk_size_chars.max(1);
    let overlap = chunk_overlap_chars.min(chunk_size.saturating_sub(1));
    let step = chunk_size.saturating_sub(overlap).max(1);

    let mut chunks = Vec::new();
    let mut start = 0usize;
    while start < chars.len() {
        let end = (start + chunk_size).min(chars.len());
        let chunk: String = chars[start..end].iter().collect();
        let trimmed = chunk.trim();
        if !trimmed.is_empty() {
            chunks.push(trimmed.to_string());
        }
        if end >= chars.len() {
            break;
        }
        start += step;
    }

    if chunks.is_empty() {
        chunks.push(content.trim().to_string());
    }
    chunks
}

fn build_ingest_entry(
    id: String,
    path: String,
    text: String,
    importance: f64,
    source: String,
    scope: String,
    metadata: serde_json::Value,
    retention_policy: Option<String>,
    domain: Option<String>,
    needs_summary: bool,
) -> MemoryEntry {
    let summary = if needs_summary {
        String::new()
    } else {
        summary_from_text(&text)
    };
    let importance = importance.clamp(0.0, 1.0);
    let retention_policy = if is_lazy_source(&source) && importance < 0.5 {
        Some("ephemeral".to_string())
    } else {
        retention_policy
    };

    MemoryEntry {
        id,
        path: path.clone(),
        summary,
        text,
        importance,
        timestamp: Utc::now().to_rfc3339(),
        valid_from: String::new(),
        valid_until: None,
        category: "fact".to_string(),
        topic: topic_from_path(&path),
        keywords: Vec::new(),
        persons: Vec::new(),
        entities: Vec::new(),
        location: String::new(),
        source,
        scope,
        archived: false,
        access_count: 0,
        last_access: None,
        revision: 1,
        metadata,
        vector: None,
        retention_policy,
        domain,
    }
}

fn enqueue_dead_letter(
    server: &MemoryServer,
    tool_name: &str,
    arguments: Option<serde_json::Map<String, serde_json::Value>>,
    error: String,
) {
    let dl = DeadLetter {
        id: uuid::Uuid::new_v4().to_string(),
        tool_name: tool_name.to_string(),
        arguments,
        error,
        error_category: "internal".to_string(),
        timestamp: Utc::now().to_rfc3339(),
        retry_count: 0,
        max_retries: 3,
        status: "pending".to_string(),
    };
    let mut dlq = server.dead_letters_lock();
    dlq.push_back(dl);
    while dlq.len() > DLQ_MAX_ENTRIES {
        dlq.pop_front();
    }
}

fn insert_ingest_audit(server: &MemoryServer, label: &str, event_hash: &str) {
    let _ = server.with_global_store(|store| {
        store
            .audit_log_insert(
                &Utc::now().to_rfc3339(),
                "ingest",
                label,
                event_hash,
                true,
                0,
                None,
            )
            .map_err(|e| format!("audit insert: {e}"))
    });
}

fn insert_ingest_skip_audit(server: &MemoryServer, label: &str, reason: &str, context: &str) {
    let args_hash = stable_hash(&format!("{label}:{reason}:{context}"));
    let _ = server.with_global_store(|store| {
        store
            .audit_log_insert(
                &Utc::now().to_rfc3339(),
                "ingest",
                label,
                &args_hash,
                false,
                0,
                Some(reason),
            )
            .map_err(|e| format!("audit insert: {e}"))
    });
}

fn claim_ingest_event(
    server: &MemoryServer,
    target_db: DbScope,
    project: Option<&str>,
    worker: &str,
    event_hash: &str,
    event_id: &str,
) -> Result<bool, String> {
    let action = |store: &mut MemoryStore| {
        store
            .try_claim_event(event_hash, event_id, worker)
            .map_err(|e| format!("Failed to claim event: {e}"))
    };

    if let Some(project_name) = project {
        server.with_named_project_store(project_name, action)
    } else {
        server.with_store_for_scope(target_db, action)
    }
}

fn release_ingest_claim(
    server: &MemoryServer,
    target_db: DbScope,
    project: Option<&str>,
    worker: &str,
    event_hash: &str,
) {
    let action = |store: &mut MemoryStore| {
        store
            .release_event_claim(event_hash, worker)
            .map_err(|e| format!("{e}"))
    };

    let _ = if let Some(project_name) = project {
        server.with_named_project_store(project_name, action)
    } else {
        server.with_store_for_scope(target_db, action)
    };
}

async fn build_similarity_edges(
    server: &MemoryServer,
    target_db: DbScope,
    project: Option<&str>,
    domain: Option<&str>,
    saved_entries: &[MemoryEntry],
) {
    for entry in saved_entries {
        let query = entry.text.chars().take(480).collect::<String>();
        if query.trim().is_empty() {
            continue;
        }

        let search_action = |store: &mut MemoryStore| {
            store
                .search(
                    &query,
                    Some(SearchOptions {
                        top_k: 4,
                        domain: domain.map(|value| value.to_string()),
                        record_access: false,
                        ..Default::default()
                    }),
                )
                .map_err(|e| format!("{e}"))
        };

        let search_results = if let Some(project_name) = project {
            server.with_named_project_store_read(project_name, search_action)
        } else {
            server.with_store_for_scope_read(target_db, search_action)
        };

        let Ok(results) = search_results else {
            continue;
        };

        for result in results {
            if result.entry.id == entry.id {
                continue;
            }
            let edge = memory_core::MemoryEdge {
                source_id: entry.id.clone(),
                target_id: result.entry.id.clone(),
                relation: "similar_to".to_string(),
                weight: result.score.final_score.clamp(0.15, 1.0),
                metadata: json!({
                    "auto_ingest": true,
                    "score": result.score.final_score,
                    "path": entry.path,
                }),
                created_at: Utc::now().to_rfc3339(),
                valid_from: String::new(),
                valid_to: None,
            };
            let save_edge =
                |store: &mut MemoryStore| store.add_edge(&edge).map_err(|e| format!("{e}"));
            let _ = if let Some(project_name) = project {
                server.with_named_project_store(project_name, save_edge)
            } else {
                server.with_store_for_scope(target_db, save_edge)
            };
        }
    }
}

pub(super) fn extract_text_from_tool_result(
    result: &rmcp::model::CallToolResult,
) -> Option<String> {
    let texts: Vec<String> = result
        .content
        .iter()
        .filter_map(|item| {
            serde_json::to_value(item).ok().and_then(|value| {
                value
                    .get("text")
                    .and_then(|text| text.as_str())
                    .map(String::from)
            })
        })
        .collect();

    if texts.is_empty() {
        None
    } else {
        Some(texts.join("\n\n"))
    }
}

pub(super) fn schedule_auto_ingest_from_mcp(
    server: &MemoryServer,
    capability_id: &str,
    tool_name: &str,
    definition: &serde_json::Value,
    arguments: Option<&serde_json::Map<String, serde_json::Value>>,
    result: &rmcp::model::CallToolResult,
) {
    let enabled = definition
        .get("auto_ingest")
        .and_then(|value| value.as_bool())
        .unwrap_or(false);
    if !enabled {
        return;
    }

    let Some(content) = extract_text_from_tool_result(result) else {
        return;
    };

    let resolved_server = capability_id.strip_prefix("mcp:").unwrap_or(capability_id);
    let source_url = arguments
        .and_then(|args| args.get("url"))
        .and_then(|value| value.as_str())
        .map(|value| value.to_string())
        .or_else(|| {
            arguments
                .and_then(|args| args.get("source_url"))
                .and_then(|value| value.as_str())
                .map(|value| value.to_string())
        });

    let domain = resolve_domain(
        definition
            .get("ingest_domain")
            .and_then(|value| value.as_str())
            .map(|value| value.to_string()),
    );
    let path_prefix = definition
        .get("ingest_path_prefix")
        .and_then(|value| value.as_str())
        .map(|value| value.to_string())
        .unwrap_or_else(|| {
            format!(
                "/wiki/{}/{}/{}",
                sanitize_safe_path_name(domain.as_deref().unwrap_or("general")),
                sanitize_safe_path_name(resolved_server),
                sanitize_safe_path_name(tool_name),
            )
        });
    let scope = definition
        .get("ingest_scope")
        .and_then(|value| value.as_str())
        .unwrap_or("global")
        .to_string();
    let source = format!("{}:{}", resolved_server, tool_name);
    let metadata = json!({
        "capability_id": capability_id,
        "tool_name": tool_name,
        "arguments": arguments.cloned().unwrap_or_default(),
        "auto_ingest": true,
    });

    let params = IngestSourceParams {
        content,
        source_url,
        source: Some(source),
        path_prefix: Some(path_prefix),
        auto_chunk: true,
        auto_summarize: true,
        auto_link: true,
        importance: 0.7,
        scope,
        project: None,
        domain,
        chunk_size_chars: default_ingest_chunk_size(),
        chunk_overlap_chars: default_ingest_chunk_overlap(),
        metadata: Some(metadata),
    };

    let server = server.clone();
    tokio::spawn(async move {
        if let Err(error) = handle_ingest_source(&server, params).await {
            eprintln!("[auto-ingest] MCP result ingest failed: {error}");
        }
    });
}

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
        params.scope.clone(),
        metadata,
        None,
        domain.clone(),
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
                    named_project.clone(),
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
                if store.upsert(&entry).is_ok() {
                    saved += 1;
                }
            }
            Ok(saved)
        })
        .map_err(|e| format!("DB write failed: {e}"))?;

    serialize_json(serde_json::json!({
        "status": "completed",
        "source": source,
        "facts_extracted": count,
        "facts_saved": saved
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

pub(crate) async fn handle_get_pipeline_status(server: &MemoryServer) -> Result<String, String> {
    let global_stats = server.with_global_store(|store| {
        store
            .stats(false)
            .map_err(|e| format!("Failed to get global stats: {}", e))
    })?;

    let project_stats = if server.has_project_db() {
        Some(server.with_project_store(|store| {
            store
                .stats(false)
                .map_err(|e| format!("Failed to get project stats: {}", e))
        })?)
    } else {
        None
    };

    let mut total_entries = global_stats.total;
    let mut by_scope = global_stats.by_scope;
    let mut by_category = global_stats.by_category;

    if let Some(project_stats) = project_stats {
        total_entries += project_stats.total;
        for (k, v) in project_stats.by_scope {
            *by_scope.entry(k).or_insert(0) += v;
        }
        for (k, v) in project_stats.by_category {
            *by_category.entry(k).or_insert(0) += v;
        }
    }

    let cache_size = server.tool_cache_lock().len();

    let (dlq_total, dlq_pending, dlq_resolved, dlq_abandoned) = {
        let dlq = server.dead_letters_lock();
        let total = dlq.len();
        let pending = dlq.iter().filter(|d| d.status == "pending").count();
        let resolved = dlq.iter().filter(|d| d.status == "resolved").count();
        let abandoned = dlq.iter().filter(|d| d.status == "abandoned").count();
        (total, pending, resolved, abandoned)
    };
    let hits = server.cache_hits.load(std::sync::atomic::Ordering::Relaxed);
    let misses = server
        .cache_misses
        .load(std::sync::atomic::Ordering::Relaxed);
    let foundry = json!({
        "queued": server.foundry_lock().foundry_stats.queued.load(std::sync::atomic::Ordering::Relaxed),
        "running": server.foundry_lock().foundry_stats.running.load(std::sync::atomic::Ordering::Relaxed),
        "completed": server.foundry_lock().foundry_stats.completed.load(std::sync::atomic::Ordering::Relaxed),
        "failed": server.foundry_lock().foundry_stats.failed.load(std::sync::atomic::Ordering::Relaxed),
        "skipped": server.foundry_lock().foundry_stats.skipped.load(std::sync::atomic::Ordering::Relaxed),
    });

    serde_json::to_string(&json!({
        "status": "running",
        "workers": if server.pipeline_enabled { "rust_async" } else { "disabled" },
        "total_entries": total_entries,
        "by_scope": by_scope,
        "by_category": by_category,
        "vec_available": {
            "global": server.global_vec_available,
            "project": server.project_vec_available,
        },
        "pipeline_enabled": server.pipeline_enabled,
        "phantom_tools": {
            "cache_size": cache_size,
            "cache_hits": hits,
            "cache_misses": misses,
            "ttl_seconds": TOOL_CACHE_TTL.as_secs(),
        },
        "dead_letter_queue": {
            "total": dlq_total,
            "pending": dlq_pending,
            "resolved": dlq_resolved,
            "abandoned": dlq_abandoned,
            "max_entries": DLQ_MAX_ENTRIES,
            "ttl_seconds": DLQ_TTL_SECS,
        },
        "foundry": foundry,
    }))
    .map_err(|e| format!("Failed to serialize response: {}", e))
}

pub(crate) async fn handle_sync_memories(
    server: &MemoryServer,
    params: SyncMemoriesParams,
) -> Result<String, String> {
    let path_prefix = params.path_prefix.as_deref().unwrap_or("/");
    let limit = params.limit.min(500);

    let mut all_entries: Vec<(MemoryEntry, DbScope)> = Vec::new();

    let global_entries = server.with_global_store(|store| {
        store
            .list_by_path(path_prefix, limit, false)
            .map_err(|e| format!("Failed to list global memories: {}", e))
    })?;
    all_entries.extend(global_entries.into_iter().map(|e| (e, DbScope::Global)));

    if server.has_project_db() {
        let project_entries = server.with_project_store(|store| {
            store
                .list_by_path(path_prefix, limit, false)
                .map_err(|e| format!("Failed to list project memories: {}", e))
        })?;
        all_entries.extend(project_entries.into_iter().map(|e| (e, DbScope::Project)));
    }

    all_entries.sort_by(|a, b| b.0.timestamp.cmp(&a.0.timestamp));
    all_entries.truncate(limit);

    if all_entries.is_empty() {
        return serde_json::to_string(&json!({
            "agent_id": params.agent_id,
            "new_count": 0,
            "changed_count": 0,
            "entries": [],
        }))
        .map_err(|e| format!("Failed to serialize: {e}"));
    }

    let memory_ids: Vec<String> = all_entries.iter().map(|(e, _)| e.id.clone()).collect();

    let known_revisions = server.with_global_store(|store| {
        store
            .get_agent_known_revisions(&params.agent_id, &memory_ids)
            .map_err(|e| format!("Failed to get known revisions: {}", e))
    })?;

    let known_revisions = if server.has_project_db() {
        let mut project_known = server.with_project_store(|store| {
            store
                .get_agent_known_revisions(&params.agent_id, &memory_ids)
                .map_err(|e| format!("Failed to get project known revisions: {}", e))
        })?;
        for (id, rev) in known_revisions {
            project_known.entry(id).or_insert(rev);
        }
        project_known
    } else {
        known_revisions
    };

    let mut diff_entries: Vec<serde_json::Value> = Vec::new();
    let mut sync_updates: Vec<(String, i64)> = Vec::new();
    let mut new_count = 0u64;
    let mut changed_count = 0u64;

    for (entry, db_scope) in &all_entries {
        let current_rev = entry.revision;
        match known_revisions.get(&entry.id) {
            None => {
                let mut obj = match slim_entry(entry, *db_scope) {
                    serde_json::Value::Object(m) => m,
                    _ => serde_json::Map::new(),
                };
                obj.insert("diff_type".into(), json!("new"));
                diff_entries.push(serde_json::Value::Object(obj));
                sync_updates.push((entry.id.clone(), current_rev));
                new_count += 1;
            }
            Some(&known_rev) if current_rev > known_rev => {
                let mut obj = match slim_entry(entry, *db_scope) {
                    serde_json::Value::Object(m) => m,
                    _ => serde_json::Map::new(),
                };
                obj.insert("diff_type".into(), json!("changed"));
                obj.insert("prev_revision".into(), json!(known_rev));
                diff_entries.push(serde_json::Value::Object(obj));
                sync_updates.push((entry.id.clone(), current_rev));
                changed_count += 1;
            }
            _ => {
                sync_updates.push((entry.id.clone(), current_rev));
            }
        }
    }

    if !sync_updates.is_empty() {
        server
            .with_global_store(|store| {
                store
                    .update_agent_known_state(&params.agent_id, &sync_updates)
                    .map_err(|e| format!("Failed to update agent state: {}", e))
            })
            .map_err(|e| {
                format!(
                    "sync_memories failed to persist agent state for '{}': {}",
                    params.agent_id, e
                )
            })?;
    }

    serde_json::to_string(&json!({
        "agent_id": params.agent_id,
        "new_count": new_count,
        "changed_count": changed_count,
        "entries": diff_entries,
    }))
    .map_err(|e| format!("Failed to serialize: {e}"))
}
