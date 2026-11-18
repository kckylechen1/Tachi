use super::security_scan::scan_skill_definition_with_llm;
use crate::tool_params::HubRegisterParams;
use crate::utils::append_warning;
use crate::MemoryServer;
use memcore::HubCapability;
use serde_json::json;
use tachi_hub::{
    capability_callable, capability_not_callable_reason, capability_visibility_for_cap,
    merge_skill_scans, scan_skill_definition, should_expose_skill_tool,
};

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
    if crate::builtins::is_retired_builtin_capability_id(&params.id) {
        return Err(format!(
            "Capability '{}' is retired and cannot be registered.",
            params.id
        ));
    }
    if params.cap_type.eq_ignore_ascii_case("mcp")
        || params.cap_type.eq_ignore_ascii_case("virtual")
    {
        return Err(format!(
            "Hub no longer registers '{}' capabilities: the upstream MCP proxy and virtual capabilities are retired. Configure MCP servers directly in the host.",
            params.cap_type
        ));
    }
    if !matches!(params.cap_type.as_str(), "skill" | "plugin") {
        return Err("Hub capability type must be 'skill' or 'plugin'.".to_string());
    }
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
    let exposure_mode = "direct".to_string();

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
        review_status: "approved".to_string(),
        health_status: "healthy".to_string(),
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
            if let Some(reason) = capability_not_callable_reason(&cap) {
                append_warning(
                    &mut resp,
                    format!("Skill registered for management/export but is not callable: {reason}"),
                );
            } else {
                append_warning(
                    &mut resp,
                    "Skill document registered but not listed in tools (policy.visibility != 'listed'). Read it with tachi_skill(action='run') or change policy.visibility.",
                );
            }
        }
        // audit G1: a skill's content changed; refresh its scope's quality
        // guards in the background (coalesced) rather than on this path.
        // Requested after this handler's own tool registration so a
        // background re-registration cannot be overwritten by it.
        server.request_skill_quality_refresh(
            target_db,
            crate::wiki_ops::SkillQualityRefreshReason::ContentChanged,
        );
    }

    serde_json::to_string(&serde_json::Value::Object(resp)).map_err(|e| format!("serialize: {e}"))
}
