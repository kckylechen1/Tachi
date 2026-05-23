use super::capture::*;
use super::helpers::*;
use super::maintenance::enqueue_capture_maintenance_jobs;
use super::recall::*;
use super::*;
use regex::Regex;
use std::collections::HashSet;
use std::sync::OnceLock;

pub(crate) async fn handle_section_build(
    _server: &MemoryServer,
    params: SectionBuildParams,
) -> Result<String, String> {
    let content = params.content.unwrap_or_default();
    let items = dedup_strings(params.items);
    if content.trim().is_empty() && items.is_empty() {
        return serde_json::to_string(&json!({
            "status": "skipped",
            "reason": "empty_section",
        }))
        .map_err(|e| format!("Failed to serialize section_build response: {e}"));
    }

    let section = build_section_artifact(
        &params.layer,
        &params.kind,
        params.title.as_deref(),
        &content,
        &items,
        &params.cache_boundary,
        &params.source_refs,
        params.target_tokens,
    );

    serde_json::to_string(&json!({
        "status": "completed",
        "section": section,
    }))
    .map_err(|e| format!("Failed to serialize section_build response: {e}"))
}

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
    let named_project = params.project.clone();
    let (target_db, warning) = if named_project.is_some() {
        (DbScope::Project, None)
    } else {
        server.resolve_write_scope(&requested_scope)
    };
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
        });
    }

    let texts = entries
        .iter()
        .map(|entry| entry.text.clone())
        .collect::<Vec<_>>();
    let embeddings = match server.llm.embed_voyage_batch(&texts, "document").await {
        Ok(vectors) => Some(vectors),
        Err(err) => {
            eprintln!("[compact_session_memory] embedding failed, deferring enrichment: {err}");
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
        persist_capture_entry(server, target_db, named_project.as_deref(), entry)?;
        if embeddings.is_none() {
            queue_capture_enrichment(
                server,
                target_db,
                named_project.clone(),
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
        &entries,
    )?;
    let saved_ids = dedup_strings(saved_ids);
    let maintenance_jobs = if params.queue_maintenance {
        enqueue_capture_maintenance_jobs(
            server,
            target_db,
            named_project.clone(),
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

pub(crate) async fn handle_recall_context(
    server: &MemoryServer,
    params: RecallContextParams,
) -> Result<String, String> {
    if params.query.trim().is_empty() {
        return serde_json::to_string(&json!({
            "status": "skipped",
            "reason": "empty_query",
            "count": 0,
            "results": [],
            "prepend_context": "",
        }))
        .map_err(|e| format!("Failed to serialize recall_context response: {e}"));
    }

    let candidate_multiplier = params.candidate_multiplier.max(1);
    let candidate_top_k = params.top_k.max(1).saturating_mul(candidate_multiplier);
    let recall_scope =
        resolve_recall_scope(params.path_prefix.as_deref(), params.agent_id.as_deref());
    let mut parsed = Vec::<Value>::new();
    let mut seen_ids = HashSet::<String>::new();
    for search_prefix in &recall_scope.search_prefixes {
        let rows = search_memory_rows(
            server,
            SearchMemoryParams {
                query: params.query.clone(),
                query_vec: None,
                top_k: candidate_top_k,
                path_prefix: search_prefix.clone(),
                include_archived: false,
                candidates_per_channel: candidate_top_k.max(20),
                mmr_threshold: None,
                graph_expand_hops: 0,
                graph_relation_filter: None,
                weights: None,
                agent_role: params.agent_role.clone(),
                project: params.project.clone(),
                domain: None,
                file_context: None,
                error_context: None,
                enable_rerank: false,
            },
        )
        .await?;
        for row in rows {
            let id = value_id(&row);
            if !id.is_empty() && !seen_ids.insert(id) {
                continue;
            }
            parsed.push(row);
        }
    }

    let excluded_topics = params
        .exclude_topics
        .iter()
        .map(|topic| topic.trim().to_ascii_lowercase())
        .filter(|topic| !topic.is_empty())
        .collect::<HashSet<_>>();

    let min_score = params.min_score.unwrap_or(0.0);

    let filtered = parsed
        .into_iter()
        .filter(|row| {
            if recall_scope.allowed_prefixes.is_empty() {
                return true;
            }
            let path = value_path(row);
            !path.is_empty()
                && recall_scope
                    .allowed_prefixes
                    .iter()
                    .any(|prefix| path_is_within_prefix(&path, prefix))
        })
        .filter(|row| row.get("l0_rule").and_then(Value::as_bool) != Some(true))
        .filter(|row| {
            let topic = value_topic(row);
            topic.is_empty() || !excluded_topics.contains(&topic.to_ascii_lowercase())
        })
        .filter(|row| value_relevance(row) >= min_score)
        .collect::<Vec<_>>();

    let reranked = rerank_rows(server, &params.query, filtered, params.top_k.max(1)).await;
    let final_rows = reranked
        .into_iter()
        .filter(|row| value_relevance(row) >= min_score)
        .collect::<Vec<_>>();
    let prepend_context = build_prepend_context(&final_rows);

    // ── Wiki auto-search: enrich recall with wiki knowledge ─────────────
    let (wiki_rows, wiki_context) = if params.include_wiki && !params.query.trim().is_empty() {
        let wiki_top_k = params.wiki_top_k.max(1).min(10);
        match search_memory_rows(
            server,
            SearchMemoryParams {
                query: params.query.clone(),
                query_vec: None,
                top_k: wiki_top_k,
                path_prefix: Some("/wiki".to_string()),
                include_archived: false,
                candidates_per_channel: (wiki_top_k * 3).max(20),
                mmr_threshold: Some(0.85),
                graph_expand_hops: 1,
                graph_relation_filter: None,
                weights: None,
                agent_role: None,
                project: Some(params.wiki_project.clone()),
                domain: None,
                file_context: None,
                error_context: None,
                enable_rerank: false,
            },
        )
        .await
        {
            Ok(rows) if !rows.is_empty() => {
                let ctx = build_wiki_context(&rows);
                (rows, ctx)
            }
            Ok(_) => (vec![], String::new()),
            Err(err) => {
                eprintln!("[recall_context] wiki search failed, skipping: {err}");
                (vec![], String::new())
            }
        }
    } else {
        (vec![], String::new())
    };

    // Combine prepend_context blocks
    let combined_context = if wiki_context.is_empty() {
        prepend_context
    } else {
        format!("{}\n{}", prepend_context, wiki_context)
    };

    serde_json::to_string(&json!({
        "status": "completed",
        "count": final_rows.len(),
        "results": final_rows,
        "prepend_context": combined_context,
        "path_prefixes": recall_scope.search_prefixes,
        "allowed_prefixes": recall_scope.allowed_prefixes,
        "warning": recall_scope.warning,
        "wiki_count": wiki_rows.len(),
        "wiki_results": wiki_rows,
    }))
    .map_err(|e| format!("Failed to serialize recall_context response: {e}"))
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct BracketSelfEvolutionNote {
    pub id: String,
    pub text: String,
    pub category: String,
}

fn bracket_note_regex() -> &'static Regex {
    static REGEX: OnceLock<Regex> = OnceLock::new();
    REGEX.get_or_init(|| Regex::new(r"（([^（）\r\n]{4,240})）|\(([^()\r\n]{4,240})\)").unwrap())
}

fn bracket_strategy_regexes() -> &'static [Regex] {
    static REGEXES: OnceLock<Vec<Regex>> = OnceLock::new();
    REGEXES
        .get_or_init(|| {
            [
                r"原来.{0,20}(喜欢|不喜欢)",
                r"记住了",
                r"下次我要|下次我会",
                r"以后我要|以后我会",
                r"雷区",
                r"更吃这一套|不吃这一套",
                r"这样更有效|这种方式有用",
                r"策略失败|无效",
            ]
            .into_iter()
            .map(|pattern| Regex::new(pattern).unwrap())
            .collect()
        })
        .as_slice()
}

fn bracket_decision_regex() -> &'static Regex {
    static REGEX: OnceLock<Regex> = OnceLock::new();
    REGEX.get_or_init(|| Regex::new(r"记住了|下次我要|下次我会|以后").unwrap())
}

fn bracket_preference_regex() -> &'static Regex {
    static REGEX: OnceLock<Regex> = OnceLock::new();
    REGEX.get_or_init(|| Regex::new(r"喜欢|不喜欢|雷区|偏好|讨厌|更吃|不吃").unwrap())
}

pub(super) fn build_bracket_self_evolution_id(agent_id: &str, note_text: &str) -> String {
    let seed = format!("{}{}", agent_id.trim(), note_text.trim());
    format!(
        "bracket-self-evolution:{}",
        uuid::Uuid::new_v5(&uuid::Uuid::NAMESPACE_OID, seed.as_bytes())
    )
}

pub(super) fn classify_bracket_self_evolution(note_text: &str) -> String {
    let trimmed = note_text.trim();
    if bracket_decision_regex().is_match(trimmed) {
        "decision".to_string()
    } else if bracket_preference_regex().is_match(trimmed) {
        "preference".to_string()
    } else {
        "experience".to_string()
    }
}

pub(super) fn extract_bracket_self_evolution_notes(
    agent_id: &str,
    messages: &[Message],
) -> Vec<BracketSelfEvolutionNote> {
    let mut seen_ids = HashSet::new();
    let mut notes = Vec::new();

    for message in messages
        .iter()
        .filter(|message| message.role.trim().eq_ignore_ascii_case("assistant"))
    {
        for captures in bracket_note_regex().captures_iter(&message.content) {
            let note_text = captures
                .get(1)
                .or_else(|| captures.get(2))
                .map(|value| value.as_str().trim())
                .unwrap_or("");
            let char_count = note_text.chars().count();
            if !(4..=240).contains(&char_count) {
                continue;
            }
            if !bracket_strategy_regexes()
                .iter()
                .any(|pattern| pattern.is_match(note_text))
            {
                continue;
            }

            let id = build_bracket_self_evolution_id(agent_id, note_text);
            if !seen_ids.insert(id.clone()) {
                continue;
            }

            notes.push(BracketSelfEvolutionNote {
                id,
                text: note_text.to_string(),
                category: classify_bracket_self_evolution(note_text),
            });
        }
    }

    notes
}

pub(crate) async fn handle_capture_session(
    server: &MemoryServer,
    params: CaptureSessionParams,
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
            "captured": 0,
        }))
        .map_err(|e| format!("Failed to serialize capture_session response: {e}"));
    }

    if !params.force && combined_text.chars().count() < params.min_chars {
        return serde_json::to_string(&json!({
            "status": "skipped",
            "reason": "below_min_chars",
            "captured": 0,
        }))
        .map_err(|e| format!("Failed to serialize capture_session response: {e}"));
    }

    let requested_scope = normalize_scope(&params.scope, "project");
    let named_project = params.project.clone();
    let (target_db, warning) = if named_project.is_some() {
        (DbScope::Project, None)
    } else {
        server.resolve_write_scope(&requested_scope)
    };

    let base_path = params.path_prefix.clone().unwrap_or_else(|| {
        format!(
            "/openclaw/agent-{}",
            sanitize_safe_path_name(&params.agent_id)
        )
    });
    let source_ref_id = format!("{}:{}", params.conversation_id, params.turn_id);
    let self_evolution_path = format!("{}/self-evolution", base_path.trim_end_matches('/'));
    let attach_kyle_person = params.agent_id.to_ascii_lowercase().contains("jayne");

    let mut entries = Vec::<MemoryEntry>::new();
    for note in extract_bracket_self_evolution_notes(&params.agent_id, &params.messages) {
        let metadata = crate::provenance::inject_provenance(
            server,
            json!({
                "source_refs": [{
                    "ref_type": "turn",
                    "ref_id": source_ref_id.clone(),
                }],
                "conversation_id": params.conversation_id,
                "turn_id": params.turn_id,
                "agent_id": params.agent_id,
                "message_count": params.messages.len(),
                "artifact_kind": "bracket_self_evolution",
            }),
            "capture_session",
            "bracket_self_evolution",
            Some(requested_scope.as_str()),
            target_db,
            json!({
                "conversation_id": params.conversation_id,
                "turn_id": params.turn_id,
                "agent_id": params.agent_id,
                "path_prefix": base_path,
            }),
        );
        let strategy_keyword = if note.category == "preference" {
            "kyle-preference".to_string()
        } else {
            "strategy".to_string()
        };

        entries.push(MemoryEntry {
            id: note.id,
            path: self_evolution_path.clone(),
            summary: note.text.chars().take(100).collect(),
            text: note.text,
            importance: 0.88,
            timestamp: Utc::now().to_rfc3339(),
            category: note.category,
            topic: "self_evolution".to_string(),
            keywords: dedup_strings(vec![
                "self-evolution".to_string(),
                "bracket-note".to_string(),
                strategy_keyword,
            ]),
            persons: if attach_kyle_person {
                vec!["Kyle".to_string()]
            } else {
                Vec::new()
            },
            entities: Vec::new(),
            location: String::new(),
            source: "bracket_self_evolution".to_string(),
            scope: requested_scope.clone(),
            archived: false,
            access_count: 0,
            last_access: None,
            revision: 1,
            metadata,
            vector: None,
            retention_policy: None,
            domain: None,
        });
    }

    let payload = json!({
        "conversation_id": params.conversation_id,
        "turn_id": params.turn_id,
        "agent_id": params.agent_id,
        "messages": params.messages,
    });
    let request = serde_json::to_string_pretty(&payload)
        .map_err(|e| format!("Failed to serialize session capture payload: {e}"))?;
    let drafts = match server
        .llm
        .call_extract_llm(
            crate::prompts::SESSION_CAPTURE_PROMPT,
            &request,
            None,
            0.1,
            2400,
        )
        .await
    {
        Ok(raw) => match parse_session_capture_response(&raw) {
            Ok(drafts) => drafts,
            Err(err) if entries.is_empty() => {
                return serde_json::to_string(&json!({
                    "status": "failed",
                    "reason": "llm_capture_parse_failed",
                    "error": err,
                    "captured": 0,
                    "conversation_id": params.conversation_id,
                    "turn_id": params.turn_id,
                    "agent_id": params.agent_id,
                }))
                .map_err(|e| format!("Failed to serialize capture_session response: {e}"));
            }
            Err(_) => Vec::new(),
        },
        Err(err) if entries.is_empty() => {
            return serde_json::to_string(&json!({
                "status": "failed",
                "reason": "llm_capture_failed",
                "error": err,
                "captured": 0,
                "conversation_id": params.conversation_id,
                "turn_id": params.turn_id,
                "agent_id": params.agent_id,
            }))
            .map_err(|e| format!("Failed to serialize capture_session response: {e}"));
        }
        Err(_) => Vec::new(),
    };

    if drafts.is_empty() && entries.is_empty() {
        return serde_json::to_string(&json!({
            "status": "skipped",
            "reason": "no_durable_memories",
            "captured": 0,
        }))
        .map_err(|e| format!("Failed to serialize capture_session response: {e}"));
    }

    for draft in drafts {
        let topic = if draft.topic.trim().is_empty() {
            "session_capture".to_string()
        } else {
            draft.topic.trim().to_string()
        };
        let scope = normalize_scope(&draft.scope, &requested_scope);
        let metadata = crate::provenance::inject_provenance(
            server,
            json!({
                "source_refs": [{
                    "ref_type": "turn",
                    "ref_id": source_ref_id.clone(),
                }],
                "conversation_id": params.conversation_id,
                "turn_id": params.turn_id,
                "agent_id": params.agent_id,
                "message_count": params.messages.len(),
            }),
            "capture_session",
            "session_capture",
            Some(scope.as_str()),
            target_db,
            json!({
                "conversation_id": params.conversation_id,
                "turn_id": params.turn_id,
                "agent_id": params.agent_id,
                "path_prefix": base_path,
            }),
        );

        let summary = if draft.summary.trim().is_empty() {
            draft.text.chars().take(100).collect::<String>()
        } else {
            draft.summary.trim().to_string()
        };

        entries.push(MemoryEntry {
            id: uuid::Uuid::new_v4().to_string(),
            path: build_entry_path(&base_path, &topic),
            summary,
            text: draft.text.trim().to_string(),
            importance: draft.importance.clamp(0.0, 1.0),
            timestamp: Utc::now().to_rfc3339(),
            category: normalize_category(&draft.category),
            topic,
            keywords: dedup_strings(draft.keywords),
            persons: dedup_strings(draft.persons),
            entities: dedup_strings(draft.entities),
            location: draft.location.trim().to_string(),
            source: "capture_session".to_string(),
            scope,
            archived: false,
            access_count: 0,
            last_access: None,
            revision: 1,
            metadata,
            vector: None,
            retention_policy: None,
            domain: None,
        });
    }

    let texts = entries
        .iter()
        .map(|entry| entry.text.clone())
        .collect::<Vec<_>>();
    let embeddings = match server.llm.embed_voyage_batch(&texts, "document").await {
        Ok(vectors) => Some(vectors),
        Err(err) => {
            eprintln!("[capture_session] embedding failed, deferring enrichment: {err}");
            None
        }
    };
    if let Some(vectors) = embeddings.as_ref() {
        for (entry, vector) in entries.iter_mut().zip(vectors.iter()) {
            entry.vector = Some(vector.clone());
        }
    }

    let mut saved_ids = Vec::new();

    for entry in &entries {
        persist_capture_entry(server, target_db, named_project.as_deref(), entry)?;
        if embeddings.is_none() {
            queue_capture_enrichment(
                server,
                target_db,
                named_project.clone(),
                entry,
                false,
                Some(&params.agent_id),
                Some(&base_path),
            );
        }
        saved_ids.push(entry.id.clone());
    }

    let saved_ids = dedup_strings(saved_ids);
    let maintenance_jobs = enqueue_capture_maintenance_jobs(
        server,
        target_db,
        named_project.clone(),
        &params.agent_id,
        &base_path,
        &saved_ids,
        0,
        0,
    )?;

    let mut response = serde_json::Map::new();
    response.insert("status".into(), json!("completed"));
    response.insert("captured".into(), json!(saved_ids.len()));
    response.insert("ids".into(), json!(saved_ids));
    response.insert("merged_ids".into(), json!(Vec::<String>::new()));
    response.insert("duplicate_ids".into(), json!(Vec::<String>::new()));
    response.insert("duplicates_skipped".into(), json!(0));
    response.insert("maintenance_jobs".into(), json!(maintenance_jobs));
    response.insert("db".into(), json!(target_db.as_str()));
    response.insert("path_prefix".into(), json!(base_path));
    if let Some(warning) = warning {
        response.insert("warning".into(), json!(warning));
    }

    serde_json::to_string(&Value::Object(response))
        .map_err(|e| format!("Failed to serialize capture_session response: {e}"))
}
