use crate::MemoryServer;
use serde_json::json;
use std::path::PathBuf;

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
    agent_seat: Option<&str>,
    allowed_mcp_servers: &[String],
) -> Result<Option<PathBuf>, String> {
    let mut mcp_servers = serde_json::Map::new();

    if inject_tachi {
        // Point at the Tachi binary in stdio mode
        let mut env = serde_json::Map::new();
        if let Some(profile) = tachi_profile.filter(|s| !s.trim().is_empty()) {
            env.insert("TACHI_PROFILE".to_string(), json!(profile.trim()));
        }
        // Round-2 fix (codex final review of #964/PR #1003, BUG CP2):
        // `TACHI_PROFILE` is a tool-profile selector (worker/delegate/
        // standard/codex), not a seat identity — a leader dispatched with
        // TACHI_PROFILE=standard would otherwise resolve as a named seat
        // (not None/leader) and lose its own broadcasts. `TACHI_AGENT_SEAT`
        // is a dedicated, separate env var carrying the actual per-worker
        // seat name (the dispatch's `profile`/`agent` identity), read by
        // `sticky_ops::identity::resolve_caller_agent_id`'s env fallback.
        // Every tachi-dispatched worker gets a real seat identity this way,
        // independent of whatever tool profile it was also given.
        if let Some(seat) = agent_seat.filter(|s| !s.trim().is_empty()) {
            env.insert("TACHI_AGENT_SEAT".to_string(), json!(seat.trim()));
        }
        // #1251: stamp this child's recursion depth = the dispatching session's
        // resolved depth + 1 into the worker's `tachi serve` env. The child's
        // stdio proxy reads it back from its own env (ENV_DISPATCH_DEPTH) and
        // re-emits it as HEADER_DISPATCH_DEPTH on every daemon call, so the
        // recursion gate consults the SESSION depth over the wire — never the
        // daemon's own env (always 0). `server` here is the per-session clone
        // that carries the dispatching session's identity, so its depth is the
        // PARENT depth. Saturating add so a pathological parent depth can only
        // push the child further past the limit, never wrap to a small value.
        let parent_depth = crate::session_identity::resolve_dispatch_depth(
            server.session_dispatch_depth().as_deref(),
            crate::session_identity::MAX_DISPATCH_DEPTH,
        );
        let child_depth = parent_depth.saturating_add(1);
        env.insert(
            crate::session_identity::ENV_DISPATCH_DEPTH.to_string(),
            json!(child_depth.to_string()),
        );
        let mut entry = json!({
            "command": "tachi",
            "args": ["serve"]
        });
        if !env.is_empty() {
            entry["env"] = json!(env);
        }
        // NOTE (doubled-prefix caveat): the server key here ("tachi") combines
        // with each host's MCP tool-namespacing convention. The tools exported
        // by `tachi serve` are already named `tachi_*` (e.g. `tachi_briefing`,
        // `tachi_memory`), so the exposed name depends on the host backend:
        //   - claude / grok / codex (`--mcp-config`): namespace as
        //     `mcp__<serverKey>__<tool>` → `mcp__tachi__tachi_briefing`.
        //   - opencode (acpx transport): namespaces as `<serverKey>_<tool>` →
        //     `tachi_tachi_briefing` (the doubled prefix observed live as
        //     "tachi_tachi_briefing Unknown").
        // The worker prompt / `allowed_facades` reference the bare `tachi_*`
        // names (see dispatch_profile.rs and dispatch_ops/prompt.rs `tool_access`),
        // and the same prompt string is shared across all backends, so a single
        // un-prefixed reference cannot match every host. This is NOT fixed here
        // on purpose: stripping the redundant prefix would require renaming the
        // tools exported by `tachi serve` (breaking the already-correct
        // `mcp__tachi__tachi_briefing` exposure on claude and every leader
        // reference), and renaming/altering the server key is opencode-specific
        // behavior that cannot be verified in-repo. The host-correct fix is to
        // make the worker contract reference host-prefixed tool names per
        // backend (opencode → `tachi_tachi_*`, claude → `mcp__tachi__tachi_*`),
        // resolved at prompt-assembly time once `params.agent` is known.
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
