use super::*;

fn summarize_text(text: &str, max_chars: usize) -> String {
    let trimmed = text.trim();
    if trimmed.chars().count() <= max_chars {
        return trimmed.to_string();
    }
    let mut out: String = trimmed.chars().take(max_chars).collect();
    out.push_str("...");
    out
}

fn payload_to_text(payload: &serde_json::Value) -> String {
    if let Some(text) = payload.as_str() {
        return text.trim().to_string();
    }
    if let Some(text) = payload.get("text").and_then(|v| v.as_str()) {
        return text.trim().to_string();
    }
    serde_json::to_string(payload).unwrap_or_else(|_| "{}".to_string())
}

pub(super) async fn handle_ghost_publish(
    server: &MemoryServer,
    params: GhostPublishParams,
) -> Result<String, String> {
    let msg_id = uuid::Uuid::new_v4().to_string();
    let timestamp = Utc::now().to_rfc3339();
    let payload_json =
        serde_json::to_string(&params.payload).map_err(|e| format!("serialize: {e}"))?;

    let topic_index = server.with_global_store(|store| {
        store
            .ghost_publish_message(
                &msg_id,
                &params.topic,
                &payload_json,
                &params.publisher,
                &timestamp,
            )
            .map_err(|e| format!("ghost publish: {e}"))
    })?;

    serde_json::to_string(&json!({
        "id": msg_id,
        "topic": params.topic,
        "publisher": params.publisher,
        "timestamp": timestamp,
        "global_index": topic_index,
    }))
    .map_err(|e| format!("serialize: {e}"))
}

pub(super) async fn handle_ghost_subscribe(
    server: &MemoryServer,
    params: GhostSubscribeParams,
) -> Result<String, String> {
    let prev_cursors = {
        let mut state = server.pubsub.lock().unwrap_or_else(|e| e.into_inner());
        if state.cursors.len() >= PUBSUB_MAX_CURSORS
            && !state.cursors.contains_key(&params.agent_id)
        {
            let evict_agent = state
                .cursor_recency
                .iter()
                .min_by_key(|(_, seq)| *seq)
                .map(|(agent_id, _)| agent_id.clone())
                .or_else(|| state.cursors.keys().next().cloned());

            if let Some(agent_id) = evict_agent {
                state.cursors.remove(&agent_id);
                state.cursor_recency.remove(&agent_id);
            }
        }

        state
            .cursors
            .get(&params.agent_id)
            .cloned()
            .unwrap_or_default()
    };

    let (new_messages, new_cursors): (Vec<serde_json::Value>, HashMap<String, usize>) = server
        .with_global_store(|store| {
            let mut messages: Vec<serde_json::Value> = Vec::new();
            let mut cursors: HashMap<String, usize> = HashMap::new();

            for topic in &params.topics {
                store
                    .ghost_upsert_subscription(&params.agent_id, topic)
                    .map_err(|e| format!("ghost subscribe upsert: {e}"))?;

                let persisted_cursor = store
                    .ghost_get_cursor(&params.agent_id, topic)
                    .map_err(|e| format!("ghost get cursor: {e}"))?;
                let mem_cursor = prev_cursors.get(topic).copied().unwrap_or(0) as u64;
                let cursor = std::cmp::max(mem_cursor, persisted_cursor);

                let rows = store
                    .ghost_fetch_messages_since(topic, cursor, PUBSUB_RING_MAX)
                    .map_err(|e| format!("ghost fetch: {e}"))?;

                let mut latest = cursor;
                for row in rows {
                    let idx = row
                        .get("topic_index")
                        .and_then(|v| v.as_u64())
                        .unwrap_or(latest);
                    latest = latest.max(idx);
                    messages.push(json!({
                        "id": row.get("id").cloned().unwrap_or_else(|| json!("")),
                        "topic": row.get("topic").cloned().unwrap_or_else(|| json!(topic)),
                        "topic_index": idx,
                        "payload": row.get("payload").cloned().unwrap_or_else(|| json!({})),
                        "publisher": row.get("publisher").cloned().unwrap_or_else(|| json!("")),
                        "timestamp": row.get("timestamp").cloned().unwrap_or_else(|| json!("")),
                    }));
                }

                store
                    .ghost_set_cursor(&params.agent_id, topic, latest)
                    .map_err(|e| format!("ghost set cursor: {e}"))?;
                cursors.insert(topic.clone(), latest as usize);
            }

            Ok((messages, cursors))
        })?;

    let mut state = server.pubsub.lock().unwrap_or_else(|e| e.into_inner());
    state.cursors.insert(params.agent_id.clone(), new_cursors);
    state.cursor_seq = state.cursor_seq.saturating_add(1);
    let recency_seq = state.cursor_seq;
    state
        .cursor_recency
        .insert(params.agent_id.clone(), recency_seq);

    serde_json::to_string(&json!({
        "agent_id": params.agent_id,
        "new_count": new_messages.len(),
        "messages": new_messages,
    }))
    .map_err(|e| format!("serialize: {e}"))
}

