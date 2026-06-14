use super::*;

pub(super) fn is_active_global_rule(entry: &MemoryEntry) -> bool {
    entry
        .metadata
        .get("state")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("DRAFT")
        == "ACTIVE"
}

pub(super) fn find_git_root_from(path: impl AsRef<std::path::Path>) -> Option<PathBuf> {
    let mut dir = path.as_ref().canonicalize().ok()?;
    if dir.is_file() {
        dir.pop();
    }
    loop {
        if dir.join(".git").exists() {
            return Some(dir);
        }
        if !dir.pop() {
            return None;
        }
    }
}

pub(super) fn find_git_root() -> Option<PathBuf> {
    find_git_root_from(std::env::current_dir().ok()?)
}

pub(super) fn find_project_git_root() -> Option<PathBuf> {
    for var in [
        "TACHI_PROJECT_ROOT",
        "TACHI_WORKSPACE_ROOT",
        "PROJECT_ROOT",
        "WORKSPACE_ROOT",
        "WORKSPACE",
        "PWD",
    ] {
        let Some(value) = std::env::var_os(var) else {
            continue;
        };
        if value.is_empty() {
            continue;
        }
        if let Some(root) = find_git_root_from(PathBuf::from(value)) {
            return Some(root);
        }
    }
    find_git_root()
}

/// Create or truncate a file with owner-only permissions on Unix.
pub(super) fn write_owner_only_file(path: &std::path::Path, bytes: &[u8]) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("create parent dir: {e}"))?;
    }
    #[cfg(unix)]
    {
        use std::io::Write;
        use std::os::unix::fs::OpenOptionsExt;
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(path)
            .map_err(|e| format!("open {}: {e}", path.display()))?;
        file.write_all(bytes)
            .map_err(|e| format!("write {}: {e}", path.display()))?;
    }
    #[cfg(not(unix))]
    {
        std::fs::write(path, bytes).map_err(|e| format!("write {}: {e}", path.display()))?;
    }
    Ok(())
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
            suffix.chars().next().is_none_or(|c| {
                c.is_ascii_digit() || matches!(c, '.' | '-' | '@' | '_')
            })
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
pub(super) fn is_trusted_mcp_command(cmd: &str) -> bool {
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
pub(super) fn is_trusted_command(cmd: &str) -> bool {
    let basename = std::path::Path::new(cmd)
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or(cmd);

    const TRUSTED_BASENAMES: &[&str] = &[
        "npx", "node", "bun", "deno", "python3", "python", "uv", "cargo", "rustup", "docker",
        "podman", "tachi", "opencode",
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

pub(crate) fn sanitize_safe_path_name(name: &str) -> String {
    let sanitized: String = name
        .trim()
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '.') {
                ch
            } else {
                '_'
            }
        })
        .collect();
    let sanitized = sanitized.trim_matches(|ch| matches!(ch, '.' | '_' | '-'));
    if sanitized.is_empty() {
        "unnamed".to_string()
    } else {
        sanitized.to_string()
    }
}

/// Collapse whitespace and truncate to `limit` chars (including ellipsis).
pub(crate) fn compact_text_line(text: &str, limit: usize) -> String {
    let one_line = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if one_line.chars().count() <= limit {
        one_line
    } else {
        let keep = limit.saturating_sub(3);
        format!("{}...", one_line.chars().take(keep).collect::<String>())
    }
}

pub(super) fn value_to_template_text(v: &Value) -> String {
    if let Some(s) = v.as_str() {
        s.to_string()
    } else {
        v.to_string()
    }
}

fn is_safe_template_arg_key(key: &str) -> bool {
    let mut chars = key.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    (first == '_' || first.is_ascii_alphabetic())
        && chars.all(|c| c == '_' || c == '-' || c.is_ascii_alphanumeric())
}

pub(super) fn render_skill_prompt_template(
    template: &str,
    args: &serde_json::Map<String, Value>,
) -> Result<String, serde_json::Error> {
    let args_json = serde_json::to_string(args)?;
    let mut prompt = template.replace("{{args_json}}", &args_json);
    prompt = prompt.replace("{{args}}", &args_json);

    for (key, value) in args {
        if matches!(key.as_str(), "args" | "args_json" | "input") || !is_safe_template_arg_key(key)
        {
            continue;
        }
        let placeholder = format!("{{{{{key}}}}}");
        prompt = prompt.replace(&placeholder, &value_to_template_text(value));
    }

    if prompt.contains("{{input}}") {
        let input = args
            .get("input")
            .map(value_to_template_text)
            .unwrap_or(args_json);
        prompt = prompt.replace("{{input}}", &input);
    }

    Ok(prompt)
}

