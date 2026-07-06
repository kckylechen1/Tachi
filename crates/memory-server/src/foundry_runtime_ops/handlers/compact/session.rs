use super::super::super::capture::{persist_capture_entry, queue_capture_enrichment};
use super::super::super::helpers::{
    build_entry_path, build_foundry_session_memory_root, build_section_artifact,
    build_stable_foundry_memory_id, dedup_strings, normalize_scope, SectionArtifactInput,
};
use super::super::super::maintenance::enqueue_capture_maintenance_jobs;
use super::super::target::resolve_capture_target;
use super::edges::persist_compact_session_import_edges;
use crate::server_state::MemoryServer;
use crate::tool_params::CompactSessionMemoryParams;
use crate::utils::sanitize_safe_path_name;
use chrono::Utc;
use memory_core::MemoryEntry;
use serde_json::{json, Value};

pub(crate) async fn handle_compact_session_memory(
    server: &MemoryServer,
    params: CompactSessionMemoryParams,
) -> Result<String, String> {
    let compacted_text = params.compacted_text.trim().to_string();
    let durable_signals = dedup_strings(params.durable_signals);
    let salient_topics = dedup_strings(params.salient_topics);
    if compacted_text.is_empty() && durable_signals.is_empty() {
        return serde_json::to_string(&json!({
            "status": "skipped",
            "reason": "empty_compact_artifact",
            "captured": 0,
        }))
        .map_err(|e| format!("Failed to serialize compact_session_memory response: {e}"));
    }

    let requested_scope = normalize_scope(&params.scope, "project");
    let (target_db, named_project, db_path, warning) = resolve_capture_target(
        server,
        &requested_scope,
        params.project.as_deref(),
        &params.agent_id,
    );
    let base_path = params
        .path_prefix
        .clone()
        .unwrap_or_else(|| build_foundry_session_memory_root(&params.agent_id));
    let source_ref_id = format!("{}:{}", params.conversation_id, params.window_id);

    let mut entries = Vec::<MemoryEntry>::new();

    if !compacted_text.is_empty() {
        let summary = compacted_text.chars().take(100).collect::<String>();
        let topic = salient_topics
            .first()
            .cloned()
            .filter(|topic| !topic.trim().is_empty())
            .unwrap_or_else(|| "session_rollup".to_string());
        let metadata = crate::provenance::inject_provenance(
            server,
            json!({
                "source_refs": [{
                    "ref_type": "compact_window",
                    "ref_id": source_ref_id.clone(),
                }],
                "conversation_id": params.conversation_id,
                "window_id": params.window_id,
                "agent_id": params.agent_id,
                "salient_topics": salient_topics.clone(),
                "durable_signals": durable_signals.clone(),
                "artifact_kind": "compact_rollup",
            }),
            "compact_session_memory",
            "session_memory_rollup",
            Some(requested_scope.as_str()),
            target_db,
            json!({
                "conversation_id": params.conversation_id,
                "window_id": params.window_id,
                "agent_id": params.agent_id,
                "path_prefix": base_path,
            }),
        );
        entries.push(MemoryEntry {
            id: build_stable_foundry_memory_id(
                "session_rollup",
                &params.agent_id,
                &params.conversation_id,
                &params.window_id,
                "rollup",
            ),
            path: build_entry_path(&format!("{base_path}/rollups"), &topic),
            summary,
            text: compacted_text.clone(),
            importance: params.importance.clamp(0.0, 1.0),
            timestamp: Utc::now().to_rfc3339(),
            valid_from: String::new(),
            valid_until: None,
            category: "experience".to_string(),
            topic,
            keywords: salient_topics.clone(),
            persons: Vec::new(),
            entities: Vec::new(),
            location: String::new(),
            source: "compact_session_memory".to_string(),
            scope: requested_scope.clone(),
            archived: false,
            access_count: 0,
            last_access: None,
            revision: 1,
            metadata,
            vector: None,
            retention_policy: None,
            domain: None,
            recall_count: 0,
            query_diversity: 0,
            tier: "raw".to_string(),
        });
    }

    for (idx, signal) in durable_signals.iter().enumerate() {
        let signal_text = signal.trim();
        if signal_text.is_empty() {
            continue;
        }
        let topic = {
            let alias = sanitize_safe_path_name(signal_text);
            if alias.is_empty() {
                format!("signal_{}", idx + 1)
            } else {
                alias.chars().take(48).collect()
            }
        };
        let metadata = crate::provenance::inject_provenance(
            server,
            json!({
                "source_refs": [{
                    "ref_type": "compact_window",
                    "ref_id": source_ref_id.clone(),
                }],
                "conversation_id": params.conversation_id,
                "window_id": params.window_id,
                "agent_id": params.agent_id,
                "salient_topics": salient_topics.clone(),
                "artifact_kind": "durable_signal",
                "signal_index": idx,
            }),
            "compact_session_memory",
            "session_memory_signal",
            Some(requested_scope.as_str()),
            target_db,
            json!({
                "conversation_id": params.conversation_id,
                "window_id": params.window_id,
                "agent_id": params.agent_id,
                "path_prefix": base_path,
            }),
        );
        entries.push(MemoryEntry {
            id: build_stable_foundry_memory_id(
                "session_signal",
                &params.agent_id,
                &params.conversation_id,
                &params.window_id,
                &idx.to_string(),
            ),
            path: build_entry_path(&format!("{base_path}/signals"), &topic),
            summary: signal_text.chars().take(100).collect::<String>(),
            text: signal_text.to_string(),
            importance: params.importance.clamp(0.0, 1.0),
            timestamp: Utc::now().to_rfc3339(),
            valid_from: String::new(),
            valid_until: None,
            category: "fact".to_string(),
            topic,
            keywords: salient_topics.clone(),
            persons: Vec::new(),
            entities: Vec::new(),
            location: String::new(),
            source: "compact_session_memory".to_string(),
            scope: requested_scope.clone(),
            archived: false,
            access_count: 0,
            last_access: None,
            revision: 1,
            metadata,
            vector: None,
            retention_policy: None,
            domain: None,
            recall_count: 0,
            query_diversity: 0,
            tier: "raw".to_string(),
        });
    }

    let texts = entries
        .iter()
        .map(|entry| entry.text.clone())
        .collect::<Vec<_>>();
    let texts: Vec<_> = texts
        .iter()
        .map(|t| crate::memory_search_ops::scrub_secrets(t).0)
        .collect();
    let embeddings = match server.llm.embed_voyage_batch(&texts, "document").await {
        Ok(vectors) => Some(vectors),
        Err(err) => {
            tracing::warn!(
                "[compact_session_memory] embedding failed, deferring enrichment: {err}"
            );
            None
        }
    };
    if let Some(vectors) = embeddings.as_ref() {
        for (idx, entry) in entries.iter_mut().enumerate() {
            entry.vector = vectors.get(idx).cloned();
        }
    }

    let mut saved_ids = Vec::new();
    for entry in &entries {
        persist_capture_entry(
            server,
            target_db,
            named_project.as_deref(),
            db_path.as_ref(),
            entry,
        )?;
        if embeddings.is_none() {
            queue_capture_enrichment(
                server,
                target_db,
                named_project.clone(),
                db_path.clone(),
                entry,
                false,
                Some(&params.agent_id),
                Some(&base_path),
            );
        }
        saved_ids.push(entry.id.clone());
    }

    let import_edges = persist_compact_session_import_edges(
        server,
        target_db,
        named_project.as_deref(),
        db_path.as_ref(),
        &entries,
    )?;
    let saved_ids = dedup_strings(saved_ids);
    let maintenance_jobs = if params.queue_maintenance {
        enqueue_capture_maintenance_jobs(
            server,
            target_db,
            named_project.clone(),
            db_path.clone(),
            &params.agent_id,
            &base_path,
            &saved_ids,
            0,
            0,
        )?
    } else {
        Vec::new()
    };
    let section = if !compacted_text.is_empty() {
        Some(build_section_artifact(SectionArtifactInput {
            layer: "session",
            kind: "session_memory",
            title: Some("Durable Session Memory"),
            content: &compacted_text,
            items: &durable_signals,
            cache_boundary: "session",
            source_refs: &[source_ref_id],
            target_tokens: None,
        }))
    } else {
        None
    };

    let mut response = serde_json::Map::new();
    response.insert("status".into(), json!("completed"));
    response.insert("captured".into(), json!(saved_ids.len()));
    response.insert("ids".into(), json!(saved_ids));
    response.insert("db".into(), json!(target_db.as_str()));
    response.insert("path_prefix".into(), json!(base_path));
    response.insert("salient_topics".into(), json!(salient_topics));
    response.insert("durable_signals".into(), json!(durable_signals));
    response.insert("import_edges".into(), json!(import_edges));
    response.insert("maintenance_jobs".into(), json!(maintenance_jobs));
    response.insert("section".into(), json!(section));
    if let Some(warning) = warning {
        response.insert("warning".into(), json!(warning));
    }

    serde_json::to_string(&Value::Object(response))
        .map_err(|e| format!("Failed to serialize compact_session_memory response: {e}"))
}
