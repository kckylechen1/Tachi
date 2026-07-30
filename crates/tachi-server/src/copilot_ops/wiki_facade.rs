use super::*;

pub(crate) async fn handle_tachi_wiki_write(
    server: &MemoryServer,
    params: WikiWriteParams,
) -> Result<String, String> {
    handle_tachi_wiki_write_inner(server, params, None).await
}

/// Internal model-derived wiki write. The receipt travels through the typed
/// save seam and is attached while building the entry for the first durable
/// write; public metadata can never populate this channel.
pub(crate) async fn handle_tachi_wiki_write_with_model_invocation(
    server: &MemoryServer,
    params: WikiWriteParams,
    invocation: tachi_llm::PersistedModelInvocationReceiptV1,
) -> Result<String, String> {
    handle_tachi_wiki_write_inner(server, params, Some(invocation)).await
}

async fn handle_tachi_wiki_write_inner(
    server: &MemoryServer,
    params: WikiWriteParams,
    model_invocation: Option<tachi_llm::PersistedModelInvocationReceiptV1>,
) -> Result<String, String> {
    crate::wiki_ops::validate_references(&params.references)?;

    if !params.force && memcore::is_noise_text(&params.text) {
        return serde_json::to_string(&json!({
            "saved": false,
            "noise": true,
            "reason": "Text detected as noise (greeting, denial, or meta-question). Not saved.",
            "hint": "Retry with force=true if this is intentional wiki content.",
        }))
        .map_err(|e| format!("serialize wiki_write noise response: {e}"));
    }

    let entry_text = params.text.clone();
    let topic = params
        .topic
        .clone()
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| wiki_slug(&params.title));
    let path = normalize_wiki_path(params.path.clone(), &topic);
    let requested_project = params.project.clone();
    let project_name = requested_project
        .clone()
        .unwrap_or_else(|| "wiki".to_string());
    let use_named_project =
        requested_project.is_some() || default_named_project_available(server, &project_name);
    let target_project = use_named_project.then(|| project_name.clone());
    if let Some(project) = target_project.as_deref() {
        server.prepare_named_project_store_for_write(project)?;
    }
    let summary = params
        .summary
        .clone()
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| params.title.chars().take(100).collect());
    let domain = params.domain.clone().or_else(|| Some("wiki".to_string()));

    let mut keywords = params.keywords.clone();
    keywords.push("wiki".to_string());
    keywords.sort();
    keywords.dedup();

    // Wiki content's canonical home is a dedicated wiki project DB. Production
    // callers should pass `project: "wiki"`; if absent we still write where the
    // server routes us, but we set `metadata.allow_cross_project=true` so
    // path-routing validation lets `/wiki/...` through on non-wiki DBs.
    // Audit B11: this metadata flag is the explicit opt-in required by
    // path_router::validate_path_for_db.
    let references = params.references.clone();
    let pattern_refs = if params.include_patterns {
        wiki_pattern_refs(server, &params, requested_project.as_deref())?
    } else {
        Vec::new()
    };
    let layer_metadata =
        wiki_layer_metadata(&path, &params.scope, target_project.as_deref(), &references);
    let mut wiki_metadata = params.metadata.clone().unwrap_or_else(|| json!({}));
    if !wiki_metadata.is_object() {
        return Err("metadata must be a JSON object when supplied for wiki write".to_string());
    }
    if let Some(obj) = wiki_metadata.as_object_mut() {
        // Ordinary wiki writes use typed evidence_refs_v1 as their canonical
        // top-level reference shape. Nested source_refs belong to their
        // containing metadata and are intentionally unaffected.
        obj.remove("source_refs");
        obj.remove("review_receipt");
        obj.remove("source_bundle_hash");
        obj.insert("wiki".to_string(), json!(true));
        obj.insert("wiki_title".to_string(), json!(params.title.clone()));
        obj.insert("user_force".to_string(), json!(params.force));
        obj.insert("allow_cross_project".to_string(), json!(true));
        if params.include_patterns {
            obj.insert("pattern_refs".to_string(), json!(pattern_refs));
        }
    }
    if let (Some(target), Some(layer)) = (wiki_metadata.as_object_mut(), layer_metadata.as_object())
    {
        for (key, value) in layer {
            target.insert(key.clone(), value.clone());
        }
    }

    let existing = with_existing_wiki_store(server, &project_name, use_named_project, |store| {
        find_wiki_entry_by_path_or_topic(store, &path, &topic)
    })?;
    if let Some(existing) = &existing {
        if let Some(obj) = wiki_metadata.as_object_mut() {
            obj.insert("wiki_update_of".to_string(), json!(existing.id));
            obj.insert(
                "wiki_previous_revision".to_string(),
                json!(existing.revision),
            );
        }
    }
    let update_id = existing.as_ref().map(|entry| entry.id.clone());
    let existing_revision = existing.as_ref().map(|entry| entry.revision).unwrap_or(1);
    let captured_at = Utc::now().to_rfc3339();
    let mut reference_mutations = build_evidence_refs_v1(&references, &captured_at)
        .into_iter()
        .map(|reference| {
            let target_kind = reference
                .target_kind
                .map(serde_json::to_value)
                .transpose()
                .map_err(|error| format!("serialize wiki target kind: {error}"))?
                .and_then(|value| value.as_str().map(str::to_string));
            memcore::db::ValidatedReferenceMutation::evidence(
                reference.target_ref,
                reference.captured_at,
                target_kind,
            )
            .map_err(|error| format!("validate wiki reference mutation: {error}"))
        })
        .collect::<Result<Vec<_>, _>>()?;
    if references.is_empty() {
        reference_mutations
            .push(memcore::db::ValidatedReferenceMutation::ensure_empty_evidence_refs_v1());
    }
    if existing
        .as_ref()
        .is_some_and(|entry| entry.metadata.get("source_refs").is_some())
    {
        reference_mutations
            .push(memcore::db::ValidatedReferenceMutation::tombstone_legacy_source_refs());
    }

    let save_params = SaveMemoryParams {
        text: entry_text.clone(),
        summary,
        path: path.clone(),
        importance: params.importance.clamp(0.0, 1.0),
        category: params.category,
        topic: topic.clone(),
        keywords,
        persons: vec![],
        entities: params.entities,
        location: String::new(),
        scope: params.scope,
        vector: None,
        id: update_id.clone(),
        force: true,
        auto_link: true,
        project: target_project.clone(),
        // #1041 F2: server-internal, programmatic construction (wiki
        // writes are their own write path, out of #1041 S1's scope per
        // F1) — `target_project.is_some()` preserves the exact pre-F2
        // gate behavior (this is not the raw client `project=`, it may
        // already be a resolved "wiki" default, but it was always this
        // call's own deliberate placement, never a session-identity
        // transport default).
        project_explicit: target_project.is_some(),
        retention_policy: Some(params.retention_policy),
        domain: domain.clone(),
        timestamp: None,
        valid_from: None,
        valid_until: None,
        metadata: Some(wiki_metadata),
        emit_continuity: false,
    };
    let save_result = match model_invocation {
        Some(invocation) => {
            crate::memory_search_ops::handle_save_memory_with_authorized_reference_mutations_and_invocation(
                server,
                save_params,
                reference_mutations,
                invocation,
            )
            .await?
        }
        None => {
            crate::memory_search_ops::handle_save_memory_with_authorized_reference_mutations(
                server,
                save_params,
                reference_mutations,
            )
            .await?
        }
    };

    let mut response: Value =
        serde_json::from_str(&save_result).map_err(|e| format!("parse wiki save response: {e}"))?;
    if let Some(obj) = response.as_object_mut() {
        obj.insert("wiki_path".to_string(), json!(path));
        obj.insert("wiki_topic".to_string(), json!(topic));
        obj.insert(
            "wiki_write_mode".to_string(),
            json!(if update_id.is_some() {
                "updated"
            } else {
                "created"
            }),
        );
    }
    let canonical_id = response
        .get("id")
        .and_then(Value::as_str)
        .ok_or_else(|| "wiki write response missing id".to_string())?
        .to_string();
    let duplicate_action = |store: &mut MemoryStore| {
        supersede_wiki_duplicates(store, &canonical_id, &path, &topic, &entry_text)
    };
    let duplicates_superseded = match with_existing_wiki_store(
        server,
        &project_name,
        use_named_project,
        duplicate_action,
    ) {
        Ok(count) => count,
        Err(err) => {
            tracing::warn!(wiki_path = %path, wiki_topic = %topic, error = %err, "wiki duplicate scan failed");
            0
        }
    };
    if let Some(obj) = response.as_object_mut() {
        obj.insert(
            "wiki_duplicates_superseded".to_string(),
            json!(duplicates_superseded),
        );
        obj.insert("pattern_refs".to_string(), json!(pattern_refs.clone()));
        if update_id.is_some() {
            obj.insert(
                "wiki_previous_revision".to_string(),
                json!(existing_revision),
            );
        }
    }
    let wiki_write_mode = if update_id.is_some() {
        "updated"
    } else {
        "created"
    };
    let continuity_event = crate::continuity_ops::emit_wiki_saved_event(
        server,
        crate::continuity_ops::WikiSavedEventInput {
            project: target_project.as_deref(),
            wiki_id: &canonical_id,
            path: &path,
            title: &params.title,
            topic: &topic,
            summary: response
                .get("summary")
                .and_then(Value::as_str)
                .unwrap_or_default(),
            text: &entry_text,
            domain: domain.as_deref(),
            mode: wiki_write_mode,
            references: &references,
            pattern_refs: &pattern_refs,
        },
    );
    crate::wiki_ops::append_wiki_log(
        server,
        "write",
        &format!(
            "{} | {} | {} duplicate(s) superseded",
            path,
            response
                .get("id")
                .and_then(Value::as_str)
                .unwrap_or("unknown"),
            duplicates_superseded
        ),
    );
    if let Some(obj) = response.as_object_mut() {
        obj.insert("continuity_event".to_string(), continuity_event);
    }
    serde_json::to_string(&response).map_err(|e| format!("serialize wiki_write: {e}"))
}

