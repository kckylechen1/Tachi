use super::*;

fn should_enqueue_enrichment(entry: &MemoryEntry) -> bool {
    entry.importance >= 0.5 || entry.vector.is_some()
}

fn path_root(path: &str) -> &str {
    path.trim_matches('/').split('/').next().unwrap_or("")
}

fn is_newer_than(new_ts: &str, old_ts: &str) -> bool {
    let new = chrono::DateTime::parse_from_rfc3339(new_ts);
    let old = chrono::DateTime::parse_from_rfc3339(old_ts);
    match (new, old) {
        (Ok(new), Ok(old)) => new > old,
        _ => new_ts > old_ts,
    }
}

fn should_supersede(
    new_entry: &MemoryEntry,
    old_entry: &MemoryEntry,
    shared_count: usize,
    symbolic_score: f64,
) -> bool {
    matches!(new_entry.category.as_str(), "fact" | "preference")
        && matches!(old_entry.category.as_str(), "fact" | "preference")
        && is_newer_than(&new_entry.timestamp, &old_entry.timestamp)
        && shared_count >= 2
        && (new_entry.topic == old_entry.topic || symbolic_score > 0.3)
        && path_root(&new_entry.path) == path_root(&old_entry.path)
}

pub(crate) async fn handle_save_memory(
    server: &MemoryServer,
    params: SaveMemoryParams,
) -> Result<String, String> {
    if !params.force && memory_core::is_noise_text(&params.text) {
        return serde_json::to_string(&json!({
            "saved": false,
            "noise": true,
            "reason": "Text detected as noise (greeting, denial, or meta-question). Not saved.",
            "hint": "Retry with force=true if this is intentional content.",
        }))
        .map_err(|e| format!("Failed to serialize: {}", e));
    }

    // Capture gate (Branch #4): validate domain, path bucket, min-chars, and
    // markdown-dump heuristic. Default mode = Warn (annotate response, write
    // proceeds). TACHI_CAPTURE_GATE=enforce switches to hard rejection.
    let gate_mode = crate::capture_gate::GateMode::from_env();
    let gate_decision = crate::capture_gate::evaluate(
        &crate::capture_gate::GateInput::new(
            &params.text,
            &params.path,
            params.domain.as_deref(),
            params.force,
        ),
        gate_mode,
    );
    if !gate_decision.accept {
        return serde_json::to_string(&json!({
            "saved": false,
            "rejected_by": "capture_gate",
            "mode": gate_decision.mode,
            "violations": gate_decision.violations,
            "hint": "Set TACHI_CAPTURE_GATE=warn to downgrade these to warnings, or pass force=true on save.",
        }))
        .map_err(|e| format!("Failed to serialize: {}", e));
    }
    let gate_warnings = if gate_decision.violations.is_empty() {
        None
    } else {
        Some(gate_decision.violations.clone())
    };

    let id = params
        .id
        .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
    let timestamp = params
        .timestamp
        .clone()
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| Utc::now().to_rfc3339());
    let requested_scope = params.scope.clone();
    let named_project = params.project.clone();
    let (target_db, warning) = if named_project.is_some() {
        (DbScope::Project, None) // Will use named project below
    } else {
        server.resolve_write_scope(&requested_scope)
    };

    let summary = params.summary;
    let needs_summary = summary.is_empty();
    let needs_embedding = params.vector.is_none();
    let path = params.path;
    let category = params.category;
    let topic = params.topic;
    let metadata = crate::provenance::inject_provenance(
        server,
        params.metadata.unwrap_or_else(|| json!({})),
        "save_memory",
        "memory_write",
        Some(requested_scope.as_str()),
        target_db,
        json!({
            "path": path.clone(),
            "category": category.clone(),
            "topic": topic.clone(),
        }),
    );

    let importance = params.importance.clamp(0.0, 1.0);

    let entry = MemoryEntry {
        id: id.clone(),
        path,
        summary,
        text: params.text,
        importance,
        timestamp: timestamp.clone(),
        category,
        topic,
        keywords: params.keywords,
        persons: params.persons,
        entities: params.entities,
        location: params.location,
        source: "mcp".to_string(),
        scope: requested_scope,
        archived: false,
        access_count: 0,
        last_access: None,
        revision: 1,
        metadata,
        vector: params.vector,
        retention_policy: params.retention_policy,
        domain: params.domain,
    };

    if let Some(ref project_name) = named_project {
        server.with_named_project_store(project_name, |store| {
            store
                .upsert(&entry)
                .map_err(|e| format_save_error(server, target_db, Some(project_name), &e))
        })?;
    } else {
        server.with_store_for_scope(target_db, |store| {
            store
                .upsert(&entry)
                .map_err(|e| format_save_error(server, target_db, None, &e))
        })?;
    }

    // Queue enrichment (embedding + summary) via the batcher instead of
    // spawning a per-item task. The batcher accumulates items and calls
    // the Voyage API in batch (up to 128 per request), dramatically
    // reducing API calls when the agent saves multiple memories in sequence.
    if (needs_embedding || needs_summary) && should_enqueue_enrichment(&entry) {
        let _ = server.enrich_tx.try_send(super::EnrichmentItem {
            id: id.clone(),
            text: entry.text.clone(),
            needs_embedding,
            needs_summary,
            target_db,
            named_project: params.project.clone(),
            foundry_agent_id: None,
            foundry_path_prefix: None,
            revision: 1,
        });
    }

    let mut response = serde_json::Map::new();
    response.insert("id".into(), json!(id));
    response.insert("timestamp".into(), json!(timestamp));
    response.insert("db".into(), json!(target_db.as_str()));
    response.insert("status".into(), json!("saved (enrichment pending)"));
    if let Some(warning) = warning {
        response.insert("warning".into(), json!(warning));
    }
    if let Some(violations) = gate_warnings {
        response.insert("capture_gate_warnings".into(), json!(violations));
    }

    if params.auto_link && !entry.entities.is_empty() {
        let auto_link_server = server.clone();
        let auto_link_id = id.clone();
        let auto_link_entry = entry.clone();
        let auto_link_entities = entry.entities.clone();
        let auto_link_named_project = params.project.clone();
        let auto_link_target_db = target_db;

        tokio::spawn(async move {
            for entity in &auto_link_entities {
                let query = entity.clone();
                let search_action = |store: &mut MemoryStore| {
                    store
                        .search(
                            &query,
                            Some(memory_core::SearchOptions {
                                top_k: 5,
                                ..Default::default()
                            }),
                        )
                        .map_err(|e| format!("{}", e))
                };

                let search_res = if let Some(ref p) = auto_link_named_project {
                    auto_link_server.with_named_project_store_read(p, search_action)
                } else {
                    auto_link_server.with_store_for_scope_read(auto_link_target_db, search_action)
                };

                if let Ok(results) = search_res {
                    for result in results {
                        if result.entry.id == auto_link_id {
                            continue;
                        }
                        let shared: Vec<&String> = result
                            .entry
                            .entities
                            .iter()
                            .filter(|e| auto_link_entities.contains(e))
                            .collect();
                        if !shared.is_empty() {
                            let now = chrono::Utc::now().to_rfc3339();
                            let supersedes = should_supersede(
                                &auto_link_entry,
                                &result.entry,
                                shared.len(),
                                result.score.symbolic,
                            );
                            let edge = memory_core::MemoryEdge {
                                source_id: auto_link_id.clone(),
                                target_id: result.entry.id.clone(),
                                relation: if supersedes {
                                    "supersedes"
                                } else {
                                    "related_to"
                                }
                                .to_string(),
                                weight: if supersedes { 0.9 } else { 0.5 },
                                metadata: json!({
                                    "auto_link": true,
                                    "shared_entities": shared,
                                }),
                                created_at: now.clone(),
                                valid_from: String::new(),
                                // New supersedes/related_to edges are open-ended.
                                // `get_edges` filters with `valid_to IS NULL OR valid_to > now`,
                                // so setting valid_to = Some(now) at creation time would
                                // immediately expire the edge and hide the supersession
                                // from graph readers, repair, and explainability. Edges
                                // are only closed/expired when the supersession is
                                // explicitly reversed.
                                valid_to: None,
                            };
                            let save_edge_action = |store: &mut MemoryStore| {
                                store.add_edge(&edge).map_err(|e| format!("{}", e))?;
                                if supersedes {
                                    store
                                        .connection()
                                        .execute(
                                            "UPDATE memories SET superseded_by = ?1, updated_at = ?2 WHERE id = ?3",
                                            rusqlite::params![auto_link_id, now, result.entry.id],
                                        )
                                        .map_err(|e| format!("{e}"))?;
                                }
                                Ok(())
                            };
                            let _ = if let Some(ref p) = auto_link_named_project {
                                auto_link_server.with_named_project_store(p, save_edge_action)
                            } else {
                                auto_link_server
                                    .with_store_for_scope(auto_link_target_db, save_edge_action)
                            };
                        }
                    }
                }
            }
        });
        response.insert("auto_link".into(), json!("pending"));
    }

    serde_json::to_string(&serde_json::Value::Object(response))
        .map_err(|e| format!("Failed to serialize response: {}", e))
}

