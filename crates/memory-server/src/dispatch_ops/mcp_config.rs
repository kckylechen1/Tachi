use super::*;

// ─── MCP config generation ───────────────────────────────────────────────────

/// Generate a temporary MCP config JSON file for the dispatched agent subprocess.
/// Queries the Hub for registered MCP servers and writes a Claude Code / Codex
/// compatible mcpServers config.
///
/// When `inject_tachi` is true, adds a "tachi" entry pointing at the running
/// daemon's stdio transport. When `inject_hub` is true, walks all Hub-registered
/// MCP capabilities and adds them.
///
/// Returns the path to the temp file (caller should clean up after subprocess exits).
pub(super) async fn generate_mcp_config(
    server: &MemoryServer,
    dispatch_id: &str,
    inject_tachi: bool,
    inject_hub: bool,
    tachi_profile: Option<&str>,
    allowed_mcp_servers: &[String],
) -> Result<Option<PathBuf>, String> {
    let mut mcp_servers = serde_json::Map::new();

    if inject_tachi {
        // Point at the Tachi binary in stdio mode
        let mut env = serde_json::Map::new();
        if let Some(profile) = tachi_profile.filter(|s| !s.trim().is_empty()) {
            env.insert("TACHI_PROFILE".to_string(), json!(profile.trim()));
        }
        let mut entry = json!({
            "command": "tachi",
            "args": ["serve"]
        });
        if !env.is_empty() {
            entry["env"] = json!(env);
        }
        mcp_servers.insert("tachi".to_string(), entry);
    }

    if inject_hub {
        // Walk Hub for MCP-type capabilities with a stdio transport definition
        let caps = server
            .with_global_store(|store| {
                store
                    .hub_list(Some("mcp"), true)
                    .map_err(|e| format!("hub list for mcp config: {e}"))
            })
            .unwrap_or_default();

        for cap in caps {
            if !cap.enabled {
                continue;
            }
            let def: serde_json::Value = serde_json::from_str(&cap.definition).unwrap_or_default();
            let transport = def.get("transport").and_then(|t| t.as_str()).unwrap_or("");
            if transport != "stdio" {
                continue;
            }
            let command = match def.get("command").and_then(|c| c.as_str()) {
                Some(c) => c,
                None => continue,
            };
            let args = def
                .get("args")
                .and_then(|a| a.as_array())
                .map(|arr| {
                    arr.iter()
                        .filter_map(|v| v.as_str().map(String::from))
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default();

            // Derive server key from capability id: "mcp:context7" → "context7"
            let key = cap.id.strip_prefix("mcp:").unwrap_or(&cap.id).to_string();
            if !allowed_mcp_servers.is_empty()
                && !allowed_mcp_servers
                    .iter()
                    .any(|allowed| allowed == &key || allowed == &cap.id)
            {
                continue;
            }

            let mut entry = json!({ "command": command });
            if !args.is_empty() {
                entry["args"] = json!(args);
            }
            mcp_servers.insert(key, entry);
        }
    }

    if mcp_servers.is_empty() {
        return Ok(None);
    }

    let config = json!({ "mcpServers": mcp_servers });

    // Write to temp file under $TACHI_HOME/tmp or $HOME/.tachi/tmp
    let tmp_dir = if let Ok(home) = std::env::var("TACHI_HOME") {
        PathBuf::from(home)
    } else if let Ok(home) = std::env::var("HOME") {
        PathBuf::from(home).join(".tachi")
    } else {
        std::env::temp_dir().join("tachi")
    };
    let tmp_dir = tmp_dir.join("tmp");
    std::fs::create_dir_all(&tmp_dir)
        .map_err(|e| format!("Failed to create tmp dir for MCP config: {e}"))?;

    let config_path = tmp_dir.join(format!("dispatch-{dispatch_id}-mcp.json"));
    let config_str = serde_json::to_string_pretty(&config)
        .map_err(|e| format!("Failed to serialize MCP config: {e}"))?;
    std::fs::write(&config_path, config_str)
        .map_err(|e| format!("Failed to write MCP config: {e}"))?;

    // Restrict the temp config to owner-only access; it may contain MCP
    // command definitions, environment values, or capability metadata.
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&config_path, std::fs::Permissions::from_mode(0o600))
            .map_err(|e| format!("Failed to set MCP config permissions: {e}"))?;
    }

    Ok(Some(config_path))
}