fn wiki_pattern_refs(
    server: &MemoryServer,
    params: &WikiWriteParams,
    project: Option<&str>,
) -> Result<Vec<Value>, String> {
    let query = params
        .pattern_query
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| {
            format!(
                "{} {}",
                params.title,
                params.summary.as_deref().unwrap_or_default()
            )
        });
    let limit = params.pattern_top_k.unwrap_or(5).clamp(1, 20);
    let entries =
        crate::continuity_ops::list_active_patterns(server, project, Some(&query), limit)?;
    Ok(entries
        .into_iter()
        .map(|entry| crate::continuity_ops::pattern_ref_json(&entry))
        .collect::<Vec<_>>())
}

pub(super) fn wiki_slug(input: &str) -> String {
    let mut output = String::new();
    let mut previous_was_sep = false;

    for ch in input.trim().chars() {
        if ch.is_alphanumeric() || matches!(ch, '_' | '.') {
            output.push(ch);
            previous_was_sep = false;
        } else if matches!(
            ch,
            '-' | ' ' | '\t' | '\n' | '\r' | ':' | '：' | '/' | '\\' | '|'
        ) {
            if !output.is_empty() && !previous_was_sep {
                output.push('-');
                previous_was_sep = true;
            }
        }
    }

    let slug = output.trim_matches(|ch| matches!(ch, '.' | '_' | '-'));
    if slug.is_empty() {
        "unnamed".to_string()
    } else {
        slug.chars().take(96).collect()
    }
}

