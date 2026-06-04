use super::*;

const AGENT_EVOLUTION_PROPOSAL_SOURCE: &str = "foundry_agent_evolution";
const FOUNDRY_JOB_NAMESPACE: &str = "foundry_job";
const FOUNDRY_PROPOSAL_REVIEW_NAMESPACE: &str = "foundry_proposal_review";

fn parse_document_kind(raw: &str) -> Result<memory_core::AgentProfileDocumentKind, String> {
    memory_core::AgentProfileDocumentKind::parse(raw).ok_or_else(|| {
        format!(
            "Unknown document kind '{}'. Expected identity|agents|latest_truths|routing_policy|tool_policy|memory_policy|other",
            raw
        )
    })
}

fn parse_evidence_kind(raw: &str) -> Result<memory_core::FoundryEvidenceKind, String> {
    memory_core::FoundryEvidenceKind::parse(raw).ok_or_else(|| {
        format!(
            "Unknown evidence kind '{}'. Expected memory|reflection|tooluse|eval|ghost|session_outcome|skill_telemetry|profile_snapshot|proposal|other",
            raw
        )
    })
}

fn build_foundry_job(
    server: &MemoryServer,
    params: &SynthesizeAgentEvolutionParams,
) -> memory_core::FoundryJobSpec {
    let requested_by = server
        .agent_runtime_read()
        .agent_profile
        .as_ref()
        .map(|profile| profile.agent_id.clone());

    memory_core::FoundryJobSpec {
        id: format!("foundry-job:{}", uuid::Uuid::new_v4()),
        kind: memory_core::FoundryJobKind::AgentEvolution,
        lane: memory_core::FoundryModelLane::Reasoning,
        status: memory_core::FoundryJobStatus::Planned,
        target_agent_id: Some(params.agent_id.clone()),
        requested_by,
        created_at: Utc::now().to_rfc3339(),
        evidence_count: params.evidence.len()
            + params.evidence_paths.len()
            + params.memory_queries.len(),
        goal_count: params.goals.len(),
        metadata: json!({
            "display_name": params.display_name,
            "document_count": params.documents.len() + params.document_paths.len(),
            "memory_query_count": params.memory_queries.len(),
        }),
    }
}

fn has_evolution_inputs(params: &SynthesizeAgentEvolutionParams) -> bool {
    !params.documents.is_empty()
        || !params.document_paths.is_empty()
        || !params.evidence.is_empty()
        || !params.evidence_paths.is_empty()
        || !params.memory_queries.is_empty()
}

fn resolve_foundry_input_path(path: &str) -> Result<std::path::PathBuf, String> {
    let trimmed = path.trim();
    if trimmed.is_empty() {
        return Err("Input path cannot be empty".to_string());
    }

    let raw = std::path::Path::new(trimmed);
    let candidates = if raw.is_absolute() {
        vec![raw.to_path_buf()]
    } else {
        let cwd = std::env::current_dir()
            .map_err(|e| format!("Failed to resolve current directory: {e}"))?;
        let mut candidates = vec![cwd.join(raw)];
        if let Some(git_root) = find_git_root() {
            let repo_candidate = git_root.join(raw);
            if !candidates
                .iter()
                .any(|candidate| candidate == &repo_candidate)
            {
                candidates.push(repo_candidate);
            }
        }
        candidates
    };

    let allowed_roots = projection_allowed_roots();
    for candidate in candidates {
        let canonical = match std::fs::canonicalize(&candidate) {
            Ok(path) => path,
            Err(_) => continue,
        };
        if !allowed_roots.iter().any(|root| canonical.starts_with(root)) {
            continue;
        }
        let metadata = std::fs::symlink_metadata(&candidate).map_err(|e| {
            format!(
                "Failed to stat evolution input path '{}': {e}",
                candidate.display()
            )
        })?;
        if metadata.file_type().is_symlink()
            && !allowed_roots.iter().any(|root| canonical.starts_with(root))
        {
            return Err(format!(
                "Evolution input path '{}' resolves outside allowed roots",
                candidate.display()
            ));
        }
        if !canonical.is_file() {
            return Err(format!(
                "Evolution input path '{}' is not a file",
                canonical.display()
            ));
        }
        return Ok(canonical);
    }

    Err(format!(
        "Evolution input path '{}' does not exist inside allowed roots",
        trimmed
    ))
}

fn read_foundry_input_text(path: &str) -> Result<(String, String), String> {
    let resolved = resolve_foundry_input_path(path)?;
    let content = std::fs::read_to_string(&resolved).map_err(|e| {
        format!(
            "Failed to read evolution input file '{}': {e}",
            resolved.display()
        )
    })?;
    Ok((resolved.display().to_string(), content))
}

