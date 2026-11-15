use crate::tool_params::{SandboxCheckParams, SandboxExecAuditParams, SandboxSetRuleParams};
use crate::utils::redact_sensitive_value;
use crate::MemoryServer;
use serde_json::json;

pub(crate) async fn handle_sandbox_set_rule(
    server: &MemoryServer,
    params: SandboxSetRuleParams,
) -> Result<String, String> {
    if !["read", "write", "deny"].contains(&params.access_level.as_str()) {
        return Err(format!(
            "Invalid access_level '{}'. Must be: read, write, deny",
            params.access_level
        ));
    }

    server.with_global_store(|store| {
        memcore::db::set_sandbox_rule(
            store.connection(),
            &params.agent_role,
            &params.path_pattern,
            &params.access_level,
        )
        .map_err(|e| format!("Failed to set sandbox rule: {e}"))
    })?;

    serde_json::to_string(&json!({
        "status": "ok",
        "agent_role": params.agent_role,
        "path_pattern": params.path_pattern,
        "access_level": params.access_level,
    }))
    .map_err(|e| format!("serialize: {e}"))
}

pub(crate) async fn handle_sandbox_check(
    server: &MemoryServer,
    params: SandboxCheckParams,
) -> Result<String, String> {
    if !["read", "write"].contains(&params.operation.as_str()) {
        return Err(format!(
            "Invalid operation '{}'. Must be: read, write",
            params.operation
        ));
    }

    let (allowed, matching_rule) = server.with_global_store(|store| {
        memcore::db::check_sandbox_access(
            store.connection(),
            &params.agent_role,
            &params.path,
            &params.operation,
        )
        .map_err(|e| format!("Failed to check sandbox access: {e}"))
    })?;

    serde_json::to_string(&json!({
        "agent_role": params.agent_role,
        "path": params.path,
        "operation": params.operation,
        "allowed": allowed,
        "matching_rule": matching_rule,
    }))
    .map_err(|e| format!("serialize: {e}"))
}

pub(crate) async fn handle_sandbox_exec_audit(
    server: &MemoryServer,
    params: SandboxExecAuditParams,
) -> Result<String, String> {
    let rows = server.with_global_store(|store| {
        store
            .list_sandbox_exec_audit(
                params.capability_id.as_deref(),
                params.stage.as_deref(),
                params.decision.as_deref(),
                params.limit,
            )
            .map_err(|e| format!("Failed to list sandbox exec audit: {e}"))
    })?;

    let mut output = json!({
        "count": rows.len(),
        "items": rows,
    });
    redact_sensitive_value(&mut output);

    serde_json::to_string(&output).map_err(|e| format!("serialize: {e}"))
}
