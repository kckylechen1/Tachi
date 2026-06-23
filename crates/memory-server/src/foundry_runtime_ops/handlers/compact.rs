use super::super::capture::{persist_capture_entry, queue_capture_enrichment};
use super::super::helpers::{
    build_entry_path, build_foundry_session_memory_root, build_section_artifact,
    build_stable_foundry_memory_id, dedup_strings, estimate_token_count, normalize_scope,
};
use super::super::maintenance::enqueue_capture_maintenance_jobs;
use super::super::recall::run_compaction_model;
use super::target::resolve_capture_target;
use crate::server_state::{DbScope, MemoryServer};
use crate::tool_params::{CompactContextParams, CompactRollupParams, CompactSessionMemoryParams};
use crate::utils::sanitize_safe_path_name;
use chrono::Utc;
use memory_core::{MemoryEntry, MemoryStore};
use serde_json::{json, Value};
use std::collections::HashSet;
use std::path::PathBuf;

pub(crate) async fn handle_compact_rollup(
    server: &MemoryServer,
    params: CompactRollupParams,
) -> Result<String, String> {
    let items = params
        .items
        .iter()
        .filter(|item| !item.compacted_text.trim().is_empty())
        .collect::<Vec<_>>();
    let current_summary = params.current_summary.as_deref().unwrap_or("").trim();

    if items.is_empty() && current_summary.is_empty() {
        return serde_json::to_string(&json!({
            "status": "skipped",
            "reason": "empty_rollup",
            "rollup_id": params.rollup_id,
            "compacted_text": "",
            "estimated_tokens": 0,
            "salient_topics": [],
            "durable_signals": [],
        }))
        .map_err(|e| format!("Failed to serialize compact_rollup response: {e}"));
    }

    let payload = json!({
        "agent_id": params.agent_id,
        "conversation_id": params.conversation_id,
        "rollup_id": params.rollup_id,
        "target_tokens": params.target_tokens.max(32),
        "current_summary": params.current_summary,
        "path_prefix": params.path_prefix,
        "project": params.project,
        "items": items
            .iter()
            .map(|item| json!({
                "item_id": item.item_id,
                "window_id": item.window_id,
                "compacted_text": item.compacted_text,
                "salient_topics": item.salient_topics,
                "durable_signals": item.durable_signals,
            }))
            .collect::<Vec<_>>(),
    });
    let draft = match run_compaction_model(
        server,
        crate::prompts::COMPACT_ROLLUP_PROMPT,
        &payload,
        params.max_output_tokens,
    )
    .await
    {
        Ok(draft) => draft,
        Err(err) => {
            return serde_json::to_string(&json!({
                "status": "failed",
                "reason": "llm_compaction_failed",
                "error": err,
                "agent_id": params.agent_id,
                "conversation_id": params.conversation_id,
                "rollup_id": params.rollup_id,
                "compacted_text": "",
                "estimated_tokens": 0,
                "target_tokens": params.target_tokens.max(32),
                "salient_topics": [],
                "durable_signals": [],
                "source_item_count": items.len(),
                "section": null,
            }))
            .map_err(|e| format!("Failed to serialize compact_rollup response: {e}"));
        }
    };
    let compacted_text = draft.compacted_text.trim().to_string();
    let estimated_tokens = estimate_token_count(&compacted_text);
    let source_refs = items
        .iter()
        .filter_map(|item| item.window_id.clone().or_else(|| item.item_id.clone()))
        .collect::<Vec<_>>();
    let section = if params.build_section && !compacted_text.is_empty() {
        Some(build_section_artifact(
            "session",
            "compact_rollup",
            Some("Session Rollup"),
            &compacted_text,
            &draft.durable_signals,
            "session",
            &source_refs,
            Some(params.target_tokens),
        ))
    } else {
        None
    };

    serde_json::to_string(&json!({
        "status": if compacted_text.is_empty() { "skipped" } else { "completed" },
        "agent_id": params.agent_id,
        "conversation_id": params.conversation_id,
        "rollup_id": params.rollup_id,
        "compacted_text": compacted_text,
        "estimated_tokens": estimated_tokens,
        "target_tokens": params.target_tokens.max(32),
        "salient_topics": dedup_strings(draft.salient_topics),
        "durable_signals": dedup_strings(draft.durable_signals),
        "source_item_count": items.len(),
        "section": section,
    }))
    .map_err(|e| format!("Failed to serialize compact_rollup response: {e}"))
}

fn compact_artifact_kind(entry: &MemoryEntry) -> Option<&str> {
    entry
        .metadata
        .get("artifact_kind")
        .and_then(serde_json::Value::as_str)
}

fn import_signal_relations(signal_text: &str) -> Vec<&'static str> {
    let lower = signal_text.to_ascii_lowercase();
    let mut relations = vec!["distilled_from", "causes"];
    if [
        "fix",
        "fixed",
        "repair",
        "error",
        "failed",
        "failure",
        "bug",
        "panic",
        "exception",
        "修复",
        "错误",
        "失败",
    ]
    .iter()
    .any(|needle| lower.contains(needle))
    {
        relations.push("fixed_by");
    }
    if [
        "reject", "rejected", "avoid", "do not", "don't", "never", "拒绝", "避免", "不要", "禁止",
    ]
    .iter()
    .any(|needle| lower.contains(needle))
    {
        relations.push("rejected_because");
    }
    relations
}

