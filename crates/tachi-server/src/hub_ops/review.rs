use crate::mcp_proxy::{
    filter_mcp_tools_by_permissions, set_mcp_discovery_failure, set_mcp_discovery_success,
};
use crate::tool_params::{HubReviewParams, HubSetActiveVersionParams, HubSetEnabledParams};
use crate::{DbScope, MemoryServer};
use memcore::HubCapability;
use serde_json::json;
use tachi_hub::{
    capability_callable, health_status_allows_call, normalize_review_status,
    review_status_allows_call, should_expose_skill_tool,
};

/// Whether an MCP capability is eligible to attempt (re)discovery.
///
/// This is deliberately **narrower** than [`capability_callable`]: it checks
/// every non-discovery condition (enabled, review approved, health not
/// "open"/circuit-broken) but does NOT require `discovery_status == "ready"`.
/// If it required that, a freshly-approved MCP could never bootstrap its
/// *first* discovery, because discovery is exactly what produces that status
/// (codex review on #968, commit 9204844a: registration intentionally
/// withholds discovery metadata until review, so the execution gate and the
/// discovery-refresh gate must differ).
fn mcp_discovery_eligible(cap: &HubCapability) -> bool {
    cap.enabled
        && review_status_allows_call(&cap.review_status)
        && health_status_allows_call(&cap.health_status)
}

/// Refresh discovery state for an MCP capability after a review/enable
/// transition.
///
/// Two-gate design, do not collapse them:
/// - **Execution gate** (`capability_callable`, in `tachi-hub`, unchanged by
///   this fix): fail-closed — a capability is only callable once discovery
///   has actually succeeded (`discovery_status == "ready"`). This is what
///   `hub_call` and friends check before invoking an MCP tool.
/// - **Discovery-refresh gate** (`mcp_discovery_eligible`, above): whether we
///   are even *allowed to attempt* discovery right now. It deliberately
///   excludes `discovery_status`, because requiring "already discovered" as
///   a precondition to "go discover" is a deadlock — no approved+enabled MCP
///   would ever reach its first discovery, execution would stay permanently
///   fail-closed, and registration/quick-add flows would be bricked.
async fn refresh_mcp_capability_state(
    server: &MemoryServer,
    target_db: DbScope,
    cap: &HubCapability,
) -> Result<HubCapability, String> {
    let Some(server_name) = cap.id.strip_prefix("mcp:") else {
        return Ok(cap.clone());
    };

    if !mcp_discovery_eligible(cap) {
        server.clear_proxy_tools(server_name);
        return Ok(cap.clone());
    }

    let mut updated_cap = cap.clone();
    let mut def: serde_json::Value = serde_json::from_str(&cap.definition)
        .map_err(|e| format!("invalid mcp definition JSON: {e}"))?;

    match server.discover_mcp_tools(&cap.id, &def).await {
        Ok(tools) => {
            let filtered_tools = filter_mcp_tools_by_permissions(&def, tools);
            set_mcp_discovery_success(&mut def, &filtered_tools);
            server.cache_proxy_tools(server_name, filtered_tools);
            updated_cap.health_status = "healthy".to_string();
            updated_cap.last_error = None;
        }
        Err(discovery_error) => {
            set_mcp_discovery_failure(&mut def, &discovery_error);
            server.clear_proxy_tools(server_name);
            updated_cap.enabled = false;
            updated_cap.health_status = "unhealthy".to_string();
            updated_cap.last_error = Some(discovery_error);
        }
    }

    updated_cap.definition = serde_json::to_string(&def)
        .map_err(|e| format!("Failed to serialize MCP definition: {e}"))?;
    server.with_store_for_scope(target_db, |store| {
        store
            .hub_register(&updated_cap)
            .map_err(|e| format!("hub register updated mcp: {e}"))
    })?;
    Ok(updated_cap)
}

