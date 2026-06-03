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
    DISPATCH_AGENTS.iter().find(|def| {
        def.name == norm || def.aliases.iter().any(|alias| *alias == norm)
    })
}

pub(crate) fn dispatch_agent_help_list() -> String {
    let names: Vec<_> = DISPATCH_AGENTS.iter().map(|a| a.name).collect();
    format!("{}. Use 'custom' for ad-hoc commands.", names.join(", "))
}

pub(crate) fn mcp_inject_supported(def: &DispatchAgentDef) -> bool {
    matches!(def.mcp, DispatchMcpSupport::JsonFile)
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
    fn mcp_policy_matches_fleet() {
        assert!(mcp_inject_supported(resolve_dispatch_agent("claude").unwrap()));
        assert!(mcp_inject_supported(resolve_dispatch_agent("grok").unwrap()));
        assert!(!mcp_inject_supported(resolve_dispatch_agent("codex").unwrap()));
        assert!(!mcp_inject_supported(resolve_dispatch_agent("kimi").unwrap()));
    }
}