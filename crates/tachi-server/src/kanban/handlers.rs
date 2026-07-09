use super::classify::enrich_kanban_card_classification;
use super::inbox::card_matches_inbox;
use super::metadata::add_trimmed_str_to_metadata;
use super::normalize::*;
use super::types::{CheckInboxParams, PostCardParams, UpdateCardParams};
use super::*;

pub(crate) async fn handle_post_card(
    server: &MemoryServer,
    params: PostCardParams,
) -> Result<String, String> {
    let from_agent = normalize_agent_id(&params.from_agent);
    let to_agent = normalize_agent_id(&params.to_agent);
    if from_agent.is_empty() {
        return Err("from_agent cannot be empty".to_string());
    }
    if to_agent.is_empty() {
        return Err("to_agent cannot be empty".to_string());
    }

    let title = params.title.trim().to_string();
    let body = params.body.trim().to_string();
    if title.is_empty() {
        return Err("title cannot be empty".to_string());
    }
    if body.is_empty() {
        return Err("body cannot be empty".to_string());
    }

    let priority = normalize_card_priority(&params.priority);
    let card_type = normalize_card_type(&params.card_type);
    let status = "open".to_string();
    let now = Utc::now().to_rfc3339();
    let card_id = uuid::Uuid::new_v4().to_string();

    let mut metadata = json!({
        "from_agent": from_agent,
        "to_agent": to_agent,
        "status": status,
        "priority": priority,
        "card_type": card_type,
        "created_at": now,
    });
    add_trimmed_str_to_metadata(&mut metadata, "thread_id", &params.thread_id);
    add_trimmed_str_to_metadata(&mut metadata, "workspace_id", &params.workspace_id);
    add_trimmed_str_to_metadata(&mut metadata, "project_id", &params.project_id);
    add_trimmed_str_to_metadata(&mut metadata, "conversation_id", &params.conversation_id);
    add_trimmed_str_to_metadata(&mut metadata, "agent_session_id", &params.agent_session_id);
    metadata = crate::provenance::inject_provenance(
        server,
        metadata,
        "post_card",
        "kanban_card",
        Some("global"),
        DbScope::Global,
        json!({
            "from_agent": params.from_agent.clone(),
            "to_agent": params.to_agent.clone(),
            "workspace_id": params.workspace_id.clone(),
            "project_id": params.project_id.clone(),
            "conversation_id": params.conversation_id.clone(),
            "agent_session_id": params.agent_session_id.clone(),
        }),
    );
    // Kanban lives in the global DB but uses per-agent paths under /kanban;
    // opt in to cross-project routing so path validation lets it through.
    if let Some(obj) = metadata.as_object_mut() {
        obj.insert(
            "allow_cross_project".to_string(),
            serde_json::Value::Bool(true),
        );
    }

    let entry = MemoryEntry {
        id: card_id.clone(),
        path: format!(
            "/kanban/{}/{}",
            normalize_agent_id(&params.from_agent),
            normalize_agent_id(&params.to_agent)
        ),
        summary: title.clone(),
        text: body.clone(),
        importance: kanban_priority_importance(metadata["priority"].as_str().unwrap_or("medium")),
        timestamp: now,
        valid_from: String::new(),
        valid_until: None,
        category: KANBAN_CATEGORY.to_string(),
        topic: String::new(),
        keywords: vec![],
        persons: vec![],
        entities: vec![
            normalize_agent_id(&params.from_agent),
            normalize_agent_id(&params.to_agent),
        ],
        location: String::new(),
        source: "kanban".to_string(),
        scope: "global".to_string(),
        archived: false,
        access_count: 0,
        last_access: None,
        revision: 1,
        vector: None,
        metadata: metadata.clone(),
        retention_policy: Some(memcore::RetentionPolicy::Pinned.as_str().to_string()),
        domain: None,
        recall_count: 0,
        query_diversity: 0,
        tier: "raw".to_string(),
    };

    server.with_global_store(|store| {
        store
            .upsert(&entry)
            .map_err(|e| format!("failed to save kanban card: {e}"))
    })?;

    let classify_enabled = parse_env_bool("KANBAN_CLASSIFY_ENABLED").unwrap_or(false);
    if classify_enabled {
        let db_path = std::sync::Arc::new(server.global_db_path_buf());
        let card_id_clone = card_id.clone();
        let body_clone = body;
        let title_clone = title;
        let source_clone = entry.source.clone();
        let metadata_clone = metadata.clone();
        let expected_revision = entry.revision;
        tokio::spawn(async move {
            if let Err(e) = enrich_kanban_card_classification(
                db_path,
                card_id_clone,
                body_clone,
                title_clone,
                source_clone,
                metadata_clone,
                expected_revision,
            )
            .await
            {
                eprintln!("[kanban] classification skipped for card: {e}");
            }
        });
    }

    let mut resp = serde_json::Map::new();
    resp.insert("status".into(), json!("posted"));
    resp.insert("card_id".into(), json!(card_id));
    resp.insert("db".into(), json!("global"));
    resp.insert("classification_enqueued".into(), json!(classify_enabled));
    serde_json::to_string(&serde_json::Value::Object(resp)).map_err(|e| format!("serialize: {e}"))
}