/// Low-friction shortcut over `handle_save_memory`. Infers `path`, `category`,
/// and `importance` so callers only need to pass `text` (and optionally
/// `tags`). Internally constructs `SaveMemoryParams` and delegates, so noise
/// filter, capture gate, provenance injection, auto-link, and the enrichment
/// batcher all run identically to a direct save_memory call.
pub(crate) async fn handle_remember(
    server: &MemoryServer,
    params: RememberParams,
) -> Result<String, String> {
    // Default path = /notes/{YYYY-MM-DD} so quick captures land in a
    // predictable, browsable bucket without forcing the caller to choose one.
    let inferred_path = params.path.unwrap_or_else(|| {
        let date = Utc::now().format("%Y-%m-%d");
        format!("/notes/{date}")
    });

    let save_params = SaveMemoryParams {
        text: params.text,
        summary: params.summary,
        path: inferred_path,
        importance: params.importance.unwrap_or(0.6).clamp(0.0, 1.0),
        category: params.category.unwrap_or_else(|| "fact".to_string()),
        topic: params.topic,
        keywords: params.tags,
        persons: Vec::new(),
        entities: Vec::new(),
        location: String::new(),
        scope: params.scope.unwrap_or_else(|| "project".to_string()),
        vector: None,
        id: None,
        force: params.force,
        auto_link: true,
        project: params.project,
        retention_policy: params.retention_policy,
        domain: params.domain,
        timestamp: None,
        metadata: Some(json!({ "shortcut": "remember" })),
    };

    handle_save_memory(server, save_params).await
}