fn build_documents(
    params: &SynthesizeAgentEvolutionParams,
) -> Result<Vec<memory_core::AgentProfileDocument>, String> {
    let mut documents = params
        .documents
        .iter()
        .map(|doc| {
            Ok(memory_core::AgentProfileDocument {
                kind: parse_document_kind(&doc.kind)?,
                path: doc.path.clone(),
                content: doc.content.clone(),
            })
        })
        .collect::<Result<Vec<_>, String>>()?;

    for doc in &params.document_paths {
        let kind = parse_document_kind(&doc.kind)?;
        let (resolved_path, content) = read_foundry_input_text(&doc.path)?;
        documents.push(memory_core::AgentProfileDocument {
            kind,
            path: Some(resolved_path),
            content,
        });
    }

    Ok(documents)
}

async fn build_evidence(
    server: &MemoryServer,
    params: &SynthesizeAgentEvolutionParams,
) -> Result<Vec<memory_core::FoundryEvidence>, String> {
    let mut evidence = params
        .evidence
        .iter()
        .map(|item| {
            Ok(memory_core::FoundryEvidence {
                kind: parse_evidence_kind(&item.kind)?,
                title: item.title.clone(),
                content: item.content.clone(),
                source_ref: item.source_ref.clone(),
                path: item.path.clone(),
                weight: item.weight.max(0.0),
            })
        })
        .collect::<Result<Vec<_>, String>>()?;

    for item in &params.evidence_paths {
        let kind = parse_evidence_kind(&item.kind)?;
        let (resolved_path, content) = read_foundry_input_text(&item.path)?;
        let fallback_title = std::path::Path::new(&resolved_path)
            .file_name()
            .and_then(|name| name.to_str())
            .map(ToOwned::to_owned);
        evidence.push(memory_core::FoundryEvidence {
            kind,
            title: item.title.clone().or(fallback_title),
            content,
            source_ref: item
                .source_ref
                .clone()
                .or_else(|| Some(resolved_path.clone())),
            path: Some(resolved_path),
            weight: item.weight.max(0.0),
        });
    }

    for query in &params.memory_queries {
        let rows = search_memory_rows(
            server,
            SearchMemoryParams {
                query: query.query.clone(),
                query_vec: None,
                top_k: query.top_k.max(1),
                path_prefix: query.path_prefix.clone(),
                include_training: false,
                include_archived: false,
                candidates_per_channel: query.top_k.max(1).max(20),
                mmr_threshold: None,
                graph_expand_hops: 1,
                graph_relation_filter: None,
                weights: None,
                agent_role: None,
                project: query.project.clone(),
                domain: None,
                file_context: None,
                error_context: None,
                enable_rerank: false,
                as_of: None,
                include_metadata: false,
            },
            false,
        )
        .await?;
        if rows.is_empty() {
            continue;
        }
        let mut lines = vec![format!("Memory query: {}", query.query.trim())];
        for (idx, row) in rows.iter().enumerate() {
            let id = row.get("id").and_then(|value| value.as_str()).unwrap_or("");
            let topic = row
                .get("topic")
                .and_then(|value| value.as_str())
                .unwrap_or("");
            let summary = row
                .get("summary")
                .and_then(|value| value.as_str())
                .unwrap_or("");
            let text = row
                .get("text")
                .and_then(|value| value.as_str())
                .unwrap_or("");
            let path = row
                .get("path")
                .and_then(|value| value.as_str())
                .unwrap_or("");
            let relevance = row
                .get("relevance")
                .and_then(|value| value.as_f64())
                .unwrap_or(0.0);
            lines.push(format!(
                "{}. [{}] {} (id={}, score={:.3}, path={})",
                idx + 1,
                if topic.is_empty() { "memory" } else { topic },
                if summary.is_empty() { text } else { summary },
                id,
                relevance,
                path
            ));
            if !summary.is_empty() && summary != text {
                lines.push(format!("   Detail: {}", text));
            }
        }
        evidence.push(memory_core::FoundryEvidence {
            kind: memory_core::FoundryEvidenceKind::Memory,
            title: query
                .title
                .clone()
                .or_else(|| Some(format!("memory query: {}", query.query.trim()))),
            content: lines.join("\n"),
            source_ref: Some(format!("memory_query:{}", query.query.trim())),
            path: query.path_prefix.clone(),
            weight: query.weight.max(0.0),
        });
    }

    Ok(evidence)
}

fn build_synthesis_payload(
    params: &SynthesizeAgentEvolutionParams,
    documents: &[memory_core::AgentProfileDocument],
    evidence: &[memory_core::FoundryEvidence],
) -> Value {
    json!({
        "agent": {
            "agent_id": params.agent_id,
            "display_name": params.display_name,
        },
        "goals": params.goals,
        "documents": documents,
        "evidence": evidence,
    })
}

fn parse_synthesis_response(raw: &str) -> Result<memory_core::AgentEvolutionSynthesis, String> {
    let json_str = llm::LlmClient::strip_code_fence(raw);
    serde_json::from_str(json_str).map_err(|e| {
        format!(
            "Failed to parse agent evolution synthesis JSON: {e} — response was: {}",
            json_str
        )
    })
}

fn proposal_root(agent_id: &str) -> String {
    format!(
        "/foundry/agents/{}/proposals",
        sanitize_safe_path_name(agent_id)
    )
}