pub(crate) async fn handle_check_inbox(
    server: &MemoryServer,
    params: CheckInboxParams,
) -> Result<String, String> {
    let agent_id = normalize_agent_id(&params.agent_id);
    if agent_id.is_empty() {
        return Err("agent_id cannot be empty".to_string());
    }
    let limit = params.limit.clamp(1, 1000);

    let mut cards: Vec<MemoryEntry> = server
        .with_global_store_read(|store| {
            store
                .list_by_path(KANBAN_PATH_PREFIX, limit * 4, false)
                .map_err(|e| format!("list kanban cards failed: {e}"))
        })?
        .into_iter()
        .filter(|entry| card_matches_inbox(entry, &params, &agent_id))
        .collect();

    cards.sort_by(|a, b| {
        let pa = kanban_priority_rank(card_priority(a).as_str());
        let pb = kanban_priority_rank(card_priority(b).as_str());
        pa.cmp(&pb).then_with(|| b.timestamp.cmp(&a.timestamp))
    });
    cards.truncate(limit);

    let payload: Vec<serde_json::Value> = cards
        .into_iter()
        .map(|entry| {
            json!({
                "id": entry.id,
                "db": "global",
                "from_agent": card_from_agent(&entry),
                "to_agent": card_to_agent(&entry),
                "status": card_status(&entry).unwrap_or_else(|| "open".to_string()),
                "priority": card_priority(&entry),
                "card_type": card_type(&entry),
                "thread_id": card_metadata_str(&entry, "thread_id"),
                "workspace_id": card_metadata_str(&entry, "workspace_id"),
                "project_id": card_metadata_str(&entry, "project_id"),
                "conversation_id": card_metadata_str(&entry, "conversation_id"),
                "agent_session_id": card_metadata_str(&entry, "agent_session_id"),
                "title": entry.summary,
                "body": entry.text,
                "path": entry.path,
                "timestamp": entry.timestamp,
                "topic": entry.topic,
                "keywords": entry.keywords,
                "metadata": entry.metadata,
            })
        })
        .collect();

    serde_json::to_string(&json!({
        "agent_id": agent_id,
        "count": payload.len(),
        "cards": payload,
    }))
    .map_err(|e| format!("serialize: {e}"))
}

pub(crate) async fn handle_update_card(
    server: &MemoryServer,
    params: UpdateCardParams,
) -> Result<String, String> {
    let new_status = normalize_card_status(&params.new_status).ok_or_else(|| {
        "new_status must be one of open|acknowledged|resolved|expired".to_string()
    })?;

    let mut entry = server
        .with_global_store_read(|store| {
            store
                .get(&params.card_id)
                .map_err(|e| format!("get card failed: {e}"))
        })?
        .ok_or_else(|| format!("kanban card '{}' not found in global db", params.card_id))?;
    if entry.category != KANBAN_CATEGORY {
        return Err(format!(
            "memory '{}' is not a kanban card (category={})",
            entry.id, entry.category
        ));
    }

    let mut metadata = entry.metadata.clone();
    if !metadata.is_object() {
        metadata = json!({});
    }
    metadata["status"] = json!(new_status);
    metadata["updated_at"] = json!(Utc::now().to_rfc3339());

    if let Some(response_text) = params
        .response_text
        .as_ref()
        .map(|v| v.trim())
        .filter(|v| !v.is_empty())
    {
        let mut replies = metadata
            .get("replies")
            .and_then(|v| v.as_array())
            .cloned()
            .unwrap_or_default();
        replies.push(json!({
            "timestamp": Utc::now().to_rfc3339(),
            "text": response_text,
        }));
        metadata["replies"] = json!(replies);
        entry.text = format!(
            "{}\n\n[{}] {}",
            entry.text,
            Utc::now().to_rfc3339(),
            response_text
        );
    }

    let updated = server.with_global_store(|store| {
        store
            .update_with_revision(
                &entry.id,
                &entry.text,
                &entry.summary,
                &entry.source,
                &metadata,
                None,
                entry.revision,
            )
            .map_err(|e| format!("update card failed: {e}"))
    })?;

    if !updated {
        return Err(format!(
            "kanban card '{}' update rejected due to revision mismatch",
            entry.id
        ));
    }

    serde_json::to_string(&json!({
        "updated": true,
        "db": "global",
        "card_id": entry.id,
        "status": metadata["status"],
        "revision": entry.revision + 1,
    }))
    .map_err(|e| format!("serialize: {e}"))
}