fn build_compact_session_import_edges(
    entries: &[MemoryEntry],
    created_at: &str,
) -> Vec<memory_core::MemoryEdge> {
    let rollups = entries
        .iter()
        .filter(|entry| compact_artifact_kind(entry) == Some("compact_rollup"))
        .collect::<Vec<_>>();
    let signals = entries
        .iter()
        .filter(|entry| compact_artifact_kind(entry) == Some("durable_signal"))
        .collect::<Vec<_>>();
    let mut edges = Vec::new();
    let mut seen = HashSet::new();

    for signal in signals {
        for rollup in &rollups {
            for relation in import_signal_relations(&signal.text) {
                let (source_id, target_id, weight) = match relation {
                    "distilled_from" | "rejected_because" => {
                        (signal.id.clone(), rollup.id.clone(), 0.8)
                    }
                    "fixed_by" => (rollup.id.clone(), signal.id.clone(), 0.85),
                    _ => (rollup.id.clone(), signal.id.clone(), 0.7),
                };
                if seen.insert((source_id.clone(), target_id.clone(), relation.to_string())) {
                    edges.push(memory_core::MemoryEdge {
                        source_id,
                        target_id,
                        relation: relation.to_string(),
                        weight,
                        metadata: json!({
                            "source": "compact_session_memory",
                            "artifact_kind": "durable_signal",
                        }),
                        created_at: created_at.to_string(),
                        valid_from: created_at.to_string(),
                        valid_to: None,
                    });
                }
            }
        }
    }

    edges
}

fn persist_compact_session_import_edges(
    server: &MemoryServer,
    target_db: DbScope,
    named_project: Option<&str>,
    db_path: Option<&PathBuf>,
    entries: &[MemoryEntry],
) -> Result<usize, String> {
    let created_at = Utc::now().to_rfc3339();
    let edges = build_compact_session_import_edges(entries, &created_at);
    if edges.is_empty() {
        return Ok(0);
    }
    let save_edges = |store: &mut MemoryStore| {
        for edge in &edges {
            store
                .add_edge(edge)
                .map_err(|e| format!("Failed to save compact_session_memory edge: {e}"))?;
        }
        Ok(edges.len())
    };
    if let Some(project_name) = named_project {
        server.with_named_project_store(project_name, save_edges)
    } else if let Some(db_path) = db_path {
        server.with_path_store(db_path, save_edges)
    } else {
        server.with_store_for_scope(target_db, save_edges)
    }
}

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
        Some(build_section_artifact(
            "session",
            "session_memory",
            Some("Durable Session Memory"),
            &compacted_text,
            &durable_signals,
            "session",
            &[source_ref_id],
            None,
        ))
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

pub(crate) async fn handle_compact_context(
    server: &MemoryServer,
    params: CompactContextParams,
) -> Result<String, String> {
    let combined_text = params
        .messages
        .iter()
        .map(|message| format!("{}: {}", message.role.trim(), message.content.trim()))
        .collect::<Vec<_>>()
        .join("\n");

    if combined_text.trim().is_empty() {
        return serde_json::to_string(&json!({
            "status": "skipped",
            "reason": "empty_messages",
            "compacted_text": "",
            "estimated_tokens": 0,
            "captured_memory_ids": [],
            "queued_job_ids": [],
        }))
        .map_err(|e| format!("Failed to serialize compact_context response: {e}"));
    }

    let payload = json!({
        "agent_id": params.agent_id,
        "conversation_id": params.conversation_id,
        "window_id": params.window_id,
        "trigger": params.trigger,
        "target_tokens": params.target_tokens.max(32),
        "current_summary": params.current_summary,
        "path_prefix": params.path_prefix,
        "project": params.project,
        "messages": params.messages,
    });
    let draft = match run_compaction_model(
        server,
        crate::prompts::COMPACT_CONTEXT_PROMPT,
        &payload,
        params.max_output_tokens,
    )
    .await
    {
        Ok(draft) => draft,
        Err(err) => {
            return serde_json::to_string(&json!({
                "status": "failed",
                "reason": "llm_compaction_failed",
                "error": err,
                "trigger": params.trigger,
                "conversation_id": params.conversation_id,
                "window_id": params.window_id,
                "compacted_text": "",
                "estimated_tokens": 0,
                "target_tokens": params.target_tokens.max(32),
                "salient_topics": [],
                "durable_signals": [],
                "captured_memory_ids": [],
                "queued_job_ids": [],
            }))
            .map_err(|e| format!("Failed to serialize compact_context response: {e}"));
        }
    };
    let compacted_text = draft.compacted_text.trim().to_string();
    let estimated_tokens = estimate_token_count(&compacted_text);

    let status = if compacted_text.is_empty() {
        "skipped"
    } else {
        "completed"
    };

    let mut response = serde_json::Map::new();
    response.insert("status".into(), json!(status));
    response.insert("trigger".into(), json!(params.trigger));
    response.insert("conversation_id".into(), json!(params.conversation_id));
    response.insert("window_id".into(), json!(params.window_id));
    response.insert("compacted_text".into(), json!(compacted_text));
    response.insert("estimated_tokens".into(), json!(estimated_tokens));
    response.insert("target_tokens".into(), json!(params.target_tokens.max(32)));
    response.insert(
        "salient_topics".into(),
        json!(dedup_strings(draft.salient_topics)),
    );
    response.insert(
        "durable_signals".into(),
        json!(dedup_strings(draft.durable_signals)),
    );
    response.insert("captured_memory_ids".into(), json!(Vec::<String>::new()));
    response.insert("queued_job_ids".into(), json!(Vec::<String>::new()));
    if params.persist {
        response.insert("warning".into(), json!("persist_requested_but_deferred"));
    }

    serde_json::to_string(&Value::Object(response))
        .map_err(|e| format!("Failed to serialize compact_context response: {e}"))
}
