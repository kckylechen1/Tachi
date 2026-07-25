use super::super::audit::{
    claim_retryable_ingest_event, fail_retryable_ingest_event, ingest_audit_key,
    insert_ingest_skip_audit, insert_required_ingest_audit, refresh_retryable_ingest_claim,
};
use super::structured_event::ingest_structured_event;
use super::*;

const INGEST_WORKER: &str = "ingest";

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
    let audit_key = ingest_audit_key(
        "ingest_event",
        target_db,
        params.project.as_deref(),
        &event_hash,
    );

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
        )?;
        return serialize_json(serde_json::json!({
            "status": "skipped",
            "reason": "No content to process"
        }));
    }

    let event_id = format!("{}:{}", params.conversation_id, params.turn_id);
    let claim = claim_retryable_ingest_event(
        server,
        target_db,
        params.project.as_deref(),
        "ingest_event",
        &audit_key,
        INGEST_WORKER,
        &event_hash,
        &event_id,
    )?;
    let Some(claim) = claim else {
        return serialize_json(serde_json::json!({
            "status": "skipped",
            "reason": "Event already processed",
            "hash": event_hash
        }));
    };

    let facts = match server.llm.extract_facts(&combined_text).await {
        Ok(facts) => facts,
        Err(error) => {
            return Err(fail_retryable_ingest_event(
                server,
                target_db,
                params.project.as_deref(),
                "ingest_event",
                &event_hash,
                INGEST_WORKER,
                &claim,
                &audit_key,
                "fact_extraction_failed",
                format!("fact extraction failed for {event_id}: {error}"),
            ));
        }
    };

    let entries = match build_conversation_entries(server, &params, target_db, &event_hash, &facts)
    {
        Ok(entries) => entries,
        Err(error) => {
            return Err(fail_retryable_ingest_event(
                server,
                target_db,
                params.project.as_deref(),
                "ingest_event",
                &event_hash,
                INGEST_WORKER,
                &claim,
                &audit_key,
                "entry_build_failed",
                error,
            ));
        }
    };
    if let Err(error) = refresh_retryable_ingest_claim(
        server,
        target_db,
        params.project.as_deref(),
        INGEST_WORKER,
        &event_hash,
        &claim,
    ) {
        return Err(fail_retryable_ingest_event(
            server,
            target_db,
            params.project.as_deref(),
            "ingest_event",
            &event_hash,
            INGEST_WORKER,
            &claim,
            &audit_key,
            "claim_ownership_lost",
            error,
        ));
    }
    let saved = match persist_conversation_entries(
        server,
        target_db,
        params.project.as_deref(),
        &entries,
    ) {
        Ok(saved) => saved,
        Err(error) => {
            return Err(fail_retryable_ingest_event(
                server,
                target_db,
                params.project.as_deref(),
                "ingest_event",
                &event_hash,
                INGEST_WORKER,
                &claim,
                &audit_key,
                "durable_write_failed",
                error,
            ));
        }
    };

    if let Err(error) = refresh_retryable_ingest_claim(
        server,
        target_db,
        params.project.as_deref(),
        INGEST_WORKER,
        &event_hash,
        &claim,
    ) {
        return Err(fail_retryable_ingest_event(
            server,
            target_db,
            params.project.as_deref(),
            "ingest_event",
            &event_hash,
            INGEST_WORKER,
            &claim,
            &audit_key,
            "claim_ownership_lost",
            error,
        ));
    }
    if let Err(error) = insert_required_ingest_audit(server, "ingest_event", &audit_key, true, None)
    {
        return Err(fail_retryable_ingest_event(
            server,
            target_db,
            params.project.as_deref(),
            "ingest_event",
            &event_hash,
            INGEST_WORKER,
            &claim,
            &audit_key,
            "success_audit_failed",
            format!("ingest writes completed but success audit failed: {error}"),
        ));
    }

    eprintln!(
        "[ingest_event] saved {saved}/{} facts for {event_id}",
        facts.len()
    );
    serialize_json(serde_json::json!({
        "status": "completed",
        "hash": event_hash,
    }))
}

fn build_conversation_entries(
    server: &MemoryServer,
    params: &IngestEventParams,
    target_db: DbScope,
    event_hash: &str,
    facts: &[serde_json::Value],
) -> Result<Vec<MemoryEntry>, String> {
    let domain = resolve_domain(params.domain.clone());
    let mut entries = Vec::new();

    for fact in facts {
        let metadata = crate::provenance::inject_provenance(
            server,
            merge_optional_metadata(params.metadata.clone()),
            "ingest_event",
            "conversation_ingest",
            Some(params.scope.as_str()),
            target_db,
            serde_json::json!({
                "conversation_id": params.conversation_id,
                "turn_id": params.turn_id,
                "event_hash": event_hash,
                "domain": domain,
            }),
        );
        let Some(mut entry) = fact_to_entry(
            fact,
            &format!("conversation:{}", params.conversation_id),
            metadata,
        ) else {
            continue;
        };

        entry.id = stable_ingest_entry_id(event_hash, fact)?;
        entry.source = "ingest_event".to_string();
        if is_lazy_source(&entry.source) && entry.importance < 0.5 {
            entry.retention_policy = Some("ephemeral".to_string());
        }
        entry.domain = domain.clone();
        entries.push(entry);
    }

    Ok(entries)
}

pub(super) fn stable_ingest_entry_id(
    event_hash: &str,
    fact: &serde_json::Value,
) -> Result<String, String> {
    let fact_key = serde_json::to_string(fact)
        .map_err(|error| format!("serialize extracted fact for idempotency: {error}"))?;
    Ok(format!("ingest:{event_hash}:{}", stable_hash(&fact_key)))
}

fn persist_conversation_entries(
    server: &MemoryServer,
    target_db: DbScope,
    project: Option<&str>,
    entries: &[MemoryEntry],
) -> Result<usize, String> {
    let write_entries = |store: &mut MemoryStore| {
        for (index, entry) in entries.iter().enumerate() {
            store.insert_if_absent(entry).map_err(|error| {
                format!("durable write failed for ingest fact row {index}: {error}")
            })?;
        }
        Ok(entries.len())
    };

    if let Some(project_name) = project {
        server.with_named_project_store(project_name, write_entries)
    } else {
        server.with_store_for_scope(target_db, write_entries)
    }
}