pub(super) async fn handle_ghost_topics(server: &MemoryServer) -> Result<String, String> {
    let topics = server.with_global_store(|store| {
        store
            .ghost_list_topics(500)
            .map_err(|e| format!("ghost list topics: {e}"))
    })?;

    serde_json::to_string(&json!({
        "active_topics": topics.len(),
        "topics": topics,
    }))
    .map_err(|e| format!("serialize: {e}"))
}

pub(super) async fn handle_ghost_ack(
    server: &MemoryServer,
    params: GhostAckParams,
) -> Result<String, String> {
    let (previous, acknowledged_index) = server.with_global_store(|store| {
        store
            .ghost_upsert_subscription(&params.agent_id, &params.topic)
            .map_err(|e| format!("ghost ack subscription: {e}"))?;

        let previous = store
            .ghost_get_cursor(&params.agent_id, &params.topic)
            .map_err(|e| format!("ghost ack get cursor: {e}"))?;

        let target = if let Some(ref message_id) = params.message_id {
            let resolved = store
                .ghost_get_message_topic_index(message_id)
                .map_err(|e| format!("ghost ack resolve message: {e}"))?
                .ok_or_else(|| format!("ghost message '{}' not found", message_id))?;
            if resolved.0 != params.topic {
                return Err(format!(
                    "message '{}' belongs to topic '{}', not '{}'",
                    message_id, resolved.0, params.topic
                ));
            }
            resolved.1
        } else if let Some(index) = params.index {
            index
        } else {
            store
                .ghost_get_topic_total(&params.topic)
                .map_err(|e| format!("ghost ack topic total: {e}"))?
        };

        let acknowledged = std::cmp::max(previous, target);
        store
            .ghost_set_cursor(&params.agent_id, &params.topic, acknowledged)
            .map_err(|e| format!("ghost ack set cursor: {e}"))?;
        Ok((previous, acknowledged))
    })?;

    let mut state = server.pubsub.lock().unwrap_or_else(|e| e.into_inner());
    state
        .cursors
        .entry(params.agent_id.clone())
        .or_default()
        .insert(params.topic.clone(), acknowledged_index as usize);
    state.cursor_seq = state.cursor_seq.saturating_add(1);
    let recency_seq = state.cursor_seq;
    state
        .cursor_recency
        .insert(params.agent_id.clone(), recency_seq);

    serde_json::to_string(&json!({
        "agent_id": params.agent_id,
        "topic": params.topic,
        "previous_index": previous,
        "acknowledged_index": acknowledged_index,
    }))
    .map_err(|e| format!("serialize: {e}"))
}

pub(super) async fn handle_ghost_reflect(
    server: &MemoryServer,
    params: GhostReflectParams,
) -> Result<String, String> {
    let reflection_id = uuid::Uuid::new_v4().to_string();
    let timestamp = Utc::now().to_rfc3339();
    let metadata = params.metadata.clone().unwrap_or_else(|| json!({}));
    let metadata_json = serde_json::to_string(&metadata).map_err(|e| format!("serialize: {e}"))?;
    let topic = params.topic.clone();
    let summary = params.summary.trim().to_string();
    if summary.is_empty() {
        return Err("summary must not be empty".to_string());
    }

    let rule_id = server.with_global_store(|store| {
        store
            .ghost_insert_reflection(
                &reflection_id,
                &params.agent_id,
                topic.as_deref(),
                &summary,
                &metadata_json,
                &timestamp,
            )
            .map_err(|e| format!("ghost reflect insert: {e}"))?;

        if params.promote_rule {
            let path = if let Some(ref t) = topic {
                format!("/ghost/reflections/{}", t)
            } else {
                "/ghost/reflections".to_string()
            };
            let derived_id = store
                .save_derived(
                    &summary,
                    &path,
                    &summarize_text(&summary, 120),
                    0.7,
                    "ghost_reflect",
                    "general",
                    &metadata,
                )
                .map_err(|e| format!("ghost reflect promote rule: {e}"))?;
            Ok(Some(derived_id))
        } else {
            Ok(None)
        }
    })?;

    serde_json::to_string(&json!({
        "reflection_id": reflection_id,
        "agent_id": params.agent_id,
        "topic": topic,
        "summary": summary,
        "timestamp": timestamp,
        "promote_rule": params.promote_rule,
        "rule_id": rule_id,
    }))
    .map_err(|e| format!("serialize: {e}"))
}

