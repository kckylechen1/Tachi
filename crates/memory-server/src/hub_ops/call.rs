use super::*;
use crate::hub_helpers::capability_callable;

pub(crate) async fn handle_run_skill(
    server: &MemoryServer,
    params: RunSkillParams,
) -> Result<String, String> {
    let cap = {
        let mut found = None;
        if server.has_project_db() {
            found = server.with_project_store(|store| {
                store
                    .hub_get(&params.skill_id)
                    .map_err(|e| format!("hub get project: {e}"))
            })?;
        }
        if found.is_none() {
            found = server.with_global_store(|store| {
                store
                    .hub_get(&params.skill_id)
                    .map_err(|e| format!("hub get global: {e}"))
            })?;
        }
        found.ok_or_else(|| format!("Skill '{}' not found in Hub", params.skill_id))?
    };

    if cap.cap_type != "skill" {
        return Err(format!(
            "'{}' is type '{}', not 'skill'",
            params.skill_id, cap.cap_type
        ));
    }
    if !capability_callable(&cap) {
        return Err(format!(
            "Skill '{}' is not callable (enabled={}, review_status={}, health_status={}).",
            params.skill_id, cap.enabled, cap.review_status, cap.health_status
        ));
    }

    let def: serde_json::Value = serde_json::from_str(&cap.definition)
        .map_err(|e| format!("invalid skill definition JSON: {e}"))?;

    let skill_content = def
        .get("content")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty());
    let prompt_template = def["prompt"]
        .as_str()
        .or_else(|| def["template"].as_str())
        .or(skill_content)
        .ok_or_else(|| {
            "skill definition missing 'prompt', 'template', or 'content' field".to_string()
        })?;

    let empty_args = serde_json::Map::new();
    let args_obj = params.args.as_object().unwrap_or(&empty_args);
    let resolved_prompt = render_skill_prompt_template(prompt_template, args_obj)
        .map_err(|e| format!("serialize skill args: {e}"))?;

    let result = if let Some(mock_response) = def.get("mock_response").and_then(|v| v.as_str()) {
        Ok(mock_response.to_string())
    } else {
        let default_system = "You are an AI assistant executing a specialized skill.";
        let system = def
            .get("system")
            .and_then(|v| v.as_str())
            .or_else(|| {
                if def["prompt"].as_str().is_none() && def["template"].as_str().is_none() {
                    skill_content
                } else {
                    None
                }
            })
            .unwrap_or(default_system);
        let model = def.get("model").and_then(|v| v.as_str());
        let temperature = def
            .get("temperature")
            .and_then(|v| v.as_f64())
            .unwrap_or(0.3) as f32;
        let max_tokens = def
            .get("max_tokens")
            .and_then(|v| v.as_u64())
            .unwrap_or(4000) as u32;

        server
            .llm
            .call_extract_llm(system, &resolved_prompt, model, temperature, max_tokens)
            .await
    };

    // Record call outcome for skill telemetry (enables skill_evolve)
    let success = result.is_ok();
    let error_msg = result.as_ref().err().map(|e| e.to_string());
    let _ = server.record_capability_call_outcome(&params.skill_id, success, error_msg.as_deref());

    result.map_err(|e| format!("skill execution failed: {}", e))
}

