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

/// Resolve only the operator-installed stdio adapter for a fixed native
/// profile. No package installer, global legacy transport override or fallback.
pub fn native_session_command(profile: &str) -> Result<Vec<String>, String> {
    let native = crate::native_session_profile(profile)
        .ok_or_else(|| format!("profile '{profile}' has no native Session contract"))?;
    let (command_env, args_env, default, default_args): (&str, &str, &str, &[&str]) =
        match native.backend {
            "codex" => (
                "TACHI_CODEX_APP_SERVER_COMMAND",
                "TACHI_CODEX_APP_SERVER_ARGS",
                "codex",
                &["app-server"],
            ),
            "opencode" => (
                "TACHI_OPENCODE_ACP_COMMAND",
                "TACHI_OPENCODE_ACP_ARGS",
                "opencode",
                &["acp"],
            ),
            _ => return Err("native profile has no installed adapter binding".into()),
        };
    let command = match std::env::var(command_env) {
        Ok(value) if !value.trim().is_empty() => value.trim().to_string(),
        Ok(_) => return Err(format!("{command_env} must name an installed ACP adapter")),
        Err(std::env::VarError::NotPresent) => default.to_string(),
        Err(error) => return Err(format!("read {command_env}: {error}")),
    };
    if !crate::is_trusted_dispatch_command(&command) {
        return Err(format!(
            "native Session adapter '{command}' is not a trusted executable"
        ));
    }
    let args: Vec<String> = match std::env::var(args_env) {
        Ok(value) => serde_json::from_str(&value).map_err(|error| {
            format!("{args_env} must be a JSON array of argument strings: {error}")
        })?,
        Err(std::env::VarError::NotPresent) => {
            default_args.iter().map(|arg| (*arg).to_string()).collect()
        }
        Err(error) => return Err(format!("read {args_env}: {error}")),
    };
    Ok(std::iter::once(command).chain(args).collect())
}

pub fn native_session_adapter_available(profile: &str) -> bool {
    let Ok(command) = native_session_command(profile) else {
        return false;
    };
    let path = std::path::Path::new(&command[0]);
    let executable = |path: &std::path::Path| {
        let Ok(metadata) = path.metadata() else {
            return false;
        };
        if !metadata.is_file() {
            return false;
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            metadata.permissions().mode() & 0o111 != 0
        }
        #[cfg(not(unix))]
        {
            true
        }
    };
    if path.components().count() > 1 {
        return executable(path);
    }
    std::env::var_os("PATH").is_some_and(|paths| {
        std::env::split_paths(&paths).any(|directory| executable(&directory.join(path)))
    })
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