fn review_status_or_default(raw: Option<&str>) -> String {
    match raw.unwrap_or("").trim().to_ascii_lowercase().as_str() {
        "approved" => "approved".to_string(),
        "rejected" => "rejected".to_string(),
        "applied" => "applied".to_string(),
        _ => "proposed".to_string(),
    }
}

fn parse_review_status(raw: &str) -> Result<String, String> {
    match raw.trim().to_ascii_lowercase().as_str() {
        "approved" => Ok("approved".to_string()),
        "rejected" => Ok("rejected".to_string()),
        "applied" => Ok("applied".to_string()),
        other => Err(format!(
            "Invalid review status '{}'. Expected approved|rejected|applied",
            other
        )),
    }
}

fn save_foundry_job_state(
    server: &MemoryServer,
    job: &memory_core::FoundryJobSpec,
    status: &str,
    extra: serde_json::Value,
) -> Result<(), String> {
    let mut payload = serde_json::Map::new();
    payload.insert("job".into(), json!(job));
    payload.insert("status".into(), json!(status));
    payload.insert("updated_at".into(), json!(Utc::now().to_rfc3339()));
    if let Some(extra_obj) = extra.as_object() {
        for (key, value) in extra_obj {
            payload.insert(key.clone(), value.clone());
        }
    }
    let value_json = serde_json::to_string(&serde_json::Value::Object(payload))
        .map_err(|e| format!("Failed to serialize foundry job state: {e}"))?;
    server.with_global_store(|store| {
        store
            .set_state(FOUNDRY_JOB_NAMESPACE, &job.id, &value_json)
            .map_err(|e| format!("Failed to persist foundry job state: {e}"))?;
        Ok(())
    })
}

fn load_review_state(
    server: &MemoryServer,
    proposal_id: &str,
) -> Result<Option<serde_json::Value>, String> {
    server.with_global_store(|store| {
        match store.get_state_kv(FOUNDRY_PROPOSAL_REVIEW_NAMESPACE, proposal_id) {
            Ok(Some((value, _version))) => {
                let parsed =
                    serde_json::from_str(&value).unwrap_or_else(|_| json!({ "raw": value }));
                Ok(Some(parsed))
            }
            Ok(None) => Ok(None),
            Err(e) => Err(format!("Failed to load proposal review state: {e}")),
        }
    })
}

fn resolve_foundry_write_scope(server: &MemoryServer) -> (DbScope, Option<String>) {
    server.resolve_write_scope("project")
}

fn persist_agent_evolution_proposal(
    server: &MemoryServer,
    params: &SynthesizeAgentEvolutionParams,
    job: &memory_core::FoundryJobSpec,
    synthesis: &memory_core::AgentEvolutionSynthesis,
) -> Result<(String, String, DbScope), String> {
    let root = proposal_root(&params.agent_id);
    let path = format!(
        "{}/{}-{}",
        root,
        Utc::now().format("%Y%m%dT%H%M%S"),
        sanitize_safe_path_name(&job.id)
    );
    let summary = if synthesis.summary.trim().is_empty() {
        synthesis
            .no_change_reason
            .clone()
            .unwrap_or_else(|| "agent evolution proposal".to_string())
    } else {
        synthesis.summary.clone()
    };
    let text = serde_json::to_string_pretty(synthesis)
        .map_err(|e| format!("Failed to serialize agent evolution synthesis: {e}"))?;
    let (target_db, _warning) = resolve_foundry_write_scope(server);
    let metadata = crate::provenance::inject_provenance(
        server,
        json!({
            "job": job,
            "agent_id": params.agent_id,
            "display_name": params.display_name,
            "goals": params.goals,
            "document_count": params.documents.len() + params.document_paths.len(),
            "evidence_count": params.evidence.len()
                + params.evidence_paths.len()
                + params.memory_queries.len(),
            "proposal_count": synthesis.proposals.len(),
            "status": "proposed",
        }),
        "synthesize_agent_evolution",
        "agent_evolution_proposal",
        Some(target_db.as_str()),
        target_db,
        json!({
            "agent_id": params.agent_id,
            "proposal_path": path,
        }),
    );
    let derived_id = server.with_store_for_scope(target_db, |store| {
        store
            .save_derived(
                &text,
                &path,
                &summary,
                0.8,
                AGENT_EVOLUTION_PROPOSAL_SOURCE,
                target_db.as_str(),
                &metadata,
            )
            .map_err(|e| format!("Failed to save agent evolution proposal: {e}"))
    })?;
    Ok((derived_id, path, target_db))
}

async fn run_agent_evolution_synthesis(
    server: &MemoryServer,
    params: &SynthesizeAgentEvolutionParams,
    job: &memory_core::FoundryJobSpec,
) -> Result<
    (
        memory_core::AgentEvolutionSynthesis,
        String,
        String,
        DbScope,
    ),
    String,