pub(crate) async fn handle_distill_trajectory(
    server: &MemoryServer,
    params: DistillTrajectoryParams,
) -> Result<String, String> {
    let named_project = params.project.clone();
    let (target_db, warning) = if named_project.is_some() {
        (DbScope::Project, None)
    } else {
        server.resolve_write_scope(&params.scope)
    };
    let domain = params.domain.clone().or_else(|| {
        std::env::var("TACHI_DOMAIN")
            .ok()
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty())
    });
    let distilled_markdown = handle_run_skill(
        server,
        RunSkillParams {
            skill_id: "skill:trajectory-distiller".to_string(),
            args: json!({
                "task_description": params.task_description,
                "execution_trace": params.execution_trace,
                "final_outcome": params.final_outcome,
                "agent_id": params.agent_id,
                "skill_path": params.skill_path,
                "domain": domain,
            }),
        },
    )
    .await?;

    let timestamp = Utc::now().to_rfc3339();
    let skill_id = params.skill_id.clone().unwrap_or_else(|| {
        format!(
            "skill:{}",
            sanitize_safe_path_name(params.skill_path.trim_matches('/'))
        )
    });
    let snapshot_path = format!(
        "{}/distilled/{}",
        params.skill_path.trim_end_matches('/'),
        Utc::now().format("%Y%m%dT%H%M%S")
    );
    let importance = params.importance.unwrap_or(0.85).clamp(0.0, 1.0);
    let snapshot_metadata = crate::provenance::inject_provenance(
        server,
        json!({
            "skill_id": skill_id,
            "task_description": params.task_description,
            "execution_trace": params.execution_trace,
            "final_outcome": params.final_outcome,
            "agent_id": params.agent_id,
            "source_skill": "skill:trajectory-distiller",
        }),
        "distill_trajectory",
        "trajectory_distill",
        Some(params.scope.as_str()),
        target_db,
        json!({
            "skill_path": params.skill_path,
            "domain": domain,
        }),
    );
    let snapshot_entry = MemoryEntry {
        id: uuid::Uuid::new_v4().to_string(),
        path: snapshot_path.clone(),
        summary: distilled_markdown.chars().take(100).collect(),
        text: distilled_markdown.clone(),
        importance,
        timestamp: timestamp.clone(),
        valid_from: String::new(),
        valid_until: None,
        category: "decision".to_string(),
        topic: sanitize_safe_path_name(params.skill_path.trim_matches('/')),
        keywords: vec![
            "trajectory".to_string(),
            "skill".to_string(),
            "distilled".to_string(),
        ],
        persons: vec![],
        entities: vec![skill_id.clone()],
        location: String::new(),
        source: "distill_trajectory".to_string(),
        scope: params.scope.clone(),
        archived: false,
        access_count: 0,
        last_access: None,
        revision: 1,
        metadata: snapshot_metadata,
        vector: None,
        retention_policy: Some("permanent".to_string()),
        domain: domain.clone(),
        recall_count: 0,
        query_diversity: 0,
        tier: "raw".to_string(),
    };

    let prior_snapshot = {
        let read_action = |store: &mut MemoryStore| {
            store
                .list_by_path(&format!("{}/distilled", params.skill_path), 8, false)
                .map_err(|e| format!("list distilled snapshots: {e}"))
        };
        let entries = if let Some(project_name) = named_project.as_deref() {
            server.with_named_project_store_read(project_name, read_action)?
        } else {
            server.with_store_for_scope_read(target_db, read_action)?
        };
        entries
            .into_iter()
            .filter(|entry| entry.id != snapshot_entry.id)
            .max_by(|a, b| a.timestamp.cmp(&b.timestamp))
    };

    if let Some(project_name) = named_project.as_deref() {
        server.with_named_project_store(project_name, |store| {
            store
                .upsert(&snapshot_entry)
                .map_err(|e| format!("save distilled snapshot: {e}"))
        })?;
    } else {
        server.with_store_for_scope(target_db, |store| {
            store
                .upsert(&snapshot_entry)
                .map_err(|e| format!("save distilled snapshot: {e}"))
        })?;
    }

    if let Some(previous) = prior_snapshot {
        let edge = memory_core::MemoryEdge {
            source_id: snapshot_entry.id.clone(),
            target_id: previous.id,
            relation: "follows".to_string(),
            weight: 0.8,
            metadata: json!({ "source": "distill_trajectory" }),
            created_at: timestamp.clone(),
            valid_from: String::new(),
            valid_to: None,
        };
        let save_edge = |store: &mut MemoryStore| store.add_edge(&edge).map_err(|e| format!("{e}"));
        if let Some(project_name) = named_project.as_deref() {
            let _ = server.with_named_project_store(project_name, save_edge);
        } else {
            let _ = server.with_store_for_scope(target_db, save_edge);
        }
    }

    let (prior_cap, cap_scope_label) = if let Some(project_name) = named_project.as_deref() {
        (
            server.with_named_project_store_read(project_name, |store| {
                store
                    .hub_get(&skill_id)
                    .map_err(|e| format!("hub get named project: {e}"))
            })?,
            "project",
        )
    } else if target_db == DbScope::Project {
        (
            server.with_store_for_scope_read(target_db, |store| {
                store
                    .hub_get(&skill_id)
                    .map_err(|e| format!("hub get project: {e}"))
            })?,
            "project",
        )
    } else {
        (
            server.with_global_store_read(|store| {
                store
                    .hub_get(&skill_id)
                    .map_err(|e| format!("hub get global: {e}"))
            })?,
            "global",
        )
    };
    let new_version = prior_cap.as_ref().map(|cap| cap.version + 1).unwrap_or(1);
    let skill_definition = json!({
        "system": "You are executing a distilled reusable skill. Follow the skill document closely and adapt it to the user's input.",
        "prompt": format!("Skill document:\\n\\n{}\\n\\nUser input:\\n{{{{input}}}}", distilled_markdown),
        "content": distilled_markdown,
        "policy": { "visibility": "listed" },
        "skill_path": params.skill_path,
        "domain": domain,
        "retention_policy": "permanent",
        "source": "distill_trajectory",
        "provenance": {
            "agent_id": params.agent_id,
            "final_outcome": params.final_outcome,
        },
        "tags": ["distilled", "trajectory", "permanent"],
    });
    let capability = HubCapability {
        id: skill_id.clone(),
        cap_type: "skill".to_string(),
        name: params
            .skill_path
            .trim_matches('/')
            .rsplit('/')
            .next()
            .unwrap_or("distilled-skill")
            .to_string(),
        version: new_version,
        description: format!("Distilled skill for {}", params.skill_path),
        definition: serde_json::to_string(&skill_definition)
            .map_err(|e| format!("serialize distilled skill: {e}"))?,
        enabled: true,
        review_status: "approved".to_string(),
        health_status: "healthy".to_string(),
        last_error: prior_cap.as_ref().and_then(|cap| cap.last_error.clone()),
        last_success_at: prior_cap
            .as_ref()
            .and_then(|cap| cap.last_success_at.clone()),
        last_failure_at: prior_cap
            .as_ref()
            .and_then(|cap| cap.last_failure_at.clone()),
        fail_streak: prior_cap.as_ref().map(|cap| cap.fail_streak).unwrap_or(0),
        active_version: None,
        exposure_mode: "direct".to_string(),
        uses: prior_cap.as_ref().map(|cap| cap.uses).unwrap_or(0),
        successes: prior_cap.as_ref().map(|cap| cap.successes).unwrap_or(0),
        failures: prior_cap.as_ref().map(|cap| cap.failures).unwrap_or(0),
        avg_rating: prior_cap.as_ref().map(|cap| cap.avg_rating).unwrap_or(0.5),
        last_used: prior_cap.as_ref().and_then(|cap| cap.last_used.clone()),
        created_at: prior_cap
            .as_ref()
            .map(|cap| cap.created_at.clone())
            .unwrap_or(timestamp.clone()),
        updated_at: timestamp,
    };

    if let Some(project_name) = named_project.as_deref() {
        server.with_named_project_store(project_name, |store| {
            store
                .hub_register(&capability)
                .map_err(|e| format!("register distilled skill: {e}"))
        })?;
    } else {
        server.with_store_for_scope(target_db, |store| {
            store
                .hub_register(&capability)
                .map_err(|e| format!("register distilled skill: {e}"))
        })?;
    }
    if should_expose_skill_tool(&capability) {
        let _ = server.register_skill_tool(&capability);
    }
    let skill_quality = crate::wiki_ops::refresh_skill_quality_guards(server)?;

    let mut response = serde_json::Map::new();
    response.insert("status".into(), json!("completed"));
    response.insert("skill_id".into(), json!(skill_id));
    response.insert("version".into(), json!(new_version));
    response.insert("snapshot_id".into(), json!(snapshot_entry.id));
    response.insert("snapshot_path".into(), json!(snapshot_path));
    response.insert("capability_scope".into(), json!(cap_scope_label));
    response.insert("db".into(), json!(target_db.as_str()));
    response.insert("skill_quality".into(), skill_quality);
    if let Some(warning) = warning {
        response.insert("warning".into(), json!(warning));
    }
    serde_json::to_string(&Value::Object(response)).map_err(|e| format!("serialize: {e}"))
}

