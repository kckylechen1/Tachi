use crate::tool_params::{AuditLogParams, HubCallParams, HubDisconnectParams};
use crate::utils::lock_or_recover;
use crate::MemoryServer;
use serde_json::json;
use tachi_hub::{health_status_allows_call, review_status_allows_call};

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
