//! Static registry for Tachi dispatch CLI workers.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DispatchMcpSupport {
    /// `--mcp-config <file>` compatible.
    JsonFile,
    /// Per-run `-c mcp_servers.<name>=<TOML>` overrides.
    TomlOverrides,
    /// No runtime MCP injection from Tachi.
    Unsupported,
}

#[derive(Debug, Clone, Copy)]
pub struct DispatchAgentDef {
    pub name: &'static str,
    pub display_name: &'static str,
    pub aliases: &'static [&'static str],
    pub binary: &'static str,
    pub default_timeout_secs: u64,
    pub mcp: DispatchMcpSupport,
}

pub const DISPATCH_AGENTS: &[DispatchAgentDef] = &[
    DispatchAgentDef {
        name: "dsh",
        display_name: "DeepSeek Harness",
        aliases: &["deepseek-harness"],
        binary: "dsh",
        default_timeout_secs: 600,
        mcp: DispatchMcpSupport::Unsupported,
    },
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
        mcp: DispatchMcpSupport::TomlOverrides,
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

/// Resolve a dispatch `agent` string to a registry entry.
pub fn resolve_dispatch_agent(raw: &str) -> Option<&'static DispatchAgentDef> {
    let norm = raw.trim().to_ascii_lowercase();
    if norm.is_empty() {
        return None;
    }
    DISPATCH_AGENTS
        .iter()
        .find(|def| def.name == norm || def.aliases.iter().any(|alias| *alias == norm))
}

pub fn dispatch_agent_help_list() -> String {
    let names: Vec<_> = DISPATCH_AGENTS.iter().map(|a| a.name).collect();
    format!(
        "{}. Use 'opencode' for typed OpenCode profiles or 'custom' for ad-hoc commands.",
        names.join(", ")
    )
}

pub fn normalize_dispatch_agent_name(raw: &str) -> Option<String> {
    let norm = raw.trim();
    if norm.is_empty() {
        None
    } else if norm.eq_ignore_ascii_case("custom") {
        Some("custom".to_string())
    } else if norm.eq_ignore_ascii_case("opencode") {
        Some("opencode".to_string())
    } else {
        resolve_dispatch_agent(norm).map(|def| def.name.to_string())
    }
}

pub fn mcp_inject_supported(def: &DispatchAgentDef) -> bool {
    matches!(
        def.mcp,
        DispatchMcpSupport::JsonFile | DispatchMcpSupport::TomlOverrides
    )
}

pub fn fallback_chain(primary: &str) -> &'static [&'static str] {
    match primary {
        "claude" => &["grok", "codex"],
        "codex" => &["claude"],
        "grok" => &["claude"],
        "kimi" => &["claude"],
        _ => &["claude", "codex", "grok", "kimi"],
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
        assert!(mcp_inject_supported(
            resolve_dispatch_agent("codex").unwrap()
        ));
        assert!(!mcp_inject_supported(
            resolve_dispatch_agent("kimi").unwrap()
        ));
    }
}