pub(super) async fn handle_ghost_promote(
    server: &MemoryServer,
    params: GhostPromoteParams,
) -> Result<String, String> {
    let target_path = params
        .path
        .clone()
        .unwrap_or_else(|| "/ghost/messages".to_string());
    let importance = params.importance.unwrap_or(0.7).clamp(0.0, 1.0);

    let (memory_id, topic, timestamp, publisher) = server.with_global_store(|store| {
        let message = store
            .ghost_get_message(&params.message_id)
            .map_err(|e| format!("ghost promote get message: {e}"))?
            .ok_or_else(|| format!("ghost message '{}' not found", params.message_id))?;

        let payload = message.get("payload").cloned().unwrap_or_else(|| json!({}));
        let topic = message
            .get("topic")
            .and_then(|v| v.as_str())
            .unwrap_or("unknown")
            .to_string();
        let publisher = message
            .get("publisher")
            .and_then(|v| v.as_str())
            .unwrap_or("ghost")
            .to_string();
        let timestamp = message
            .get("timestamp")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string())
            .unwrap_or_else(|| Utc::now().to_rfc3339());

        let text = payload_to_text(&payload);
        let memory_id = format!("ghost:{}", params.message_id);
        let mut metadata = crate::provenance::inject_provenance(
            server,
            json!({
                "ghost_message_id": params.message_id.clone(),
                "ghost_topic": topic.clone(),
                "ghost_publisher": publisher.clone(),
                "payload": payload.clone(),
            }),
            "ghost_promote",
            "ghost_message",
            Some("general"),
            DbScope::Global,
            json!({
                "message_id": params.message_id.clone(),
                "topic": topic.clone(),
                "publisher": publisher.clone(),
            }),
        );
        // B9: keep `source` canonical ('ghost') and stash the publisher under
        // metadata.ghost.publisher so we don't fan the source vocabulary out.
        if let Some(map) = metadata.as_object_mut() {
            let ghost_obj = map.entry("ghost".to_string()).or_insert_with(|| json!({}));
            if let Some(g) = ghost_obj.as_object_mut() {
                g.insert(
                    "publisher".to_string(),
                    serde_json::Value::String(publisher.clone()),
                );
            }
        }
        let entry = MemoryEntry {
            id: memory_id.clone(),
            path: target_path.clone(),
            summary: summarize_text(&text, 140),
            text,
            importance,
            timestamp: timestamp.clone(),
            category: "ghost".to_string(),
            topic: topic.clone(),
            keywords: vec!["ghost".to_string(), "promoted".to_string()],
            persons: vec![publisher.clone()],
            entities: Vec::new(),
            location: String::new(),
            source: "ghost".to_string(),
            scope: "general".to_string(),
            archived: false,
            access_count: 0,
            last_access: None,
            revision: 1,
            metadata,
            vector: None,
            retention_policy: None,
            domain: None,
        };

        store
            .upsert(&entry)
            .map_err(|e| format!("ghost promote upsert memory: {e}"))?;
        let marked = store
            .ghost_mark_message_promoted(&params.message_id, Some(importance))
            .map_err(|e| format!("ghost promote mark promoted: {e}"))?;
        if !marked {
            return Err(format!(
                "failed to mark ghost message '{}' promoted",
                params.message_id
            ));
        }
        Ok((memory_id, topic, timestamp, publisher))
    })?;

    serde_json::to_string(&json!({
        "message_id": params.message_id,
        "memory_id": memory_id,
        "path": target_path,
        "importance": importance,
        "topic": topic,
        "publisher": publisher,
        "timestamp": timestamp,
    }))
    .map_err(|e| format!("serialize: {e}"))
}