> {
    let documents = build_documents(params)?;
    let evidence = build_evidence(server, params).await?;
    let payload = build_synthesis_payload(params, &documents, &evidence);
    let user = serde_json::to_string_pretty(&payload)
        .map_err(|e| format!("Failed to serialize synthesis request: {e}"))?;
    let response = server
        .llm
        .call_reasoning_llm(
            crate::prompts::AGENT_EVOLUTION_SYNTHESIS_PROMPT,
            &user,
            None,
            0.2,
            2400,
        )
        .await?;
    let synthesis = parse_synthesis_response(&response)?;
    let (proposal_id, proposal_path, target_db) =
        persist_agent_evolution_proposal(server, params, job, &synthesis)?;
    Ok((synthesis, proposal_id, proposal_path, target_db))
}

fn parse_derived_metadata(row: &serde_json::Value) -> serde_json::Value {
    row.get("metadata")
        .and_then(|value| value.as_str())
        .and_then(|raw| serde_json::from_str(raw).ok())
        .unwrap_or_else(|| json!({}))
}

fn parse_derived_synthesis(
    row: &serde_json::Value,
) -> Option<memory_core::AgentEvolutionSynthesis> {
    row.get("text")
        .and_then(|value| value.as_str())
        .and_then(|raw| serde_json::from_str(raw).ok())
}

fn proposal_record_from_row(
    row: &serde_json::Value,
    review: Option<serde_json::Value>,
) -> serde_json::Value {
    let metadata = parse_derived_metadata(row);
    let status = review_status_or_default(
        review
            .as_ref()
            .and_then(|value| value.get("status"))
            .and_then(|value| value.as_str())
            .or_else(|| metadata.get("status").and_then(|value| value.as_str())),
    );
    json!({
        "proposal_id": row.get("id").and_then(|value| value.as_str()).unwrap_or(""),
        "path": row.get("path").and_then(|value| value.as_str()).unwrap_or(""),
        "summary": row.get("summary").and_then(|value| value.as_str()).unwrap_or(""),
        "created_at": row.get("created_at").and_then(|value| value.as_str()).unwrap_or(""),
        "status": status,
        "metadata": metadata,
        "review": review,
        "synthesis": parse_derived_synthesis(row),
    })
}

fn load_agent_proposal_rows(
    server: &MemoryServer,
    agent_id: &str,
    limit: usize,
) -> Result<Vec<serde_json::Value>, String> {
    let root = proposal_root(agent_id);
    let (target_db, _warning) = resolve_foundry_write_scope(server);
    server.with_store_for_scope_read(target_db, |store| {
        store
            .list_derived_by_source(AGENT_EVOLUTION_PROPOSAL_SOURCE, &root, limit)
            .map_err(|e| format!("Failed to list agent evolution proposals: {e}"))
    })
}

fn markdown_heading_info(line: &str) -> Option<(usize, String)> {
    let trimmed = line.trim();
    if !trimmed.starts_with('#') {
        return None;
    }
    let level = trimmed.chars().take_while(|&ch| ch == '#').count();
    let title = trimmed[level..].trim();
    if level == 0 || title.is_empty() {
        None
    } else {
        Some((level, title.to_string()))
    }
}

fn apply_markdown_section_update(
    content: &str,
    section: Option<&str>,
    suggested_value: &str,
) -> String {
    let replacement = suggested_value.trim();
    if replacement.is_empty() {
        return content.to_string();
    }

    let Some(section) = section.map(str::trim).filter(|section| !section.is_empty()) else {
        if content.trim().is_empty() {
            return format!("{replacement}\n");
        }
        return format!("{}\n\n{}\n", content.trim_end(), replacement);
    };

    let lines = content.lines().collect::<Vec<_>>();
    let mut heading_index = None;
    let mut heading_level = None;
    for (idx, line) in lines.iter().enumerate() {
        if markdown_heading_info(line)
            .map(|(level, title)| {
                if title == section {
                    heading_level = Some(level);
                    true
                } else {
                    false
                }
            })
            .unwrap_or(false)
        {
            heading_index = Some(idx);
            break;
        }
    }

    if let Some(start_idx) = heading_index {
        let start_level = heading_level.unwrap_or(2);
        let mut end_idx = lines.len();
        for idx in (start_idx + 1)..lines.len() {
            if let Some((level, _)) = markdown_heading_info(lines[idx]) {
                if level <= start_level {
                    end_idx = idx;
                    break;
                }
            }
        }

        let mut rebuilt = Vec::new();
        rebuilt.extend_from_slice(&lines[..=start_idx]);
        rebuilt.push("");
        rebuilt.extend(replacement.lines());
        if end_idx < lines.len() {
            rebuilt.push("");
            rebuilt.extend_from_slice(&lines[end_idx..]);
        }
        return rebuilt.join("\n").trim_end().to_string() + "\n";
    }

    let mut suffix = String::new();
    if !content.trim().is_empty() {
        suffix.push_str(content.trim_end());
        suffix.push_str("\n\n");
    }
    suffix.push_str(&format!("## {section}\n\n{replacement}\n"));
    suffix
}

