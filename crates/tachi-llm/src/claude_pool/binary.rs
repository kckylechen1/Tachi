use super::*;

pub(super) fn resolve_claude_binary() -> Result<String, String> {
    match std::env::var("CLAUDE_BIN") {
        Ok(raw) => validate_claude_binary_override(&raw),
        Err(_) => Ok("claude".to_string()),
    }
}

pub(super) fn validate_claude_binary_override(raw: &str) -> Result<String, String> {
    let value = raw.trim();
    if value.is_empty() {
        return Err("CLAUDE_BIN is empty".to_string());
    }
    if value.chars().any(char::is_whitespace) {
        return Err("CLAUDE_BIN must be a single executable path without arguments".to_string());
    }
    if value == "claude" {
        return Ok(value.to_string());
    }

    let path = Path::new(value);
    if !path.is_absolute() {
        return Err("CLAUDE_BIN must be 'claude' or an absolute path".to_string());
    }
    if path
        .components()
        .any(|component| matches!(component, Component::ParentDir))
    {
        return Err("CLAUDE_BIN must not contain parent directory components".to_string());
    }
    let file_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| "CLAUDE_BIN must include a valid executable name".to_string())?;
    if file_name == "claude" || file_name.starts_with("claude-") {
        let metadata = std::fs::metadata(path)
            .map_err(|_| "CLAUDE_BIN must point to an existing executable".to_string())?;
        if !metadata.is_file() || !is_executable(&metadata) {
            return Err("CLAUDE_BIN must point to an executable file".to_string());
        }
        Ok(value.to_string())
    } else {
        Err("CLAUDE_BIN executable name must be 'claude' or start with 'claude-'".to_string())
    }
}

fn is_executable(metadata: &std::fs::Metadata) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        metadata.permissions().mode() & 0o111 != 0
    }

    #[cfg(not(unix))]
    {
        true
    }
}
