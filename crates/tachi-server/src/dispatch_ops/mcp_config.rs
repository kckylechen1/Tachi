use crate::MemoryServer;
use serde_json::json;
use std::path::PathBuf;

/// Convert the generated host config to ACP's descriptor array. Unsupported
/// entries fail rather than disappearing from an explicitly requested grant.
pub(in crate::dispatch_ops) fn acp_mcp_servers_from_config(
    path: &std::path::Path,
) -> Result<Vec<serde_json::Value>, String> {
    use serde_json::Value;
    let raw = std::fs::read(path).map_err(|error| format!("read ACP MCP config: {error}"))?;
    let config: Value =
        serde_json::from_slice(&raw).map_err(|error| format!("parse ACP MCP config: {error}"))?;
    let servers = config
        .get("mcpServers")
        .and_then(Value::as_object)
        .ok_or("ACP MCP config requires an mcpServers object")?;
    let pairs = |entry: &Value, field: &str| -> Result<Vec<Value>, String> {
        let Some(value) = entry.get(field) else {
            return Ok(Vec::new());
        };
        let map = value
            .as_object()
            .ok_or_else(|| format!("ACP {field} must be a string map"))?;
        map.iter()
            .map(|(name, value)| {
                let value = value
                    .as_str()
                    .ok_or_else(|| format!("ACP {field} values must be strings"))?;
                Ok(json!({"name":name,"value":value}))
            })
            .collect()
    };
    servers.iter().map(|(name, entry)| {
        if entry.get("type").and_then(Value::as_str) == Some("http") {
            let url = entry.get("url").and_then(Value::as_str).ok_or("ACP HTTP entry requires url")?;
            if entry.get("command").is_some() {
                return Err("ACP MCP entry has conflicting transports".into());
            }
            Ok(json!({"type":"http","name":name,"url":url,"headers":pairs(entry,"headers")?}))
        } else {
            if entry.get("url").is_some() || entry.get("type").is_some_and(|kind| kind != "stdio") {
                return Err("unsupported or conflicting ACP MCP transport".into());
            }
            let command = entry.get("command").and_then(Value::as_str).ok_or("unsupported ACP MCP transport")?;
            if !std::path::Path::new(command).is_absolute() {
                return Err(format!("ACP stdio server '{name}' requires an absolute command path in its Hub definition"));
            }
            let args = entry.get("args").cloned().unwrap_or_else(|| json!([]));
            if !args.as_array().is_some_and(|args| args.iter().all(Value::is_string)) {
                return Err("ACP stdio arguments must be strings".into());
            }
            Ok(json!({"name":name,"command":command,"args":args,"env":pairs(entry,"env")?}))
        }
    }).collect()
}

// ─── MCP config generation ───────────────────────────────────────────────────

/// Generate a temporary MCP config JSON file for the dispatched agent subprocess.
/// Queries the Hub for registered MCP servers and writes a Claude Code / Codex
/// compatible mcpServers config.
///
/// When `inject_tachi` is true, adds a "tachi" entry pointing at the running
/// daemon's Streamable HTTP transport. When `inject_hub` is true, walks all Hub-registered
/// MCP capabilities and adds them.
///
/// Returns the path to the temp file (caller should clean up after subprocess exits).
pub(super) async fn generate_mcp_config(
    server: &MemoryServer,
    dispatch_id: &str,
    inject_tachi: bool,
    inject_hub: bool,
    tachi_profile: Option<&str>,
    agent_seat: Option<&str>,
    allowed_mcp_servers: &[String],
) -> Result<Option<PathBuf>, String> {
    let mut mcp_servers = serde_json::Map::new();

    if inject_tachi {
        if server.published_daemon_mcp_url().is_none() {
            let daemon = crate::cli_client::detect_daemon_for_global_db(
                &server.tachi_home_dir(),
                &server.global_db_path_buf(),
            )
            .await
            .ok_or_else(|| "Tachi MCP injection requires a running daemon; start `tachi --daemon --port <fixed-port> serve` first".to_string())?;
            server.set_published_daemon_mcp_url(daemon.url);
        }
        let profile = tachi_profile
            .filter(|value| !value.trim().is_empty())
            .map(str::trim)
            .unwrap_or("delegate");
        let seat = agent_seat
            .filter(|value| !value.trim().is_empty())
            .map(str::trim)
            .unwrap_or(dispatch_id);
        let parent_depth = crate::session_identity::resolve_dispatch_depth(
            server.session_dispatch_depth().as_deref(),
            crate::session_identity::MAX_DISPATCH_DEPTH,
        );
        // A worker has its own seat, but no new AgentIdentity is minted from
        // that label and the parent's principal is never impersonated.
        let entry = server.http_mcp_entry(profile, seat, None, parent_depth.saturating_add(1))?;
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
            // Hub entries must not replace the admitted first-party binding.
            if inject_tachi && key == "tachi" {
                continue;
            }
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

    // Write to temp file under the server-bound Tachi home's tmp dir (#1096
    // leaf-2a: was a private TACHI_HOME/HOME two-key duplicate of the
    // canonical funnel).
    let tmp_dir = server.tachi_home_dir().join("tmp");
    std::fs::create_dir_all(&tmp_dir)
        .map_err(|e| format!("Failed to create tmp dir for MCP config: {e}"))?;

    let config_path = tmp_dir.join(format!("dispatch-{dispatch_id}-mcp.json"));
    let config_str = serde_json::to_string_pretty(&config)
        .map_err(|e| format!("Failed to serialize MCP config: {e}"))?;
    crate::utils::write_owner_only_file_atomic(&config_path, config_str.as_bytes())
        .map_err(|e| format!("Failed to write MCP config: {e}"))?;

    Ok(Some(config_path))
}
