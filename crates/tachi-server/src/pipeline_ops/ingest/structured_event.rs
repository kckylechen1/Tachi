use super::*;

pub(super) async fn ingest_structured_event(
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
        if let Err(error) =
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
                ))
        {
            tracing::warn!(
                entry_id = %entry.id,
                error = %error,
                "failed to enqueue enrichment for structured event"
            );
        }
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