fn document_target_aliases(doc: &AgentEvolutionDocumentParams) -> Vec<String> {
    let mut aliases = Vec::new();
    if let Some(path) = &doc.path {
        if let Some(name) = std::path::Path::new(path)
            .file_name()
            .and_then(|s| s.to_str())
        {
            aliases.push(name.to_ascii_lowercase());
        }
    }
    let kind_alias = match parse_document_kind(&doc.kind) {
        Ok(memory_core::AgentProfileDocumentKind::Identity) => Some("identity"),
        Ok(memory_core::AgentProfileDocumentKind::Agents) => Some("agents"),
        Ok(memory_core::AgentProfileDocumentKind::LatestTruths) => Some("latest_truths"),
        Ok(memory_core::AgentProfileDocumentKind::RoutingPolicy) => Some("routing_policy"),
        Ok(memory_core::AgentProfileDocumentKind::ToolPolicy) => Some("tool_policy"),
        Ok(memory_core::AgentProfileDocumentKind::MemoryPolicy) => Some("memory_policy"),
        _ => None,
    };
    if let Some(alias) = kind_alias {
        aliases.push(alias.to_string());
        aliases.push(format!("{alias}.md"));
    }
    aliases.sort();
    aliases.dedup();
    aliases
}

fn proposal_targets_document(
    proposal: &memory_core::AgentEvolutionProposal,
    doc: &AgentEvolutionDocumentParams,
) -> bool {
    let target = proposal.target.trim().to_ascii_lowercase();
    if target.is_empty() {
        return false;
    }
    document_target_aliases(doc)
        .into_iter()
        .any(|alias| alias == target)
}

fn projection_allowed_roots() -> Vec<std::path::PathBuf> {
    let mut roots = Vec::new();
    if let Ok(cwd) = std::env::current_dir() {
        roots.push(cwd);
    }
    if let Some(workspace_root) = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(|path| path.parent())
    {
        roots.push(workspace_root.to_path_buf());
    }
    if let Some(git_root) = find_git_root() {
        roots.push(git_root);
    }
    if let Some(home) = dirs::home_dir() {
        for suffix in [".openclaw", ".claude", ".codex", ".cursor", ".agents"] {
            let root = home.join(suffix);
            if root.exists() {
                roots.push(root);
            }
        }
    }

    let mut canonical = roots
        .into_iter()
        .filter_map(|root| std::fs::canonicalize(root).ok())
        .collect::<Vec<_>>();
    canonical.sort();
    canonical.dedup();
    canonical
}

fn resolve_projection_write_path(path: &str) -> Result<std::path::PathBuf, String> {
    let trimmed = path.trim();
    if trimmed.is_empty() {
        return Err("Projected document path cannot be empty".to_string());
    }

    let raw = std::path::Path::new(trimmed);
    let candidates = if raw.is_absolute() {
        vec![raw.to_path_buf()]
    } else {
        let cwd = std::env::current_dir()
            .map_err(|e| format!("Failed to resolve current directory: {e}"))?;
        let mut candidates = vec![cwd.join(raw)];
        if let Some(git_root) = find_git_root() {
            let repo_candidate = git_root.join(raw);
            if !candidates
                .iter()
                .any(|candidate| candidate == &repo_candidate)
            {
                candidates.push(repo_candidate);
            }
        }
        candidates
    };
    let file_name = raw.file_name().ok_or_else(|| {
        format!(
            "Projected document path '{}' must include a file name",
            trimmed
        )
    })?;
    let allowed_roots = projection_allowed_roots();
    let mut last_parent_error = None;

    for candidate in candidates {
        let parent = match candidate.parent() {
            Some(parent) => parent,
            None => continue,
        };
        let canonical_parent = match std::fs::canonicalize(parent) {
            Ok(path) => path,
            Err(err) => {
                last_parent_error = Some(format!(
                    "Failed to resolve parent directory for projected document '{}': {err}",
                    trimmed
                ));
                continue;
            }
        };
        let resolved = canonical_parent.join(file_name);
        if allowed_roots.iter().any(|root| resolved.starts_with(root)) {
            if let Ok(metadata) = std::fs::symlink_metadata(&resolved) {
                if metadata.file_type().is_symlink() {
                    let canonical_target = std::fs::canonicalize(&resolved).map_err(|err| {
                        format!(
                            "Failed to resolve symlinked projected document '{}': {err}",
                            resolved.display()
                        )
                    })?;
                    if !allowed_roots
                        .iter()
                        .any(|root| canonical_target.starts_with(root))
                    {
                        return Err(format!(
                            "Projected document path '{}' resolves outside allowed roots",
                            resolved.display()
                        ));
                    }
                }
            }
            return Ok(resolved);
        }
    }

    if let Some(err) = last_parent_error {
        Err(err)
    } else {
        Err(format!(
            "Projected document path '{}' is outside allowed roots",
            trimmed
        ))
    }
}

async fn write_document_if_requested(path: &str, content: &str) -> Result<(), String> {
    let resolved = resolve_projection_write_path(path)?;
    tokio::fs::write(&resolved, content).await.map_err(|e| {
        format!(
            "Failed to write projected document {}: {e}",
            resolved.display()
        )
    })
}

