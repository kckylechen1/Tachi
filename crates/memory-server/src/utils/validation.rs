pub(crate) fn query_limit(limit: usize) -> usize {
    limit.clamp(1, 500)
}

pub(crate) fn is_shell_env_name(name: &str) -> bool {
    let mut chars = name.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    if !(first == '_' || first.is_ascii_alphabetic()) {
        return false;
    }
    chars.all(|c| c == '_' || c.is_ascii_alphanumeric())
}

pub(crate) fn normalize_supported_values<'a>(
    raw_values: &[String],
    supported: &'a [&'a str],
    value_label: &str,
) -> Result<Vec<&'a str>, String> {
    if raw_values.is_empty() {
        return Ok(supported.to_vec());
    }

    let mut out = Vec::new();
    for raw in raw_values {
        let normalized = raw.trim().to_ascii_lowercase();
        let Some(supported_value) = supported
            .iter()
            .copied()
            .find(|candidate| *candidate == normalized)
        else {
            return Err(format!(
                "unsupported {value_label} '{raw}'. Supported: {}",
                supported.join(", ")
            ));
        };
        if !out.contains(&supported_value) {
            out.push(supported_value);
        }
    }
    Ok(out)
}

/// Check if a command is in the trusted allowlist for MCP server spawning.
///
/// Returns true when the command basename looks like an interpreter, shell, or
/// package runner that must not be auto-approved for MCP registration.
fn is_mcp_interpreter_basename(basename: &str) -> bool {
    const INTERPRETER_BASENAMES: &[&str] = &[
        "npm", "npx", "yarn", "pnpm", "node", "bun", "deno", "python", "python3", "pip", "uv",
        "cargo", "rustup", "sh", "bash", "zsh", "fish", "ruby", "perl", "php",
    ];

    INTERPRETER_BASENAMES.iter().any(|&interp| {
        if basename == interp {
            return true;
        }
        if let Some(suffix) = basename.strip_prefix(interp) {
            suffix
                .chars()
                .next()
                .is_none_or(|c| c.is_ascii_digit() || matches!(c, '.' | '-' | '@' | '_'))
        } else {
            false
        }
    })
}

/// Interpreters and package runners (node, python, npx, bun, deno, uv,
/// cargo, rustup, etc.) are intentionally NOT auto-approved for MCP
/// registration; they require capability-level approval. Only
/// container/platform runtimes and explicitly known binaries are trusted by
/// path or basename for auto-enabled stdio MCP servers.
pub(crate) fn is_trusted_mcp_command(cmd: &str) -> bool {
    let basename = std::path::Path::new(cmd)
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or(cmd);

    if is_mcp_interpreter_basename(basename) {
        return false;
    }

    const TRUSTED_BASENAMES: &[&str] = &["docker", "podman", "tachi", "opencode"];

    if TRUSTED_BASENAMES.contains(&basename) {
        return true;
    }

    // Allow absolute paths under Homebrew and local package managers.
    const TRUSTED_PREFIXES: &[&str] = &["/opt/homebrew/", "/usr/local/bin/"];

    for prefix in TRUSTED_PREFIXES {
        if cmd.starts_with(prefix) {
            return true;
        }
    }

    // Allow paths under user's home .cargo/bin, .local/bin, .nvm, .bun
    if let Ok(home) = std::env::var("HOME") {
        let home_prefixes = [
            format!("{}/.cargo/bin/", home),
            format!("{}/.local/bin/", home),
            format!("{}/.nvm/", home),
            format!("{}/.bun/bin/", home),
        ];
        for prefix in &home_prefixes {
            if cmd.starts_with(prefix.as_str()) {
                return true;
            }
        }
    }

    false
}

/// Trusted command list for `agent=custom` dispatch.
///
/// This retains interpreters because the command is caller-supplied: the
/// caller already decided to run it. MCP auto-registration is the stricter
/// path (see [`is_trusted_mcp_command`]).
pub(crate) fn is_trusted_command(cmd: &str) -> bool {
    let basename = std::path::Path::new(cmd)
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or(cmd);

    const TRUSTED_BASENAMES: &[&str] = &[
        "npx", "node", "bun", "deno", "python3", "python", "uv", "cargo", "rustup", "docker",
        "podman", "tachi", "opencode", "acpx",
    ];

    if TRUSTED_BASENAMES.contains(&basename) {
        return true;
    }

    // Allow absolute paths under Homebrew, nvm, cargo, common bin dirs
    const TRUSTED_PREFIXES: &[&str] = &["/opt/homebrew/", "/usr/local/bin/", "/usr/bin/", "/bin/"];

    for prefix in TRUSTED_PREFIXES {
        if cmd.starts_with(prefix) {
            return true;
        }
    }

    // Allow paths under user's home .cargo/bin, .local/bin, .nvm
    if let Ok(home) = std::env::var("HOME") {
        let home_prefixes = [
            format!("{}/.cargo/bin/", home),
            format!("{}/.local/bin/", home),
            format!("{}/.nvm/", home),
            format!("{}/.bun/bin/", home),
        ];
        for prefix in &home_prefixes {
            if cmd.starts_with(prefix.as_str()) {
                return true;
            }
        }
    }

    false
}