pub(super) fn redact_sensitive_value(value: &mut Value) {
    match value {
        Value::Object(map) => {
            for (key, child) in map.iter_mut() {
                if is_sensitive_key(key) {
                    *child = Value::String("[REDACTED]".to_string());
                } else {
                    redact_sensitive_value(child);
                }
            }
        }
        Value::Array(items) => {
            for item in items {
                redact_sensitive_value(item);
            }
        }
        Value::String(s) => {
            *s = redact_sensitive_string(s);
        }
        _ => {}
    }
}

fn is_sensitive_key(key: &str) -> bool {
    let key = key.to_ascii_lowercase();
    [
        "apikey",
        "api_key",
        "token",
        "secret",
        "password",
        "authorization",
    ]
    .iter()
    .any(|needle| key.contains(needle))
}

fn redact_sensitive_string(input: &str) -> String {
    let Some(query_start) = input.find('?') else {
        return redact_inline_secret_markers(input);
    };
    let (prefix, rest) = input.split_at(query_start + 1);
    let (query, suffix) = match rest.find('#') {
        Some(fragment_start) => rest.split_at(fragment_start),
        None => (rest, ""),
    };
    let redacted_query = query
        .split('&')
        .map(|part| {
            let Some((name, _value)) = part.split_once('=') else {
                return part.to_string();
            };
            if is_sensitive_key(name) {
                format!("{name}=[REDACTED]")
            } else {
                part.to_string()
            }
        })
        .collect::<Vec<_>>()
        .join("&");
    redact_inline_secret_markers(&format!("{prefix}{redacted_query}{suffix}"))
}

fn redact_inline_secret_markers(input: &str) -> String {
    let mut out = input.to_string();
    for marker in ["Bearer ", "bearer "] {
        if let Some(start) = out.find(marker) {
            let value_start = start + marker.len();
            let value_end = out[value_start..]
                .find(|ch: char| ch.is_whitespace() || matches!(ch, '"' | '\'' | '&'))
                .map(|idx| value_start + idx)
                .unwrap_or(out.len());
            out.replace_range(value_start..value_end, "[REDACTED]");
        }
    }
    out
}

/// Stable hash function (FNV-1a). Deterministic across Rust toolchain versions,
/// unlike DefaultHasher which uses SipHash with randomized keys.
pub(super) fn stable_hash(input: &str) -> String {
    const FNV_OFFSET: u64 = 0xcbf29ce484222325;
    const FNV_PRIME: u64 = 0x100000001b3;
    let mut hash = FNV_OFFSET;
    for byte in input.as_bytes() {
        hash ^= *byte as u64;
        hash = hash.wrapping_mul(FNV_PRIME);
    }
    format!("{:016x}", hash)
}

pub(super) fn parse_env_bool(name: &str) -> Option<bool> {
    let raw = std::env::var(name).ok()?;
    let normalized = raw.trim().to_ascii_lowercase();
    match normalized.as_str() {
        "1" | "true" | "yes" | "on" => Some(true),
        "0" | "false" | "no" | "off" => Some(false),
        _ => {
            eprintln!("Ignoring invalid {name} value '{raw}' (expected true/false)");
            None
        }
    }
}

pub(super) fn parse_env_u64(name: &str) -> Option<u64> {
    let raw = std::env::var(name).ok()?;
    match raw.trim().parse::<u64>() {
        Ok(value) => Some(value),
        Err(_) => {
            eprintln!("Ignoring invalid {name} value '{raw}' (expected non-negative integer)");
            None
        }
    }
}

pub(super) fn lock_or_recover<'a, T>(
    mutex: &'a StdMutex<T>,
    label: &str,
) -> std::sync::MutexGuard<'a, T> {
    match mutex.lock() {
        Ok(guard) => guard,
        Err(poisoned) => {
            eprintln!("WARNING: mutex poisoned: {label}; recovering with inner state");
            poisoned.into_inner()
        }
    }
}

pub(super) fn read_or_recover<'a, T>(
    rwlock: &'a StdRwLock<T>,
    label: &str,
) -> std::sync::RwLockReadGuard<'a, T> {
    match rwlock.read() {
        Ok(guard) => guard,
        Err(poisoned) => {
            eprintln!("WARNING: rwlock poisoned (read): {label}; recovering with inner state");
            poisoned.into_inner()
        }
    }
}