pub(crate) async fn handle_tachi_wiki_search(
    server: &MemoryServer,
    params: WikiSearchParams,
) -> Result<String, String> {
    let path_prefix = params.path_prefix.unwrap_or_else(|| "/wiki".to_string());
    let top_k = crate::clamp_facade_top_k(params.top_k);
    let project = params.project.clone();
    let lifecycle_scope = params.lifecycle.clone();
    let plan = WikiReadPlan::from_project(project.as_deref())?;
    if let WikiReadPlan::NamedOnly(StoreRef::NamedProject { project }) = &plan {
        if !crate::memory_search_ops::named_project_db_exists(server, project) {
            return Err(format!("Wiki project '{project}' not found"));
        }
    }
    let mut rows = search_memory_rows(
        server,
        SearchMemoryParams {
            query: params.query.clone(),
            query_vec: None,
            top_k,
            path_prefix: Some(path_prefix.clone()),
            include_training: false,
            include_archived: params.include_archived,
            candidates_per_channel: top_k.max(20),
            mmr_threshold: None,
            graph_expand_hops: 1,
            graph_relation_filter: None,
            weights: params.weights.or(Some(HybridWeightsParam {
                semantic: 0.48,
                fts: 0.30,
                symbolic: 0.20,
                decay: 0.02,
                use_rrf: true,
            })),
            context_symbols: Vec::new(),
            agent_role: params.agent_role,
            project: params.project,
            domain: params.domain,
            file_context: params.file_context,
            error_context: params.error_context,
            enable_rerank: false,
            as_of: None,
            include_metadata: false,
            format: None,
        },
        matches!(plan, WikiReadPlan::NamedOnly(_)),
    )
    .await?;
    crate::wiki_ops::filter_user_facing_wiki_rows(&mut rows);
    // #1072 RED case 2: this is a second, independent search entry point
    // (`tachi_wiki(action='search')`'s non-JSON branch and `tachi_wiki_search`)
    // from `wiki_ops::search::collect_wiki_search_value` — both must gate
    // pending/candidate drafts out of the default result set.
    crate::wiki_ops::apply_wiki_lifecycle_gate_for_plan(
        server,
        &plan,
        &mut rows,
        lifecycle_scope.as_deref(),
    )?;

    crate::wiki_ops::append_wiki_log(
        server,
        "search",
        &format!("{} | {} result(s)", params.query, rows.len()),
    );

    Ok(crate::agent_markdown::format_wiki_search(
        &params.query,
        rows.len(),
        &serde_json::Value::Array(rows),
    ))
}
