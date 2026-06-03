//! Static registry for `tachi_dispatch` CLI workers (Phase 1 fleet).
//!
//! Canonical policy: `docs/agent-fleet.md`.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum DispatchMcpSupport {
    /// `--mcp-config <file>` (Claude Code compatible)
    JsonFile,
    /// MCP only via `~/.codex/config.toml`
    TomlSidecar,
    /// No runtime MCP injection from Tachi
    Unsupported,
}

#[derive(Debug, Clone, Copy)]
#[allow(dead_code)] // binary/default_timeout used when wiring runtime capability checks (#155)
pub(crate) struct DispatchAgentDef {
    pub name: &'static str,
    pub display_name: &'static str,
    pub aliases: &'static [&'static str],
    pub binary: &'static str,
    pub default_timeout_secs: u64,
    pub mcp: DispatchMcpSupport,
}

pub(crate) const DISPATCH_AGENTS: &[DispatchAgentDef] = &[
    DispatchAgentDef {
        name: "claude",
        display_name: "Claude Code",
        aliases: &["claude-code", "claude-cli"],
        binary: "claude",
        default_timeout_secs: 600,
        mcp: DispatchMcpSupport::JsonFile,
    },
    DispatchAgentDef {
        name: "codex",
        display_name: "Codex CLI",
        aliases: &["codex-cli", "openai"],
        binary: "codex",
        default_timeout_secs: 600,
        mcp: DispatchMcpSupport::TomlSidecar,
    },
    DispatchAgentDef {
        name: "grok",
        display_name: "Grok Build",
        aliases: &["grok-cli", "xai"],
        binary: "grok",
        default_timeout_secs: 600,
        mcp: DispatchMcpSupport::JsonFile,
    },
    DispatchAgentDef {
        name: "kimi",
        display_name: "Kimi Code",
        aliases: &["kimi-cli", "moonshot"],
        binary: "kimi",
        default_timeout_secs: 600,
        mcp: DispatchMcpSupport::Unsupported,
    },
];

/// Resolve a dispatch `agent` string to a registry entry (canonical name).
pub(crate) fn resolve_dispatch_agent(raw: &str) -> Option<&'static DispatchAgentDef> {
    let norm = raw.trim().to_ascii_lowercase();
    if norm.is_empty() {
        return None;
    }
    DISPATCH_AGENTS
        .iter()
        .find(|def| def.name == norm || def.aliases.iter().any(|alias| *alias == norm))
}

pub(crate) fn dispatch_agent_help_list() -> String {
    let names: Vec<_> = DISPATCH_AGENTS.iter().map(|a| a.name).collect();
    format!("{}. Use 'custom' for ad-hoc commands.", names.join(", "))
}

pub(crate) fn mcp_inject_supported(def: &DispatchAgentDef) -> bool {
    matches!(def.mcp, DispatchMcpSupport::JsonFile)
}

/// Heuristic router (Phase 1) — replaced by classifier when #153 lands.
pub(crate) fn select_agent_for_intent(intent: &str) -> &'static str {
    match intent.trim().to_ascii_lowercase().as_str() {
        "fix_request" => "claude",
        "review_request" => "kimi",
        "plan_request" => "claude",
        "test_request" => "codex",
        "refactor_request" => "codex",
        "explain_request" => "kimi",
        "research_request" => "grok",
        "migration_request" => "codex",
        _ => "claude",
    }
}

/// Keyword assist when intent is `other` (#153 Phase 0 before LoRA classifier).
pub(crate) fn select_agent_for_task(intent: &str, task: &str) -> &'static str {
    let intent_norm = intent.trim().to_ascii_lowercase();
    if intent_norm != "other" && !intent_norm.is_empty() {
        return select_agent_for_intent(intent);
    }
    let lower = task.to_ascii_lowercase();
    if lower.contains("review") || lower.contains("审查") {
        return "kimi";
    }
    if lower.contains("refactor") || lower.contains("重构") {
        return "codex";
    }
    if lower.contains("explain") || lower.contains("解释") {
        return "kimi";
    }
    if lower.contains("plan") || lower.contains("方案") || lower.contains("架构") {
        return "claude";
    }
    "claude"
}

pub(crate) fn fallback_chain(primary: &str) -> &'static [&'static str] {
    match primary {
        "claude" => &["claude", "grok", "codex"],
        "codex" => &["codex", "claude"],
        "grok" => &["grok", "claude"],
        "kimi" => &["kimi", "claude"],
        _ => &["claude", "codex", "grok", "kimi"],
    }
}

pub(crate) fn registry_json() -> serde_json::Value {
    serde_json::json!({
        "agents": DISPATCH_AGENTS.iter().map(|a| serde_json::json!({
            "name": a.name,
            "display_name": a.display_name,
            "aliases": a.aliases,
            "binary": a.binary,
            "default_timeout_secs": a.default_timeout_secs,
            "mcp_inject": mcp_inject_supported(a),
        })).collect::<Vec<_>>(),
        "fleet_policy": "docs/agent-fleet.md",
    })
}

pub(crate) async fn handle_agents(
    _server: &crate::MemoryServer,
    params: crate::tool_params::TachiAgentsParams,
) -> Result<String, String> {
    let action = params.action.trim().to_ascii_lowercase();
    match action.as_str() {
        "list" => serde_json::to_string(&registry_json()).map_err(|e| format!("serialize: {e}")),
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
    fn intent_routes_to_fleet_agent() {
        assert_eq!(select_agent_for_intent("review_request"), "kimi");
        assert_eq!(select_agent_for_intent("refactor_request"), "codex");
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
