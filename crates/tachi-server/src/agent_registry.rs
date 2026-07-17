//! Static registry for `tachi_dispatch` CLI workers (Phase 1 fleet).
//!
//! Canonical policy: `docs/engineering/architecture/agent-fleet.md`.

#[cfg(test)]
pub(crate) use tachi_dispatch::select_agent_for_intent;
pub(crate) use tachi_dispatch::{
    dispatch_agent_help_list, fallback_chain, mcp_inject_supported, normalize_dispatch_agent_name,
    resolve_dispatch_agent, select_agent_for_task, DISPATCH_AGENTS,
};

pub(crate) fn registry_json(server: &crate::MemoryServer) -> Result<serde_json::Value, String> {
    Ok(serde_json::json!({
        "agents": DISPATCH_AGENTS.iter().map(|a| serde_json::json!({
            "name": a.name,
            "display_name": a.display_name,
            "aliases": a.aliases,
            "binary": a.binary,
            "default_timeout_secs": a.default_timeout_secs,
            "mcp_inject": mcp_inject_supported(a),
        })).collect::<Vec<_>>(),
        // tachi#1173 item 2 slimmed the default `tachi_task(action='profiles')`
        // shape; this registry's `dispatch_profiles` entry is an established
        // full-card consumer (`tachi_agents` list/profiles action), so it opts
        // back into the pre-#1173 verbose shape explicitly rather than
        // inheriting the new slim default.
        "dispatch_profiles": crate::dispatch_profile::dispatch_profiles_json_for_server(server, true)?
            .get("dispatch_profiles")
            .cloned()
            .unwrap_or_else(|| serde_json::json!([])),
        "fleet_policy": "docs/engineering/architecture/agent-fleet.md",
    }))
}

pub(crate) async fn handle_agents(
    server: &crate::MemoryServer,
    params: crate::tool_params::TachiAgentsParams,
) -> Result<String, String> {
    let action = params.action.trim().to_ascii_lowercase();
    match action.as_str() {
        "list" | "profiles" => {
            serde_json::to_string(&registry_json(server)?).map_err(|e| format!("serialize: {e}"))
        }
        "select" => {
            let intent = params.intent.as_deref().unwrap_or("other");
            let task = params.task.as_deref().unwrap_or("");
            let primary = select_agent_for_task(intent, task);
            let chain = fallback_chain(primary);
            serde_json::to_string(&serde_json::json!({
                "intent": intent,
                "agent": primary,
                "fallback_chain": chain,
            }))
            .map_err(|e| format!("serialize: {e}"))
        }
        other => Err(format!(
            "Invalid agents action '{other}'. Use list or select."
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolve_canonical_and_aliases() {
        assert_eq!(resolve_dispatch_agent("claude").unwrap().name, "claude");
        assert_eq!(
            resolve_dispatch_agent("claude-code").unwrap().name,
            "claude"
        );
        assert_eq!(resolve_dispatch_agent("CODEX").unwrap().name, "codex");
        assert_eq!(resolve_dispatch_agent("openai").unwrap().name, "codex");
        assert_eq!(resolve_dispatch_agent("grok-cli").unwrap().name, "grok");
        assert_eq!(resolve_dispatch_agent("moonshot").unwrap().name, "kimi");
        assert!(resolve_dispatch_agent("gemini").is_none());
    }

    #[test]
    fn normalize_dispatch_agent_name_handles_adapters_and_registered_agents() {
        assert_eq!(
            normalize_dispatch_agent_name(" claude-code ").as_deref(),
            Some("claude")
        );
        assert_eq!(
            normalize_dispatch_agent_name("OPENCODE").as_deref(),
            Some("opencode")
        );
        assert_eq!(
            normalize_dispatch_agent_name("custom").as_deref(),
            Some("custom")
        );
        assert!(normalize_dispatch_agent_name("gemini").is_none());
        assert!(normalize_dispatch_agent_name(" ").is_none());
    }

    #[test]
    fn intent_routes_to_fleet_agent() {
        assert_eq!(select_agent_for_intent("review_request"), "kimi");
        assert_eq!(select_agent_for_intent("refactor_request"), "codex");
    }

    #[test]
    fn fallback_chain_excludes_primary_agent() {
        for primary in ["claude", "codex", "grok", "kimi"] {
            assert!(
                !fallback_chain(primary).contains(&primary),
                "fallback chain should not retry primary agent {primary}"
            );
        }
    }

    #[test]
    fn mcp_policy_matches_fleet() {
        assert!(mcp_inject_supported(
            resolve_dispatch_agent("claude").unwrap()
        ));
        assert!(mcp_inject_supported(
            resolve_dispatch_agent("grok").unwrap()
        ));
        assert!(!mcp_inject_supported(
            resolve_dispatch_agent("codex").unwrap()
        ));
        assert!(!mcp_inject_supported(
            resolve_dispatch_agent("kimi").unwrap()
        ));
    }
}
