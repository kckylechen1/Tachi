use crate::mcp_proxy::{set_mcp_discovery_failure, set_mcp_discovery_success};
use crate::tool_params::{HubReviewParams, HubSetEnabledParams};
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
///   the upstream proxy checks before invoking an MCP tool.
/// - **Discovery-refresh gate** (`mcp_discovery_eligible`, above): whether we
///   are even *allowed to attempt* discovery right now. It deliberately
///   excludes `discovery_status`, because requiring "already discovered" as
///   a precondition to "go discover" is a deadlock — no approved+enabled MCP
///   would ever reach its first discovery, execution would stay permanently
///   fail-closed, and registration/quick-add flows would be bricked.
pub(crate) async fn refresh_mcp_capability_state(
    server: &MemoryServer,
    target_db: DbScope,
    cap: &HubCapability,
    reauthorized: bool,
) -> Result<HubCapability, String> {
    let Some(_) = cap.id.strip_prefix("mcp:") else {
        return Ok(cap.clone());
    };

    // A completed owner OAuth login can probe a credential-related open circuit;
    // it never enables a disabled or unapproved connection.
    if !mcp_discovery_eligible(cap)
        && !(reauthorized && cap.enabled && review_status_allows_call(&cap.review_status))
    {
        return Ok(cap.clone());
    }

    let mut updated_cap = cap.clone();
    let mut def: serde_json::Value = serde_json::from_str(&cap.definition)
        .map_err(|e| format!("invalid mcp definition JSON: {e}"))?;

    match server.discover_mcp_tools(&cap.id, &def).await {
        Ok(tools) => {
            set_mcp_discovery_success(&mut def, &tools);
            updated_cap.health_status = "healthy".to_string();
            updated_cap.last_error = None;
        }
        Err(discovery_error) => {
            set_mcp_discovery_failure(&mut def, &discovery_error);
            updated_cap.health_status = "unhealthy".to_string();
            updated_cap.last_error = Some(discovery_error);
        }
    }

    updated_cap.definition = serde_json::to_string(&def)
        .map_err(|e| format!("Failed to serialize MCP definition: {e}"))?;
    server.with_store_for_scope(target_db, |store| {
        let published = store
            .hub_publish_mcp_discovery(
                cap,
                &updated_cap.definition,
                updated_cap.last_error.as_deref(),
            )
            .map_err(|e| format!("save MCP discovery: {e}"))?;
        if !published {
            return Err(
                "MCP connection or governance changed during discovery; retry refresh".into(),
            );
        }
        store
            .hub_get(&cap.id)
            .map_err(|e| format!("reload MCP discovery: {e}"))?
            .ok_or_else(|| "MCP was removed during discovery".into())
    })
}

pub(crate) async fn handle_hub_review(
    server: &MemoryServer,
    params: HubReviewParams,
) -> Result<String, String> {
    let review_status = normalize_review_status(&params.review_status)
        .ok_or_else(|| "review_status must be one of: pending, approved, rejected".to_string())?;
    if crate::builtins::is_retired_builtin_capability_id(&params.id)
        && (review_status != "rejected" || params.enabled == Some(true))
    {
        return Err(format!(
            "Capability '{}' is retired; only review_status='rejected' with enabled=false or omitted is allowed.",
            params.id
        ));
    }

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
            current_cap = refresh_mcp_capability_state(server, scope, &current_cap, false).await?;
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

pub(crate) async fn handle_hub_set_enabled(
    server: &MemoryServer,
    params: HubSetEnabledParams,
) -> Result<String, String> {
    if params.enabled && crate::builtins::is_retired_builtin_capability_id(&params.id) {
        return Err(format!(
            "Capability '{}' is retired and cannot be enabled.",
            params.id
        ));
    }
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
                let _ = refresh_mcp_capability_state(server, scope, &cap, false).await?;
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