pub(crate) async fn handle_hub_review(
    server: &MemoryServer,
    params: HubReviewParams,
) -> Result<String, String> {
    let review_status = normalize_review_status(&params.review_status)
        .ok_or_else(|| "review_status must be one of: pending, approved, rejected".to_string())?;

    let enabled_override = params.enabled.or(match review_status {
        "approved" => Some(true),
        "rejected" => Some(false),
        _ => None,
    });

    let mut updated = false;
    let mut target_db = "not_found";
    let mut cap: Option<HubCapability> = None;

    if server.has_project_db() {
        let in_project = server.with_project_store_read(|store| {
            store
                .hub_get(&params.id)
                .map(|c| c.is_some())
                .map_err(|e| format!("hub get project: {e}"))
        })?;
        if in_project {
            updated = server.with_project_store(|store| {
                store
                    .hub_set_review(&params.id, review_status, enabled_override)
                    .map_err(|e| format!("hub review project: {e}"))
            })?;
            if updated {
                target_db = "project";
                cap = server.with_project_store_read(|store| {
                    store
                        .hub_get(&params.id)
                        .map_err(|e| format!("hub get project: {e}"))
                })?;
            }
        }
    }

    if !updated {
        updated = server.with_global_store(|store| {
            store
                .hub_set_review(&params.id, review_status, enabled_override)
                .map_err(|e| format!("hub review global: {e}"))
        })?;
        if updated {
            target_db = "global";
            cap = server.with_global_store_read(|store| {
                store
                    .hub_get(&params.id)
                    .map_err(|e| format!("hub get global: {e}"))
            })?;
        }
    }

    if let Some(current_cap) = cap.take() {
        let mut current_cap = current_cap;
        if current_cap.id.starts_with("mcp:") {
            let scope = if target_db == "project" {
                DbScope::Project
            } else {
                DbScope::Global
            };
            current_cap = refresh_mcp_capability_state(server, scope, &current_cap).await?;
        }

        if current_cap.id.starts_with("skill:") {
            if capability_callable(&current_cap) && should_expose_skill_tool(&current_cap) {
                let _ = server.register_skill_tool(&current_cap);
            } else {
                let _ = server.unregister_skill_tool(&current_cap.id);
            }
        }

        cap = Some(current_cap);
    }

    serde_json::to_string(&json!({
        "updated": updated,
        "db": target_db,
        "id": params.id,
        "review_status": review_status,
        "enabled": cap.as_ref().map(|c| c.enabled),
        "health_status": cap.as_ref().map(|c| c.health_status.clone()),
        "callable": cap.as_ref().map(capability_callable),
    }))
    .map_err(|e| format!("serialize: {e}"))
}

pub(crate) async fn handle_hub_set_active_version(
    server: &MemoryServer,
    params: HubSetActiveVersionParams,
) -> Result<String, String> {
    if params.alias_id.trim().is_empty() || params.active_capability_id.trim().is_empty() {
        return Err("alias_id and active_capability_id must be non-empty".to_string());
    }

    let active_cap = server
        .get_capability(&params.active_capability_id)
        .map_err(|e| format!("{e}"))?;

    let target_db = if server.has_project_db()
        && server.with_project_store_read(|store| {
            store
                .hub_get(&params.alias_id)
                .map(|cap| cap.is_some())
                .map_err(|e| format!("hub get project alias: {e}"))
        })? {
        DbScope::Project
    } else {
        DbScope::Global
    };

    server.with_store_for_scope(target_db, |store| {
        store
            .hub_set_active_version_route(&params.alias_id, &params.active_capability_id)
            .map_err(|e| format!("hub route set: {e}"))?;

        if let Some(mut alias_cap) = store
            .hub_get(&params.alias_id)
            .map_err(|e| format!("hub get alias: {e}"))?
        {
            alias_cap.active_version = Some(params.active_capability_id.clone());
            store
                .hub_register(&alias_cap)
                .map_err(|e| format!("hub update alias metadata: {e}"))?;
        }
        Ok(())
    })?;

    serde_json::to_string(&json!({
        "updated": true,
        "db": target_db.as_str(),
        "alias_id": params.alias_id,
        "active_capability_id": params.active_capability_id,
        "active_capability_type": active_cap.cap_type,
    }))
    .map_err(|e| format!("serialize: {e}"))
}

pub(crate) async fn handle_hub_set_enabled(
    server: &MemoryServer,
    params: HubSetEnabledParams,
) -> Result<String, String> {
    let mut updated = false;
    let mut target_db = "not_found";

    if server.has_project_db() {
        let result = server.with_project_store(|store| {
            store
                .hub_set_enabled(&params.id, params.enabled)
                .map_err(|e| format!("Failed: {}", e))
        })?;
        if result {
            updated = true;
            target_db = "project";
        }
    }

    if !updated {
        let result = server.with_global_store(|store| {
            store
                .hub_set_enabled(&params.id, params.enabled)
                .map_err(|e| format!("Failed: {}", e))
        })?;
        if result {
            updated = true;
            target_db = "global";
        }
    }

    if updated {
        if params.id.starts_with("mcp:") {
            if let Ok(cap) = server.get_capability(&params.id) {
                let scope = if target_db == "project" {
                    DbScope::Project
                } else {
                    DbScope::Global
                };
                let _ = refresh_mcp_capability_state(server, scope, &cap).await?;
            } else if let Some(server_name) = params.id.strip_prefix("mcp:") {
                server.clear_proxy_tools(server_name);
            }
        }

        if params.id.starts_with("skill:") {
            if params.enabled {
                if let Ok(cap) = server.get_capability(&params.id) {
                    if should_expose_skill_tool(&cap) {
                        let _ = server.register_skill_tool(&cap);
                    } else {
                        let _ = server.unregister_skill_tool(&cap.id);
                    }
                }
            } else {
                let _ = server.unregister_skill_tool(&params.id);
            }
        }
    }

    serde_json::to_string(&json!({
        "updated": updated,
        "db": target_db,
        "id": params.id,
        "enabled": params.enabled,
    }))
    .map_err(|e| format!("Failed to serialize: {}", e))
}