fn mark_proposal_applied(server: &MemoryServer, proposal_id: &str) -> Result<(), String> {
    let reviewer = server
        .agent_runtime_read()
        .agent_profile
        .as_ref()
        .map(|profile| profile.agent_id.clone());
    let mut review = load_review_state(server, proposal_id)?.unwrap_or_else(|| json!({}));
    if let Some(obj) = review.as_object_mut() {
        obj.insert("status".into(), json!("applied"));
        obj.insert("applied_at".into(), json!(Utc::now().to_rfc3339()));
        obj.insert("applied_by".into(), json!(reviewer));
    }
    let value_json = serde_json::to_string(&review)
        .map_err(|e| format!("Failed to serialize applied proposal state: {e}"))?;
    server.with_global_store(|store| {
        store
            .set_state(FOUNDRY_PROPOSAL_REVIEW_NAMESPACE, proposal_id, &value_json)
            .map_err(|e| format!("Failed to persist applied proposal state: {e}"))?;
        Ok(())
    })
}

pub(crate) async fn handle_synthesize_agent_evolution(
    server: &MemoryServer,
    params: SynthesizeAgentEvolutionParams,
) -> Result<String, String> {
    if !has_evolution_inputs(&params) {
        return Err(
            "synthesize_agent_evolution requires at least one document or evidence item"
                .to_string(),
        );
    }

    let documents = build_documents(&params)?;
    let evidence = build_evidence(server, &params).await?;
    let job = build_foundry_job(server, &params);
    let payload = build_synthesis_payload(&params, &documents, &evidence);

    if params.dry_run {
        return serde_json::to_string(&json!({
            "status": "dry_run",
            "job": job,
            "request": payload,
        }))
        .map_err(|e| format!("Failed to serialize dry-run response: {e}"));
    }

    let (synthesis, proposal_id, proposal_path, target_db) =
        run_agent_evolution_synthesis(server, &params, &job).await?;

    serde_json::to_string(&json!({
        "status": "completed",
        "job": job,
        "proposal_id": proposal_id,
        "proposal_path": proposal_path,
        "db": target_db.as_str(),
        "synthesis": synthesis,
    }))
    .map_err(|e| format!("Failed to serialize synthesis response: {e}"))
}

pub(crate) async fn handle_queue_agent_evolution(
    server: &MemoryServer,
    params: SynthesizeAgentEvolutionParams,
) -> Result<String, String> {
    if !has_evolution_inputs(&params) {
        return Err(
            "queue_agent_evolution requires at least one document or evidence item".to_string(),
        );
    }

    let mut job = build_foundry_job(server, &params);
    job.status = memory_core::FoundryJobStatus::Queued;
    save_foundry_job_state(server, &job, "queued", json!({}))?;

    let server = server.clone();
    let params_for_task = params;
    let job_for_task = job.clone();
    tokio::spawn(async move {
        if let Err(err) = save_foundry_job_state(&server, &job_for_task, "running", json!({})) {
            eprintln!(
                "[foundry-agent-evolution] failed to update job {} to running: {err}",
                job_for_task.id
            );
        }

        match run_agent_evolution_synthesis(&server, &params_for_task, &job_for_task).await {
            Ok((synthesis, proposal_id, proposal_path, target_db)) => {
                let _ = save_foundry_job_state(
                    &server,
                    &job_for_task,
                    "completed",
                    json!({
                        "proposal_id": proposal_id,
                        "proposal_path": proposal_path,
                        "db": target_db.as_str(),
                        "proposal_count": synthesis.proposals.len(),
                    }),
                );
            }
            Err(err) => {
                eprintln!(
                    "[foundry-agent-evolution] job {} failed: {err}",
                    job_for_task.id
                );
                let _ = save_foundry_job_state(
                    &server,
                    &job_for_task,
                    "failed",
                    json!({ "error": err }),
                );
            }
        }
    });

    serde_json::to_string(&json!({
        "status": "queued",
        "job": job,
    }))
    .map_err(|e| format!("Failed to serialize queue response: {e}"))
}

pub(crate) async fn handle_list_agent_evolution_proposals(
    server: &MemoryServer,
    params: ListAgentEvolutionProposalsParams,
) -> Result<String, String> {
    let root = proposal_root(&params.agent_id);
    let limit = params.limit.max(1).min(100);
    let (target_db, _warning) = resolve_foundry_write_scope(server);
    let rows = server.with_store_for_scope_read(target_db, |store| {
        store
            .list_derived_by_source(AGENT_EVOLUTION_PROPOSAL_SOURCE, &root, limit)
            .map_err(|e| format!("Failed to list agent evolution proposals: {e}"))
    })?;

    let desired_status = params
        .status
        .as_deref()
        .map(|status| review_status_or_default(Some(status)));
    let mut proposals = Vec::new();
    for row in rows {
        let proposal_id = row
            .get("id")
            .and_then(|value| value.as_str())
            .unwrap_or("")
            .to_string();
        let review = load_review_state(server, &proposal_id)?;
        let record = proposal_record_from_row(&row, review);
        let status = record
            .get("status")
            .and_then(|value| value.as_str())
            .unwrap_or("proposed");
        if let Some(ref desired) = desired_status {
            if status != desired {
                continue;
            }
        }
        proposals.push(record);
    }

    serde_json::to_string(&json!({
        "agent_id": params.agent_id,
        "count": proposals.len(),
        "proposals": proposals,
    }))
    .map_err(|e| format!("Failed to serialize proposal list: {e}"))
}

