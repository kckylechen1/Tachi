use super::*;

fn now() -> String {
    Utc::now().to_rfc3339()
}

pub(super) fn make_skill_capability(
    id: &str,
    name: &str,
    description: &str,
    definition: Value,
) -> Result<HubCapability, String> {
    let timestamp = now();
    Ok(HubCapability {
        id: id.to_string(),
        cap_type: "skill".to_string(),
        name: name.to_string(),
        version: 1,
        description: description.to_string(),
        definition: serde_json::to_string(&definition)
            .map_err(|e| format!("serialize builtin skill {id}: {e}"))?,
        enabled: true,
        review_status: "approved".to_string(),
        health_status: "healthy".to_string(),
        last_error: None,
        last_success_at: None,
        last_failure_at: None,
        fail_streak: 0,
        active_version: None,
        exposure_mode: "direct".to_string(),
        uses: 0,
        successes: 0,
        failures: 0,
        avg_rating: 0.0,
        last_used: None,
        created_at: timestamp.clone(),
        updated_at: timestamp,
    })
}

pub(super) fn make_mcp_capability(
    id: &str,
    name: &str,
    description: &str,
    url: &str,
    auto_ingest: bool,
    ingest_domain: Option<&str>,
    ingest_path_prefix: Option<&str>,
) -> Result<HubCapability, String> {
    let timestamp = now();
    let definition = json!({
        "transport": "streamable-http",
        "url": url,
        "auth": {
            "type": "bearer",
            "token": "ZAI_API_KEY|BIGMODEL_API_KEY|REASONING_API_KEY"
        },
        "tool_exposure": "gateway",
        "policy": {
            "visibility": "discoverable"
        },
        "auto_ingest": auto_ingest,
        "ingest_scope": "global",
        "ingest_domain": ingest_domain,
        "ingest_path_prefix": ingest_path_prefix,
        "startup_timeout_ms": 10_000,
        "tool_timeout_ms": 30_000,
        "max_concurrency": 2,
        "tags": ["builtin", "bigmodel", "mcp"]
    });

    Ok(HubCapability {
        id: id.to_string(),
        cap_type: "mcp".to_string(),
        name: name.to_string(),
        version: 1,
        description: description.to_string(),
        definition: serde_json::to_string(&definition)
            .map_err(|e| format!("serialize builtin MCP {id}: {e}"))?,
        enabled: true,
        review_status: "approved".to_string(),
        health_status: "healthy".to_string(),
        last_error: None,
        last_success_at: None,
        last_failure_at: None,
        fail_streak: 0,
        active_version: None,
        exposure_mode: "gateway".to_string(),
        uses: 0,
        successes: 0,
        failures: 0,
        avg_rating: 0.0,
        last_used: None,
        created_at: timestamp.clone(),
        updated_at: timestamp,
    })
}

pub(super) fn make_local_mcp_capability(
    id: &str,
    name: &str,
    description: &str,
    command: &str,
    args: &[&str],
    env: Value,
    auto_ingest: bool,
) -> Result<HubCapability, String> {
    let timestamp = now();
    let definition = json!({
        "transport": "stdio",
        "command": command,
        "args": args,
        "env": env,
        "tool_exposure": "gateway",
        "policy": {
            "visibility": "discoverable"
        },
        "auto_ingest": auto_ingest,
        "ingest_scope": "global",
        "startup_timeout_ms": 20_000,
        "tool_timeout_ms": 60_000,
        "max_concurrency": 1,
        "tags": ["builtin", "bigmodel", "mcp", "local"]
    });

    Ok(HubCapability {
        id: id.to_string(),
        cap_type: "mcp".to_string(),
        name: name.to_string(),
        version: 1,
        description: description.to_string(),
        definition: serde_json::to_string(&definition)
            .map_err(|e| format!("serialize builtin local MCP {id}: {e}"))?,
        enabled: true,
        review_status: "approved".to_string(),
        health_status: "healthy".to_string(),
        last_error: None,
        last_success_at: None,
        last_failure_at: None,
        fail_streak: 0,
        active_version: None,
        exposure_mode: "gateway".to_string(),
        uses: 0,
        successes: 0,
        failures: 0,
        avg_rating: 0.0,
        last_used: None,
        created_at: timestamp.clone(),
        updated_at: timestamp,
    })
}
