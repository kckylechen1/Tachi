use super::*;

const GH_AGENT_ID: &str = "tachi_gh_ops";
const MAX_GH_OUTPUT_CHARS: usize = 50_000;
const GH_ENV_ALLOWLIST: &[&str] = &[
    "PATH",
    "HOME",
    "USERPROFILE",
    "APPDATA",
    "LOCALAPPDATA",
    "XDG_CONFIG_HOME",
    "XDG_CACHE_HOME",
    "XDG_DATA_HOME",
    "SSL_CERT_FILE",
    "SSL_CERT_DIR",
    "REQUESTS_CA_BUNDLE",
    "CURL_CA_BUNDLE",
    "GIT_SSL_CAINFO",
    "HTTP_PROXY",
    "HTTPS_PROXY",
    "ALL_PROXY",
    "NO_PROXY",
    "http_proxy",
    "https_proxy",
    "all_proxy",
    "no_proxy",
    "GIT_CONFIG_GLOBAL",
    "GIT_CONFIG_SYSTEM",
    "SSH_AUTH_SOCK",
    "SYSTEMROOT",
    "WINDIR",
    "COMSPEC",
    "TMP",
    "TEMP",
    "LANG",
    "LC_ALL",
];

pub(in crate::gh_ops) fn validate_repo(repo: &str) -> Result<(), String> {
    let count = repo.matches('/').count();
    if count != 1 {
        return Err(format!(
            "Invalid repo format '{}'. Expected 'owner/repo' with exactly one '/'.",
            repo
        ));
    }
    let parts: Vec<&str> = repo.splitn(2, '/').collect();
    if parts[0].is_empty() || parts[1].is_empty() {
        return Err(format!(
            "Invalid repo format '{}'. Both owner and repo name must be non-empty.",
            repo
        ));
    }
    Ok(())
}

/// Resolve absolute path of `gh` binary. Returns error if not found.
pub(in crate::gh_ops) fn resolve_gh_path() -> Result<String, String> {
    let output = Command::new("which")
        .arg("gh")
        .output()
        .map_err(|e| format!("Failed to locate `gh` CLI: {e}"))?;
    if !output.status.success() {
        return Err("GitHub CLI (`gh`) not found. Install it: https://cli.github.com".into());
    }
    let path = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if path.is_empty() {
        return Err("`gh` CLI path resolved to empty string".into());
    }
    Ok(path)
}

/// Strip sensitive tokens from output text
pub(in crate::gh_ops) fn sanitize_output(text: &str, token: &str) -> String {
    let mut sanitized = text.to_string();
    if !token.is_empty() {
        sanitized = sanitized.replace(token, "[REDACTED]");
    }
    // Also strip common auth header patterns
    let patterns = [
        "x-github-token",
        "Bearer gho_",
        "Bearer ghp_",
        "Bearer github_pat_",
    ];
    for pat in patterns {
        if let Some(pos) = sanitized.to_lowercase().find(&pat.to_lowercase()) {
            // Redact from the pattern to end of line or next whitespace
            if let Some(end) = sanitized[pos..].find(|c: char| c == '\n' || c == '\r') {
                sanitized.replace_range(pos..pos + end, "[REDACTED]");
            }
        }
    }
    sanitized
}

pub(in crate::gh_ops) fn vault_secret_unavailable(err: &str) -> bool {
    err.starts_with("Secret not found: ")
        || err.starts_with("Vault is locked")
        || err.starts_with("Vault auto-locked")
        || err.starts_with("Vault not initialized")
}

pub(in crate::gh_ops) fn env_gh_token() -> Option<String> {
    for key in ["GH_TOKEN", "GITHUB_TOKEN"] {
        if let Ok(value) = std::env::var(key) {
            let trimmed = value.trim();
            if !trimmed.is_empty() {
                return Some(trimmed.to_string());
            }
        }
    }
    None
}

pub(in crate::gh_ops) fn resolve_gh_token(server: &MemoryServer) -> Result<Option<String>, String> {
    match read_unlocked_vault_secret(server, "GH_TOKEN", Some(GH_AGENT_ID), false) {
        Ok(token) => Ok(Some(token)),
        Err(err) if vault_secret_unavailable(&err) => Ok(env_gh_token()),
        Err(err) => Err(err),
    }
}

pub(in crate::gh_ops) fn preserve_gh_env(cmd: &mut Command) {
    for var in GH_ENV_ALLOWLIST {
        if let Ok(val) = std::env::var(var) {
            cmd.env(var, val);
        }
    }
    if std::env::var_os("GITHUB_TOKEN").is_some() && std::env::var_os("GH_TOKEN").is_none() {
        if let Ok(val) = std::env::var("GITHUB_TOKEN") {
            cmd.env("GITHUB_TOKEN", val);
        }
    }
}

/// Build a sanitized Command for `gh` with env_clear + vault token injection
pub(in crate::gh_ops) fn build_gh_command(
    server: &MemoryServer,
) -> Result<(Command, String), String> {
    let gh_path = resolve_gh_path()?;
    let token = resolve_gh_token(server)?;

    let mut cmd = Command::new(&gh_path);
    cmd.env_clear();

    preserve_gh_env(&mut cmd);
    if let Some(token) = token.as_deref() {
        cmd.env("GH_TOKEN", token);
    }
    cmd.env("GH_PROMPT_DISABLED", "1");
    cmd.env("NO_COLOR", "1");

    Ok((cmd, token.unwrap_or_default()))
}

/// Execute a gh command and return sanitized output, truncated to MAX_GH_OUTPUT_CHARS
pub(in crate::gh_ops) fn run_gh(mut cmd: Command, token: &str) -> Result<String, String> {
    let output = cmd
        .output()
        .map_err(|e| format!("Failed to execute `gh`: {e}"))?;

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);

    let sanitized_stdout = sanitize_output(&stdout, token);
    let sanitized_stderr = sanitize_output(&stderr, token);

    if !output.status.success() {
        return Err(format!(
            "gh failed (exit {}): {}",
            output.status.code().unwrap_or(-1),
            sanitized_stderr.chars().take(1000).collect::<String>()
        ));
    }

    // Truncate large output
    let result = if sanitized_stdout.len() > MAX_GH_OUTPUT_CHARS {
        let truncated: String = sanitized_stdout.chars().take(MAX_GH_OUTPUT_CHARS).collect();
        format!(
            "{}\n\n[truncated: {} total chars]",
            truncated,
            sanitized_stdout.len()
        )
    } else {
        sanitized_stdout
    };

    Ok(result)
}

pub(in crate::gh_ops) fn run_gh_json(mut cmd: Command, token: &str) -> Result<String, String> {
    let output = cmd
        .output()
        .map_err(|e| format!("Failed to execute `gh`: {e}"))?;

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);

    let sanitized_stdout = sanitize_output(&stdout, token);
    let sanitized_stderr = sanitize_output(&stderr, token);

    if !output.status.success() {
        return Err(format!(
            "gh failed (exit {}): {}",
            output.status.code().unwrap_or(-1),
            sanitized_stderr.chars().take(1000).collect::<String>()
        ));
    }

    Ok(sanitized_stdout)
}
