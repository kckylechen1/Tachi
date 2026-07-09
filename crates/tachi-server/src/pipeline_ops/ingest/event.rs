use super::structured_event::ingest_structured_event;
use super::*;

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