pub(super) fn write_or_recover<'a, T>(
    rwlock: &'a StdRwLock<T>,
    label: &str,
) -> std::sync::RwLockWriteGuard<'a, T> {
    match rwlock.write() {
        Ok(guard) => guard,
        Err(poisoned) => {
            eprintln!("WARNING: rwlock poisoned (write): {label}; recovering with inner state");
            poisoned.into_inner()
        }
    }
}

#[cfg(test)]
pub(crate) fn global_test_lock() -> &'static std::sync::Mutex<()> {
    static LOCK: std::sync::OnceLock<std::sync::Mutex<()>> = std::sync::OnceLock::new();
    LOCK.get_or_init(|| std::sync::Mutex::new(()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compact_text_line_respects_limit_with_ellipsis() {
        assert_eq!(compact_text_line("hello world", 20), "hello world");
        assert_eq!(
            compact_text_line("one two three four five", 10),
            "one two..."
        );
    }

    #[test]
    fn skill_prompt_template_renders_safe_args_only() {
        let args = serde_json::Map::from_iter([
            ("name".to_string(), json!("Ada")),
            ("input".to_string(), json!("review this")),
        ]);

        let rendered = render_skill_prompt_template(
            "Name={{name}}\nInput={{input}}\nAll={{args_json}}",
            &args,
        )
        .expect("render prompt");

        assert!(rendered.contains("Name=Ada"));
        assert!(rendered.contains("Input=review this"));
        assert!(rendered.contains("\"name\":\"Ada\""));
        assert!(!rendered.contains("\n  \"name\""));
    }

    #[test]
    fn skill_prompt_template_ignores_reserved_and_unsafe_arg_keys() {
        let args = serde_json::Map::from_iter([
            ("args_json".to_string(), json!("replace all args")),
            ("input".to_string(), json!("safe input value")),
            ("name}} {{args_json".to_string(), json!("injected")),
            ("unsafe.key".to_string(), json!("dot value")),
        ]);

        let rendered = render_skill_prompt_template(
            "Args={{args_json}}\nInput={{input}}\nBad={{name}} {{args_json}}\nDot={{unsafe.key}}",
            &args,
        )
        .expect("render prompt");

        assert!(rendered.contains("Input=safe input value"));
        assert!(rendered.contains("\"args_json\":\"replace all args\""));
        assert!(rendered.contains("Bad={{name}}"));
        assert!(rendered.contains("Dot={{unsafe.key}}"));
        assert!(
            !rendered.contains("Bad=injected"),
            "unsafe keys must not synthesize template placeholders: {rendered}"
        );
    }

    #[test]
    fn redact_sensitive_value_redacts_secret_keys_and_url_params() {
        let mut value = json!({
            "definition": {
                "url": "https://example.test/mcp?tavilyApiKey=abc123&safe=ok",
                "headers": {
                    "Authorization": "Bearer secret-token"
                }
            }
        });

        redact_sensitive_value(&mut value);

        assert_eq!(
            value["definition"]["url"],
            json!("https://example.test/mcp?tavilyApiKey=[REDACTED]&safe=ok")
        );
        assert_eq!(
            value["definition"]["headers"]["Authorization"],
            json!("[REDACTED]")
        );
    }

    #[test]
    fn mcp_interpreters_are_not_auto_approved() {
        let test_cmds = [
            "python3",
            "python",
            "node",
            "npx",
            "bun",
            "deno",
            "uv",
            "cargo",
            "rustup",
            "python3.12",
            "node20",
            "bash",
            "sh",
            "zsh",
            "ruby",
            "perl",
            "php",
        ];
        for cmd in test_cmds {
            assert!(
                !is_trusted_mcp_command(cmd),
                "{cmd} should require capability-level approval for MCP"
            );
            assert!(
                !is_trusted_mcp_command(&format!("/usr/local/bin/{cmd}")),
                "absolute {cmd} should also be rejected for MCP"
            );
        }
    }

    #[test]
    fn dispatch_interpreters_remain_trusted_for_caller_supplied_commands() {
        assert!(is_trusted_command("python3"));
        assert!(is_trusted_command("/usr/local/bin/node"));
    }

    #[test]
    fn mcp_container_and_platform_runtimes_remain_trusted() {
        for cmd in ["docker", "podman", "tachi", "opencode"] {
            assert!(
                is_trusted_mcp_command(cmd),
                "{cmd} should be trusted by basename for MCP"
            );
        }
    }

    #[test]
    fn mcp_trusted_prefixes_still_allow_non_interpreters() {
        assert!(is_trusted_mcp_command("/opt/homebrew/bin/opencode"));
        assert!(!is_trusted_mcp_command("/usr/bin/python3"));
        assert!(!is_trusted_mcp_command("/bin/bash"));
    }
}
