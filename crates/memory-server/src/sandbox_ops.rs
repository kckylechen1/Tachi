use super::*;
use crate::utils::redact_sensitive_value;

pub(super) async fn handle_sandbox_set_rule(
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
        memory_core::db::set_sandbox_rule(
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

pub(super) async fn handle_sandbox_check(
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
        memory_core::db::check_sandbox_access(
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

pub(super) async fn handle_sandbox_set_policy(
    server: &MemoryServer,
    params: SandboxSetPolicyParams,
) -> Result<String, String> {
    if !["process", "wasm"].contains(&params.runtime_type.as_str()) {
        return Err(format!(
            "Invalid runtime_type '{}'. Must be: process, wasm",
            params.runtime_type
        ));
    }
    if params.max_startup_ms == 0 || params.max_tool_ms == 0 {
        return Err("max_startup_ms and max_tool_ms must be >= 1".to_string());
    }
    if params.max_concurrency == 0 {
        return Err("max_concurrency must be >= 1".to_string());
    }

    let env_allowlist_json =
        serde_json::to_string(&params.env_allowlist).map_err(|e| format!("serialize env: {e}"))?;
    let fs_read_roots_json = serde_json::to_string(&params.fs_read_roots)
        .map_err(|e| format!("serialize fs_read: {e}"))?;
    let fs_write_roots_json = serde_json::to_string(&params.fs_write_roots)
        .map_err(|e| format!("serialize fs_write: {e}"))?;
    let cwd_roots_json = serde_json::to_string(&params.cwd_roots)
        .map_err(|e| format!("serialize cwd_roots: {e}"))?;

    server.with_global_store(|store| {
        store
            .set_sandbox_policy(
                &params.capability_id,
                &params.runtime_type,
                &env_allowlist_json,
                &fs_read_roots_json,
                &fs_write_roots_json,
                &cwd_roots_json,
                params.max_startup_ms,
                params.max_tool_ms,
                params.max_concurrency,
                params.enabled,
            )
            .map_err(|e| format!("Failed to set sandbox policy: {e}"))
    })?;

    serde_json::to_string(&json!({
        "status": "ok",
        "capability_id": params.capability_id,
        "runtime_type": params.runtime_type,
        "max_startup_ms": params.max_startup_ms,
        "max_tool_ms": params.max_tool_ms,
        "max_concurrency": params.max_concurrency,
        "enabled": params.enabled,
    }))
    .map_err(|e| format!("serialize: {e}"))
}

pub(super) async fn handle_sandbox_get_policy(
    server: &MemoryServer,
    params: SandboxGetPolicyParams,
) -> Result<String, String> {
    let policy = server.with_global_store(|store| {
        store
            .get_sandbox_policy(&params.capability_id)
            .map_err(|e| format!("Failed to get sandbox policy: {e}"))
    })?;

    match policy {
        Some(v) => serde_json::to_string(&v).map_err(|e| format!("serialize: {e}")),
        None => serde_json::to_string(&json!({
            "error": "Sandbox policy not found",
            "capability_id": params.capability_id,
        }))
        .map_err(|e| format!("serialize: {e}")),
    }
}

pub(super) async fn handle_sandbox_list_policies(
    server: &MemoryServer,
    params: SandboxListPoliciesParams,
) -> Result<String, String> {
    let policies = server.with_global_store(|store| {
        store
            .list_sandbox_policies(params.enabled_only, params.limit)
            .map_err(|e| format!("Failed to list sandbox policies: {e}"))
    })?;

    serde_json::to_string(&json!({
        "count": policies.len(),
        "policies": policies,
    }))
    .map_err(|e| format!("serialize: {e}"))
}

pub(super) async fn handle_sandbox_exec_audit(
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