pub(super) async fn search_memory_rows(
    server: &MemoryServer,
    mut params: SearchMemoryParams,
) -> Result<Vec<serde_json::Value>, String> {
    if memory_core::should_skip_query(&params.query) {
        return Ok(vec![]);
    }

    let named_project_vec_available = if let Some(ref project_name) = params.project {
        server
            .with_named_project_store_read(project_name, |store| Ok(store.vec_available))
            .unwrap_or(false)
    } else {
        false
    };

    if params.query_vec.is_none()
        && (server.global_vec_available
            || server.project_vec_available
            || named_project_vec_available)
    {
        match server.llm.embed_voyage(&params.query, "query").await {
            Ok(query_vec) => {
                params.query_vec = Some(query_vec);
            }
            Err(e) => {
                eprintln!(
                    "[search_memory] query embedding failed, falling back to lexical-only search: {e}"
                );
            }
        }
    }

    let pipeline_enabled = server.pipeline_enabled;

    let mut combined_results: Vec<(memory_core::SearchResult, DbScope)> = Vec::new();

    let global_opts = params.to_search_options(server.global_vec_available);

    let global_results = server.with_global_store_read(|store| {
        store
            .search(&params.query, Some(global_opts))
            .map_err(|e| format!("Search failed in global DB: {}", e))
    })?;
    combined_results.extend(global_results.into_iter().map(|r| (r, DbScope::Global)));

    // Search project DB — either named project or default
    if let Some(ref project_name) = params.project {
        let project_results = server.with_named_project_store_read(project_name, |store| {
            let vec_avail = store.vec_available;
            let project_opts = params.to_search_options(vec_avail);
            store
                .search(&params.query, Some(project_opts))
                .map_err(|e| format!("Search failed in project DB '{}': {}", project_name, e))
        })?;
        combined_results.extend(project_results.into_iter().map(|r| (r, DbScope::Project)));
    } else if server.has_project_db() {
        let project_opts = params.to_search_options(server.project_vec_available);

        let project_results = server.with_project_store_read(|store| {
            store
                .search(&params.query, Some(project_opts))
                .map_err(|e| format!("Search failed in project DB: {}", e))
        })?;
        combined_results.extend(project_results.into_iter().map(|r| (r, DbScope::Project)));
    }

    combined_results.sort_by(|a, b| {
        b.0.score
            .final_score
            .partial_cmp(&a.0.score.final_score)
            .unwrap_or(std::cmp::Ordering::Equal)
    });

    let mut seen_ids = HashSet::new();
    let mut deduped_results: Vec<(memory_core::SearchResult, DbScope)> = Vec::new();
    for (result, db_scope) in combined_results {
        if seen_ids.insert(result.entry.id.clone()) {
            deduped_results.push((result, db_scope));
        }
        if deduped_results.len() >= params.top_k {
            break;
        }
    }

    // Sandbox filtering: if agent_role is specified, filter out denied entries
    if let Some(ref role) = params.agent_role {
        deduped_results.retain(|(result, db_scope)| {
            let allowed = match db_scope {
                DbScope::Global => server.with_global_store_read(|store| {
                    store
                        .check_sandbox_access(role, &result.entry.path, "read")
                        .map(|(allowed, _)| allowed)
                        .map_err(|e| format!("{e}"))
                }),
                DbScope::Project => {
                    if let Some(ref p) = params.project {
                        server.with_named_project_store_read(p, |store| {
                            store
                                .check_sandbox_access(role, &result.entry.path, "read")
                                .map(|(allowed, _)| allowed)
                                .map_err(|e| format!("{e}"))
                        })
                    } else {
                        server.with_project_store_read(|store| {
                            store
                                .check_sandbox_access(role, &result.entry.path, "read")
                                .map(|(allowed, _)| allowed)
                                .map_err(|e| format!("{e}"))
                        })
                    }
                }
            };
            allowed.unwrap_or(true)
        });
    }

    let mut output: Vec<serde_json::Value> = deduped_results
        .iter()
        .map(|(r, db_scope)| slim_search_result(r, *db_scope))
        .collect();

    if pipeline_enabled {
        let mut existing_ids: HashSet<String> = deduped_results
            .iter()
            .map(|(r, _)| r.entry.id.clone())
            .collect();

        if server.has_project_db() {
            let project_rules = server.with_project_store_read(|store| {
                Ok(store
                    .list_by_path("/behavior/global_rules", 50, false)
                    .unwrap_or_default())
            })?;
            for rule in project_rules {
                if !is_active_global_rule(&rule) {
                    continue;
                }
                if !existing_ids.insert(rule.id.clone()) {
                    continue;
                }
                output.push(slim_l0_rule(&rule, DbScope::Project));
            }
        }

        let global_rules = server.with_global_store_read(|store| {
            Ok(store
                .list_by_path("/behavior/global_rules", 50, false)
                .unwrap_or_default())
        })?;
        for rule in global_rules {
            if !is_active_global_rule(&rule) {
                continue;
            }
            if !existing_ids.insert(rule.id.clone()) {
                continue;
            }
            output.push(slim_l0_rule(&rule, DbScope::Global));
        }
    }

    Ok(output)
}

