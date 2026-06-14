use super::security_scan::{
    merge_skill_scans, scan_skill_definition, scan_skill_definition_with_llm,
};
use super::*;

const MAX_HUB_DEFINITION_BYTES: usize = 256 * 1024;

fn ensure_hub_definition_size(label: &str, definition: &str) -> Result<(), String> {
    let len = definition.len();
    if len > MAX_HUB_DEFINITION_BYTES {
        return Err(format!(
            "{label} definition is too large: {len} bytes exceeds {MAX_HUB_DEFINITION_BYTES} byte limit"
        ));
    }
    Ok(())
}

pub(crate) async fn handle_hub_register(
    server: &MemoryServer,
    params: HubRegisterParams,
) -> Result<String, String> {
    ensure_hub_definition_size("hub_register", &params.definition)?;

    let (target_db, warning) = server.resolve_write_scope(&params.scope);

    let mut resp = serde_json::Map::new();
    resp.insert("id".into(), json!(params.id));
    resp.insert("db".into(), json!(target_db.as_str()));
    resp.insert("version".into(), json!(params.version));
    if let Some(w) = warning {
        append_warning(&mut resp, w);
    }

    let mut cap_definition = params.definition.clone();
    let mut enabled = true;
    let mut exposure_mode = "direct".to_string();

    if params.cap_type == "mcp" {
        let mut def: serde_json::Value = serde_json::from_str(&params.definition)
            .map_err(|e| format!("invalid mcp definition JSON: {e}"))?;
        let transport_type = def["transport"].as_str().unwrap_or("stdio").to_string();
        let tool_exposure_mode =
            resolve_mcp_tool_exposure(&def, server.tool_discovery.mcp_tool_exposure_mode);
        exposure_mode = tool_exposure_mode.as_str().to_string();
        def["tool_exposure"] = json!(tool_exposure_mode.as_str());
        resp.insert("tool_exposure".into(), json!(tool_exposure_mode.as_str()));

        // Security: validate MCP server commands against the stricter MCP
        // allowlist. Interpreters/package runners are NOT auto-approved.
        let auto_enabled = if transport_type == "stdio" {
            if let Some(cmd) = def["command"].as_str() {
                is_trusted_mcp_command(cmd)
            } else {
                false
            }
        } else {
            true // SSE/HTTP are URLs, no local exec risk
        };

        let server_name = params
            .id
            .strip_prefix("mcp:")
            .unwrap_or(&params.id)
            .to_string();
        server.clear_proxy_tools(&server_name);

        clear_mcp_discovery_metadata(&mut def);
        server.clear_proxy_tools(&server_name);

        if !auto_enabled {
            let cmd = def["command"].as_str().unwrap_or("unknown");
            append_warning(
                &mut resp,
                format!(
                    "Command '{}' is not in the trusted allowlist. Capability registered in pending review state; approve and enable explicitly before discovery.",
                    cmd
                ),
            );
        } else {
            append_warning(
                &mut resp,
                "MCP discovery is deferred until review approval and enablement.",
            );
        }
        resp.insert("discovery".into(), json!("deferred"));
        // PR6: surface a structured eligibility signal so `hub_quick_add` can
        // safely auto-approve trusted-command MCP registrations without ever
        // bypassing the allowlist for untrusted commands. SSE/HTTP transports
        // (`auto_enabled = true` because there is no local exec risk) are also
        // marked eligible.
        resp.insert("auto_approval_eligible".into(), json!(auto_enabled));

        cap_definition = serde_json::to_string(&def)
            .map_err(|e| format!("Failed to serialize MCP definition: {e}"))?;
        ensure_hub_definition_size("serialized MCP", &cap_definition)?;

        // Governance gate: MCP capabilities must be reviewed before activation.
        enabled = false;
        resp.insert("enabled".into(), json!(false));
        resp.insert("review_status".into(), json!("pending"));
        append_warning(
            &mut resp,
            "MCP capability registered in pending review state; use hub_review to approve before hub_call.",
        );
    }

    if params.cap_type == "skill" {
        let mut def: serde_json::Value = serde_json::from_str::<serde_json::Value>(&cap_definition)
            .map_err(|e| format!("invalid skill definition JSON: {e}"))?;
        if !def.is_object() {
            return Err("invalid skill definition JSON: expected object".to_string());
        }

        let static_scan = scan_skill_definition(&def);
        let llm_scan = scan_skill_definition_with_llm(server, &def).await;
        let scan = merge_skill_scans(&static_scan, llm_scan.as_ref());

        let blocked = scan
            .get("blocked")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        let risk = scan
            .get("risk")
            .and_then(|v| v.as_str())
            .unwrap_or("unknown")
            .to_string();
        def["security_scan"] = scan.clone();
        resp.insert("skill_scan".into(), scan);
        if blocked {
            enabled = false;
            resp.insert("enabled".into(), json!(false));
            append_warning(
                &mut resp,
                format!(
                    "Skill static scan blocked registration activation (risk={risk}). Review definition and re-enable explicitly."
                ),
            );
        }
        cap_definition = serde_json::to_string(&def)
            .map_err(|e| format!("Failed to serialize skill definition: {e}"))?;
        ensure_hub_definition_size("serialized skill", &cap_definition)?;
    }

    let cap = HubCapability {
        id: params.id.clone(),
        cap_type: params.cap_type.clone(),
        name: params.name.clone(),
        version: params.version,
        description: params.description.clone(),
        definition: cap_definition,
        enabled,
        review_status: if params.cap_type == "mcp" {
            "pending".to_string()
        } else {
            "approved".to_string()
        },
        health_status: if params.cap_type == "mcp" {
            "unknown".to_string()
        } else {
            "healthy".to_string()
        },
        last_error: None,
        last_success_at: None,
        last_failure_at: None,
        fail_streak: 0,
        active_version: None,
        exposure_mode,
        uses: 0,
        successes: 0,
        failures: 0,
        avg_rating: 0.0,
        last_used: None,
        created_at: String::new(),
        updated_at: String::new(),
    };
    let visibility = capability_visibility_for_cap(&cap);
    resp.insert("visibility".into(), json!(visibility.as_str()));
    resp.insert("callable".into(), json!(capability_callable(&cap)));

    server.with_store_for_scope(target_db, |store| {
        store
            .hub_register(&cap)
            .map_err(|e| format!("Failed to register: {e}"))
    })?;

    if params.cap_type == "skill" {
        if should_expose_skill_tool(&cap) {
            match server.register_skill_tool(&cap) {
                Ok(tool_name) => {
                    resp.insert("tool_name".into(), json!(tool_name));
                }
                Err(e) => {
                    resp.insert("skill_error".into(), json!(e));
                }
            }
        } else {
            let _ = server.unregister_skill_tool(&cap.id);
            append_warning(
                &mut resp,
                "Skill registered but not listed in tools (policy.visibility != 'listed'). Use run_skill or change policy.visibility.",
            );
        }

        // L0 analysis: async background scan of the prompt template
        let def: serde_json::Value = match serde_json::from_str(&params.definition) {
            Ok(def) => def,
            Err(e) => {
                append_warning(
                    &mut resp,
                    format!("Skill definition is not valid JSON; skipped async analysis ({e})"),
                );
                json!({})
            }
        };
        if let Some(prompt_text) = def.get("prompt").and_then(|v| v.as_str()) {
            let llm = server.llm.clone();
            let claude_pool = server.claude_pool.clone();
            let cap_clone = cap;
            let desc_empty = params.description.is_empty();
            let db_path = match target_db {
                DbScope::Global => server.global_db_path.clone(),
                DbScope::Project => server
                    .project_db_path
                    .clone()
                    .unwrap_or_else(|| server.global_db_path.clone()),
            };
            let prompt_text = prompt_text.to_string();

            let cap_id = cap_clone.id.clone();

            tokio::spawn(async move {
                // Phase 2: try Claude CLI pool first; on Err, fall back to the
                // existing SiliconFlow/Qwen extract lane. Backend used is
                // reported in the audit log.
                let prompt_for_fallback = prompt_text.clone();
                let llm_for_fallback = llm.clone();
                let pool_result = crate::claude_pool::pool_call_with_fallback(
                    &claude_pool,
                    crate::prompts::SKILL_ANALYSIS_PROMPT,
                    &prompt_text,
                    "skill-analysis",
                    move || async move {
                        llm_for_fallback
                            .call_extract_llm(
                                crate::prompts::SKILL_ANALYSIS_PROMPT,
                                &prompt_for_fallback,
                                None,
                                0.3,
                                500,
                            )
                            .await
                    },
                )
                .await;

                match pool_result {
                    Ok((analysis_raw, source)) => {
                        let analysis_json: serde_json::Value = match serde_json::from_str(
                            llm::LlmClient::strip_code_fence(&analysis_raw),
                        ) {
                            Ok(parsed) => parsed,
                            Err(e) => {
                                eprintln!(
                                    "[skill-analysis] invalid JSON output for {}: {}; using raw summary fallback",
                                    cap_id, e
                                );
                                serde_json::json!({"summary": analysis_raw})
                            }
                        };

                        // Auto-fill description if it was empty — use spawn_blocking
                        // to avoid holding the SQLite connection on the async runtime
                        // and risking "database is locked" under concurrent writes.
                        if desc_empty {
                            if let Some(summary) = analysis_json["summary"].as_str() {
                                let mut updated_cap = cap_clone;
                                updated_cap.description = summary.to_string();
                                let db_str = db_path.to_string_lossy().to_string();
                                let cap_id_inner = cap_id.clone();
                                let _ = tokio::task::spawn_blocking(move || {
                                    match MemoryStore::open(&db_str) {
                                        Ok(store) => {
                                            if let Err(e) = store.hub_register(&updated_cap) {
                                                eprintln!(
                                                    "[skill-analysis] failed to persist auto description for {}: {}",
                                                    cap_id_inner, e
                                                );
                                            }
                                        }
                                        Err(e) => {
                                            eprintln!(
                                                "[skill-analysis] failed to open DB for {} at '{}': {}",
                                                cap_id_inner, db_str, e
                                            );
                                        }
                                    }
                                }).await;
                            }
                        }
                        eprintln!(
                            "[skill-analysis] {} (via {}): {:?}",
                            cap_id,
                            source.as_str(),
                            analysis_json
                        );
                    }
                    Err(e) => {
                        eprintln!("[skill-analysis] failed for {}: {}", cap_id, e);
                    }
                }
            });
            resp.insert("analysis".into(), json!("pending (async)"));
        }
    }

    serde_json::to_string(&serde_json::Value::Object(resp)).map_err(|e| format!("serialize: {e}"))
}