pub(crate) async fn handle_review_agent_evolution_proposal(
    server: &MemoryServer,
    params: ReviewAgentEvolutionProposalParams,
) -> Result<String, String> {
    let status = parse_review_status(&params.status)?;
    let reviewer = server
        .agent_runtime_read()
        .agent_profile
        .as_ref()
        .map(|profile| profile.agent_id.clone());
    let review = json!({
        "status": status,
        "note": params.note,
        "reviewed_at": Utc::now().to_rfc3339(),
        "reviewed_by": reviewer,
    });
    let value_json = serde_json::to_string(&review)
        .map_err(|e| format!("Failed to serialize proposal review: {e}"))?;
    server.with_global_store(|store| {
        store
            .set_state(
                FOUNDRY_PROPOSAL_REVIEW_NAMESPACE,
                &params.proposal_id,
                &value_json,
            )
            .map_err(|e| format!("Failed to persist proposal review: {e}"))?;
        Ok(())
    })?;

    serde_json::to_string(&json!({
        "proposal_id": params.proposal_id,
        "review": review,
    }))
    .map_err(|e| format!("Failed to serialize proposal review response: {e}"))
}

pub(crate) async fn handle_project_agent_profile(
    server: &MemoryServer,
    params: ProjectAgentProfileParams,
) -> Result<String, String> {
    if params.documents.is_empty() {
        return Err("project_agent_profile requires at least one document".to_string());
    }

    let proposal_id_filter = params
        .proposal_ids
        .iter()
        .map(|id| id.trim().to_string())
        .filter(|id| !id.is_empty())
        .collect::<std::collections::HashSet<_>>();
    let rows = load_agent_proposal_rows(server, &params.agent_id, 100)?;

    let mut proposal_records = Vec::<serde_json::Value>::new();
    for row in rows {
        let proposal_id = row
            .get("id")
            .and_then(|value| value.as_str())
            .unwrap_or("")
            .to_string();
        if !proposal_id_filter.is_empty() && !proposal_id_filter.contains(&proposal_id) {
            continue;
        }
        let review = load_review_state(server, &proposal_id)?;
        let record = proposal_record_from_row(&row, review);
        let status = record
            .get("status")
            .and_then(|value| value.as_str())
            .unwrap_or("proposed");
        if params.approved_only && status != "approved" {
            continue;
        }
        proposal_records.push(record);
    }

    if proposal_records.is_empty() {
        return serde_json::to_string(&json!({
            "status": "skipped",
            "reason": "no_matching_proposals",
            "agent_id": params.agent_id,
        }))
        .map_err(|e| format!("Failed to serialize projection response: {e}"));
    }

    let mut projected_documents = Vec::<serde_json::Value>::new();
    let mut applied_proposal_ids = std::collections::HashSet::<String>::new();

    for doc in &params.documents {
        let mut content = doc.content.clone();
        let mut applied = Vec::<serde_json::Value>::new();
        for record in &proposal_records {
            let proposal_id = record
                .get("proposal_id")
                .and_then(|value| value.as_str())
                .unwrap_or("")
                .to_string();
            let status = record
                .get("status")
                .and_then(|value| value.as_str())
                .unwrap_or("proposed")
                .to_string();
            let Some(synthesis) = record.get("synthesis").cloned().and_then(|value| {
                serde_json::from_value::<memory_core::AgentEvolutionSynthesis>(value).ok()
            }) else {
                continue;
            };
            for proposal in synthesis.proposals {
                if !proposal_targets_document(&proposal, doc) {
                    continue;
                }
                content = apply_markdown_section_update(
                    &content,
                    proposal.target_section.as_deref(),
                    &proposal.suggested_value,
                );
                applied.push(json!({
                    "proposal_id": proposal_id,
                    "status": status,
                    "title": proposal.title,
                    "target_section": proposal.target_section,
                }));
                applied_proposal_ids.insert(proposal_id.clone());
            }
        }

        let written = if params.write && !applied.is_empty() {
            let path = doc.path.as_deref().ok_or_else(|| {
                "project_agent_profile write=true requires document paths".to_string()
            })?;
            write_document_if_requested(path, &content).await?;
            true
        } else {
            false
        };

        projected_documents.push(json!({
            "kind": doc.kind,
            "path": doc.path,
            "written": written,
            "applied_proposals": applied,
            "content": content,
        }));
    }

    if params.write {
        for proposal_id in &applied_proposal_ids {
            mark_proposal_applied(server, proposal_id)?;
        }
    }

    serde_json::to_string(&json!({
        "status": "completed",
        "agent_id": params.agent_id,
        "applied_count": applied_proposal_ids.len(),
        "documents": projected_documents,
    }))
    .map_err(|e| format!("Failed to serialize projection response: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_synthesis_response_accepts_json_object() {
        let parsed = parse_synthesis_response(
            r#"{
              "summary":"stable overall",
              "stable_signals":["keeps verifying changes"],
              "drift_signals":["routing is inconsistent"],
              "proposals":[
                {
                  "title":"Tighten routing policy",
                  "target":"AGENTS.md",
                  "target_section":"Runtime 分流（硬规则）",
                  "current_value":"old",
                  "suggested_value":"new",
                  "rationale":"eval shows repeated mismatch",
                  "risk":"medium",
                  "evidence_refs":["eval:2026-04-01"]
                }
              ],
              "no_change_reason":null
            }"#,
        )
        .expect("response should parse");

        assert_eq!(parsed.proposals.len(), 1);
        assert_eq!(parsed.proposals[0].target, "AGENTS.md");
    }

    #[test]
    fn parse_review_status_accepts_expected_values() {
        assert_eq!(parse_review_status("approved").unwrap(), "approved");
        assert_eq!(parse_review_status("rejected").unwrap(), "rejected");
        assert_eq!(parse_review_status("applied").unwrap(), "applied");
        assert!(parse_review_status("queued").is_err());
    }

    #[test]
    fn apply_markdown_section_update_replaces_existing_section() {
        let original = "# Identity\n\n## Routing\n\nold value\n\n## Other\n\nstay\n";
        let updated = apply_markdown_section_update(original, Some("Routing"), "new value");
        assert!(updated.contains("## Routing\n\nnew value"));
        assert!(updated.contains("## Other\n\nstay"));
        assert!(!updated.contains("old value"));
    }

    #[test]
    fn apply_markdown_section_update_appends_missing_section() {
        let updated =
            apply_markdown_section_update("# Identity\n", Some("Memory Policy"), "write less");
        assert!(updated.contains("## Memory Policy"));
        assert!(updated.contains("write less"));
    }

    #[test]
    fn apply_markdown_section_update_replaces_nested_subsections_with_parent() {
        let original = "# Identity\n\n## Routing\n\nold value\n\n### Detail\n\nkeep with section\n\n## Other\n\nstay\n";
        let updated = apply_markdown_section_update(original, Some("Routing"), "new value");
        assert!(updated.contains("## Routing\n\nnew value"));
        assert!(updated.contains("## Other\n\nstay"));
        assert!(!updated.contains("### Detail"));
        assert!(!updated.contains("old value"));
    }

    #[test]
    fn resolve_projection_write_path_accepts_repo_relative_file() {
        let repo_root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .and_then(|path| path.parent())
            .expect("repo root")
            .to_path_buf();
        let target = repo_root.join("docs/neural-foundry-v1.md");
        let resolved = resolve_projection_write_path(target.to_string_lossy().as_ref())
            .expect("repo path allowed");
        assert!(resolved.ends_with("docs/neural-foundry-v1.md"));
    }

    #[test]
    fn resolve_projection_write_path_rejects_outside_allowed_roots() {
        let outside = std::env::temp_dir().join("foundry-projection-outside.md");
        let err = resolve_projection_write_path(outside.to_string_lossy().as_ref())
            .expect_err("outside path should be rejected");
        assert!(err.contains("outside allowed roots"));
    }

    #[test]
    fn proposal_targets_document_matches_policy_kind_without_md_suffix() {
        let doc = AgentEvolutionDocumentParams {
            kind: "routing_policy".to_string(),
            path: None,
            content: String::new(),
        };
        let proposal = memory_core::AgentEvolutionProposal {
            title: "Tighten routing".to_string(),
            target: "routing_policy".to_string(),
            target_section: Some("Rules".to_string()),
            current_value: None,
            suggested_value: "new rule".to_string(),
            rationale: "safer".to_string(),
            risk: "medium".to_string(),
            evidence_refs: vec![],
        };

        assert!(proposal_targets_document(&proposal, &doc));
    }

    #[test]
    fn resolve_projection_write_path_rejects_symlink_target_outside_allowed_roots() {
        let root = std::env::temp_dir().join(format!("foundry-symlink-{}", uuid::Uuid::new_v4()));
        let allowed = root.join("allowed");
        let outside = root.join("outside");
        std::fs::create_dir_all(&allowed).expect("create allowed root");
        std::fs::create_dir_all(&outside).expect("create outside root");

        let original_cwd = std::env::current_dir().expect("cwd");
        std::env::set_current_dir(&allowed).expect("set cwd");

        let link_path = allowed.join("IDENTITY.md");
        let outside_target = outside.join("IDENTITY.md");
        std::fs::write(&outside_target, "outside").expect("seed outside target");
        std::os::unix::fs::symlink(&outside_target, &link_path).expect("create symlink");

        let err = resolve_projection_write_path(link_path.to_string_lossy().as_ref())
            .expect_err("symlink to outside root should be rejected");
        assert!(err.contains("resolves outside allowed roots"));

        std::env::set_current_dir(original_cwd).expect("restore cwd");
        let _ = std::fs::remove_file(&link_path);
        let _ = std::fs::remove_file(&outside_target);
        let _ = std::fs::remove_dir_all(&root);
    }
}