pub(crate) async fn handle_search_memory(
    server: &MemoryServer,
    params: SearchMemoryParams,
) -> Result<String, String> {
    let rows = search_memory_rows(server, params).await?;
    serde_json::to_string(&rows).map_err(|e| format!("Failed to serialize response: {}", e))
}

pub(crate) async fn handle_find_similar_memory(
    server: &MemoryServer,
    params: FindSimilarMemoryParams,
) -> Result<String, String> {
    if params.query_vec.is_empty() {
        return serde_json::to_string(&json!([]))
            .map_err(|e| format!("Failed to serialize response: {}", e));
    }

    if params.query_vec.iter().any(|v| !v.is_finite()) {
        return Err("query_vec contains non-finite values".to_string());
    }

    let mut combined_results: Vec<(memory_core::SearchResult, DbScope)> = Vec::new();
    let common_weights = memory_core::HybridWeights {
        semantic: 1.0,
        fts: 0.0,
        symbolic: 0.0,
        decay: 0.0,
    };

    let global_opts = SearchOptions {
        candidates_per_channel: params.candidates_per_channel.max(params.top_k),
        top_k: params.top_k,
        weights: common_weights.clone(),
        path_prefix: params.path_prefix.clone(),
        query_vec: Some(params.query_vec.clone()),
        vec_available: server.global_vec_available,
        record_access: false,
        include_archived: params.include_archived,
        mmr_threshold: None,
        graph_expand_hops: 0,
        graph_relation_filter: None,
        domain: None,
    };

    let global_results = server.with_global_store_read(|store| {
        store
            .search("", Some(global_opts))
            .map_err(|e| format!("Vector search failed in global DB: {}", e))
    })?;
    combined_results.extend(global_results.into_iter().map(|r| (r, DbScope::Global)));

    if server.has_project_db() {
        let project_opts = SearchOptions {
            candidates_per_channel: params.candidates_per_channel.max(params.top_k),
            top_k: params.top_k,
            weights: common_weights,
            path_prefix: params.path_prefix.clone(),
            query_vec: Some(params.query_vec.clone()),
            vec_available: server.project_vec_available,
            record_access: false,
            include_archived: params.include_archived,
            mmr_threshold: None,
            graph_expand_hops: 0,
            graph_relation_filter: None,
            domain: None,
        };

        let project_results = server.with_project_store_read(|store| {
            store
                .search("", Some(project_opts))
                .map_err(|e| format!("Vector search failed in project DB: {}", e))
        })?;
        combined_results.extend(project_results.into_iter().map(|r| (r, DbScope::Project)));
    }

    combined_results.sort_by(|a, b| {
        b.0.score
            .vector
            .partial_cmp(&a.0.score.vector)
            .unwrap_or(std::cmp::Ordering::Equal)
    });

    let mut seen_ids = HashSet::new();
    let mut output: Vec<serde_json::Value> = Vec::new();
    for (result, db_scope) in combined_results {
        if !seen_ids.insert(result.entry.id.clone()) {
            continue;
        }
        let mut obj = match slim_entry(&result.entry, db_scope) {
            serde_json::Value::Object(m) => m,
            _ => serde_json::Map::new(),
        };
        obj.insert(
            "similarity".into(),
            json!((result.score.vector * 1000.0).round() / 1000.0),
        );
        output.push(serde_json::Value::Object(obj));
        if output.len() >= params.top_k {
            break;
        }
    }

    serde_json::to_string(&output).map_err(|e| format!("Failed to serialize response: {}", e))
}