pub(crate) async fn handle_tachi_audit_log(
    server: &MemoryServer,
    params: AuditLogParams,
) -> Result<String, String> {
    server.with_global_store(|store| {
        let entries = store
            .audit_log_list(params.limit, params.server_filter.as_deref())
            .map_err(|e| format!("audit log: {e}"))?;
        serde_json::to_string(&entries).map_err(|e| format!("serialize: {e}"))
    })
}

pub(crate) async fn handle_hub_call(
    server: &MemoryServer,
    params: HubCallParams,
) -> Result<String, String> {
    let target = server.resolve_call_target(&params.server_id)?;
    let resolved_server_id = target.resolved_id.clone();
    let cap = server
        .get_capability(&resolved_server_id)
        .map_err(|e| format!("{e}"))?;

    if !cap.enabled {
        return Err(format!(
            "MCP server '{}' is disabled. Use hub_set_enabled to activate after review.",
            resolved_server_id
        ));
    }
    if !review_status_allows_call(&cap.review_status) {
        return Err(format!(
            "Capability '{}' is not approved (review_status={}). Use hub_review first.",
            resolved_server_id, cap.review_status
        ));
    }
    if !health_status_allows_call(&cap.health_status) {
        return Err(format!(
            "Capability '{}' is circuit-open (health_status={}).",
            resolved_server_id, cap.health_status
        ));
    }
    if !cap.cap_type.eq_ignore_ascii_case("mcp") {
        return Err(format!(
            "Capability '{}' is type '{}', expected MCP.",
            resolved_server_id, cap.cap_type
        ));
    }

    let result = server
        .proxy_call_capability_internal(
            &resolved_server_id,
            Some(&target.requested_id),
            &params.tool_name,
            Some(params.arguments.clone()),
        )
        .await;

    let success = result.is_ok();
    let error_kind = result.as_ref().err().map(|e| format!("{e}"));
    if let Err(e) =
        server.record_capability_call_outcome(&resolved_server_id, success, error_kind.as_deref())
    {
        eprintln!(
            "[hub_call] failed to persist governance health for '{}': {}",
            resolved_server_id, e
        );
    }

    let result = result.map_err(|e| format!("{e}"))?;

    let content_texts: Vec<String> = result
        .content
        .iter()
        .filter_map(|c| {
            serde_json::to_value(c)
                .ok()
                .and_then(|v| v.get("text").and_then(|t| t.as_str().map(String::from)))
        })
        .collect();
    serde_json::to_string(&json!({
        "server": params.server_id,
        "requested_kind": target.requested_kind,
        "resolved_server": resolved_server_id,
        "resolution": target.resolution,
        "tool": params.tool_name,
        "content": content_texts,
        "is_error": result.is_error.unwrap_or(false),
    }))
    .map_err(|e| format!("serialize: {e}"))
}

pub(crate) async fn handle_hub_disconnect(
    server: &MemoryServer,
    params: HubDisconnectParams,
) -> Result<String, String> {
    let server_name = params
        .server_id
        .strip_prefix("mcp:")
        .unwrap_or(&params.server_id);

    // Remove from connection pool
    let had_connection = server.pool.remove_connection(server_name);

    // Also clear discovered tools cache
    {
        lock_or_recover(&server.tool_discovery.proxy_tools, "proxy_tools").remove(server_name);
    }

    serde_json::to_string(&json!({
        "server": server_name,
        "disconnected": had_connection,
        "message": if had_connection {
            "Connection dropped. Next hub_call will reconnect with latest config."
        } else {
            "No active connection found (will connect fresh on next hub_call)."
        },
    }))
    .map_err(|e| format!("serialize: {e}"))
}