/// Format a save-path error string. When the underlying SQLite error indicates
/// a readonly database, attach the resolved DB path, the active scope/profile,
/// and a concrete remediation hint. Non-readonly errors fall through to the
/// previous one-line format so existing callers (and tests) keep working.
fn format_save_error(
    server: &MemoryServer,
    target_db: DbScope,
    named_project: Option<&str>,
    err: &dyn std::fmt::Display,
) -> String {
    let err_str = err.to_string();
    let lower = err_str.to_ascii_lowercase();
    let is_readonly = lower.contains("readonly")
        || lower.contains("read-only")
        || lower.contains("read only")
        || lower.contains("attempt to write a readonly database");

    if !is_readonly {
        return match named_project {
            Some(name) => format!("Failed to save memory to '{}': {}", name, err_str),
            None => format!("Failed to save memory: {}", err_str),
        };
    }

    let db_path = match named_project {
        Some(name) => crate::MemoryServer::resolve_named_project_db_path(name)
            .map(|p| p.display().to_string())
            .unwrap_or_else(|_| format!("<named project: {name}>")),
        None => match target_db {
            DbScope::Global => server.global_db_path.display().to_string(),
            DbScope::Project => server
                .project_db_path
                .as_ref()
                .map(|p| p.display().to_string())
                .unwrap_or_else(|| "<no project DB configured>".to_string()),
        },
    };

    let profile_label = server
        .active_tool_profile()
        .map(|p| p.as_str())
        .unwrap_or_else(|| "admin".to_string());

    format!(
        "Failed to save memory: database is read-only.\n  \
         db_path: {db_path}\n  \
         scope: {scope}\n  \
         profile: {profile_label}\n  \
         hints:\n    \
         - Another process may hold an exclusive lock; check for stale `tachi` daemons.\n    \
         - File permissions may be wrong; ensure the user owns the DB file and parent dir.\n    \
         - The DB may have been opened read-only by an earlier CLI command — restart the daemon.\n    \
         - If targeting the wrong DB, pass --global-db / --project-db (or `project=` on the call).\n  \
         underlying: {err_str}",
        scope = target_db.as_str(),
    )
}
