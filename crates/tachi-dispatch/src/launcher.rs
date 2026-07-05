//! Dispatch launcher policy and pure command construction.

use std::path::{Component, Path, PathBuf};

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct DispatchLaunchParams {
    pub cwd: Option<String>,
    pub model: Option<String>,
    pub permission_profile: Option<String>,
    pub allowed_tools: Vec<String>,
    pub max_turns: Option<u32>,
    pub sandbox: Option<String>,
    pub command: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PermissionProfile {
    Default,
    Allowlist,
    Full,
}

impl PermissionProfile {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Default => "default",
            Self::Allowlist => "allowlist",
            Self::Full => "full",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LaunchCommand {
    pub program: String,
    pub args: Vec<String>,
    pub current_dir: Option<PathBuf>,
}

impl LaunchCommand {
    fn new(program: impl Into<String>) -> Self {
        Self {
            program: program.into(),
            args: Vec::new(),
            current_dir: None,
        }
    }

    fn arg(&mut self, arg: impl Into<String>) {
        self.args.push(arg.into());
    }

    fn args(&mut self, args: impl IntoIterator<Item = impl Into<String>>) {
        self.args.extend(args.into_iter().map(Into::into));
    }

    fn current_dir(&mut self, cwd: impl Into<PathBuf>) {
        self.current_dir = Some(cwd.into());
    }
}

/// Resolve the effective permission profile.
///
/// Defaults to `default`. `allowlist` requires at least one audited allowed
/// tool. `full` is dangerous and requires explicit administrator opt-in via
/// `TACHI_DISPATCH_ALLOW_FULL_PERMISSION_PROFILE=true`.
pub fn resolve_permission_profile(
    params: &DispatchLaunchParams,
) -> Result<PermissionProfile, String> {
    let profile = params.permission_profile.as_deref().unwrap_or("default");
    match profile {
        "default" => Ok(PermissionProfile::Default),
        "allowlist" if !params.allowed_tools.is_empty() => Ok(PermissionProfile::Allowlist),
        "allowlist" => Err(
            "permission_profile 'allowlist' requires at least one allowed_tool".to_string(),
        ),
        "full" => {
            let allowed = std::env::var("TACHI_DISPATCH_ALLOW_FULL_PERMISSION_PROFILE")
                .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
                .unwrap_or(false);
            if allowed {
                Ok(PermissionProfile::Full)
            } else {
                Err(
                    "permission_profile 'full' requires explicit opt-in via TACHI_DISPATCH_ALLOW_FULL_PERMISSION_PROFILE=true"
                        .to_string(),
                )
            }
        }
        other => Err(format!(
            "unsupported permission_profile '{other}'; allowed: default, allowlist, full (with opt-in)"
        )),
    }
}

fn reject_non_claude_allowlist(agent: &str, profile: PermissionProfile) -> Result<(), String> {
    if profile == PermissionProfile::Allowlist {
        return Err(format!(
            "permission_profile 'allowlist' is only supported for agent 'claude', not '{agent}'"
        ));
    }
    Ok(())
}

pub fn build_claude_launch(
    params: &DispatchLaunchParams,
    prompt: &str,
    mcp_config_path: Option<&Path>,
) -> Result<LaunchCommand, String> {
    let mut cmd = LaunchCommand::new("claude");
    cmd.args(["-p", "--output-format", "json"]);

    match resolve_permission_profile(params)? {
        PermissionProfile::Full => cmd.arg("--dangerously-skip-permissions"),
        PermissionProfile::Allowlist => {
            for tool in &params.allowed_tools {
                cmd.arg("--allowedTools");
                cmd.arg(tool);
            }
        }
        PermissionProfile::Default => {}
    }

    if let Some(turns) = params.max_turns {
        cmd.arg("--max-turns");
        cmd.arg(turns.to_string());
    }

    if let Some(model) = &params.model {
        cmd.arg("--model");
        cmd.arg(model);
    }

    // Prompt must precede --mcp-config because that CLI option is variadic.
    cmd.arg(prompt);

    if let Some(path) = mcp_config_path {
        cmd.arg("--mcp-config");
        cmd.arg(path.to_string_lossy());
    }

    if let Some(cwd) = &params.cwd {
        cmd.current_dir(cwd);
    }
    Ok(cmd)
}

pub fn build_codex_launch(
    params: &DispatchLaunchParams,
    prompt: &str,
    _mcp_config_path: Option<&Path>,
) -> Result<LaunchCommand, String> {
    let mut cmd = LaunchCommand::new("codex");
    cmd.arg("exec");

    let profile = resolve_permission_profile(params)?;
    reject_non_claude_allowlist("codex", profile)?;
    if profile == PermissionProfile::Full {
        cmd.arg("--dangerously-bypass-approvals-and-sandbox");
    } else {
        let sandbox = params.sandbox.as_deref().unwrap_or("workspace-write");
        cmd.arg("--sandbox");
        cmd.arg(sandbox);
    }

    cmd.arg("--json");

    if let Some(turns) = params.max_turns {
        cmd.arg("-c");
        cmd.arg(format!("max_turns={turns}"));
    }

    if let Some(model) = &params.model {
        cmd.arg("-m");
        cmd.arg(model);
    }

    cmd.arg(prompt);

    if let Some(cwd) = &params.cwd {
        cmd.arg("-C");
        cmd.arg(cwd);
    }
    Ok(cmd)
}

pub fn build_grok_launch(
    params: &DispatchLaunchParams,
    prompt: &str,
    mcp_config_path: Option<&Path>,
) -> Result<LaunchCommand, String> {
    let mut cmd = LaunchCommand::new("grok");
    cmd.arg("-p");
    cmd.arg(prompt);
    cmd.arg("--output-format");
    cmd.arg("json");

    let profile = resolve_permission_profile(params)?;
    reject_non_claude_allowlist("grok", profile)?;
    if profile == PermissionProfile::Full {
        cmd.arg("--permission-mode");
        cmd.arg("bypassPermissions");
    }

    if let Some(turns) = params.max_turns {
        cmd.arg("--max-turns");
        cmd.arg(turns.to_string());
    }

    if let Some(model) = &params.model {
        cmd.arg("-m");
        cmd.arg(model);
    }

    if let Some(path) = mcp_config_path {
        cmd.arg("--mcp-config");
        cmd.arg(path.to_string_lossy());
    }

    if let Some(cwd) = &params.cwd {
        cmd.current_dir(cwd);
    }
    Ok(cmd)
}

pub fn build_kimi_launch(
    params: &DispatchLaunchParams,
    prompt: &str,
) -> Result<LaunchCommand, String> {
    let profile = resolve_permission_profile(params)?;
    reject_non_claude_allowlist("kimi", profile)?;
    let mut cmd = LaunchCommand::new("kimi");
    cmd.args(kimi_command_args(params, prompt, profile));

    if let Some(cwd) = &params.cwd {
        cmd.current_dir(cwd);
    }
    Ok(cmd)
}

fn kimi_command_args(
    params: &DispatchLaunchParams,
    prompt: &str,
    profile: PermissionProfile,
) -> Vec<String> {
    let mut args = vec![
        "-p".to_string(),
        prompt.to_string(),
        "--output-format".to_string(),
        "stream-json".to_string(),
    ];
    if profile == PermissionProfile::Full {
        args.push("-y".to_string());
    }

    if let Some(model) = &params.model {
        args.push("-m".to_string());
        args.push(model.clone());
    }

    args
}

pub fn build_custom_launch(
    params: &DispatchLaunchParams,
    prompt: &str,
) -> Result<LaunchCommand, String> {
    if params.command.is_empty() {
        return Err("agent='custom' requires a non-empty 'command' array".to_string());
    }
    let binary = &params.command[0];
    if !is_trusted_dispatch_command(binary) {
        return Err(format!(
            "Command '{}' is not in the trusted allowlist. Allowed: npx, node, bun, deno, python3, python, uv, cargo, rustup, docker, podman, tachi, opencode, acpx, or paths under /opt/homebrew/, /usr/local/bin/, /usr/bin/, /bin/, ~/.cargo/bin/, ~/.local/bin/",
            binary
        ));
    }
    let mut cmd = LaunchCommand::new(binary);
    cmd.args(params.command[1..].iter().cloned());
    cmd.arg(prompt);
    if let Some(cwd) = &params.cwd {
        cmd.current_dir(cwd);
    }
    Ok(cmd)
}

/// Trusted command list for `agent=custom` dispatch.
pub fn is_trusted_dispatch_command(cmd: &str) -> bool {
    const TRUSTED_BASENAMES: &[&str] = &[
        "npx", "node", "bun", "deno", "python3", "python", "uv", "cargo", "rustup", "docker",
        "podman", "tachi", "opencode", "acpx",
    ];

    let path = Path::new(cmd);
    if path.components().count() == 1 && TRUSTED_BASENAMES.contains(&cmd) {
        return true;
    }

    if !path.is_absolute()
        || path.file_name().is_none()
        || cmd
            .split('/')
            .any(|segment| segment == "." || segment == "..")
        || path
            .components()
            .any(|component| matches!(component, Component::CurDir | Component::ParentDir))
    {
        return false;
    }

    const TRUSTED_PREFIXES: &[&str] = &["/opt/homebrew", "/usr/local/bin", "/usr/bin", "/bin"];

    for prefix in TRUSTED_PREFIXES {
        if path.starts_with(Path::new(prefix)) {
            return true;
        }
    }

    if let Ok(home) = std::env::var("HOME") {
        let home_prefixes = [
            format!("{}/.cargo/bin/", home),
            format!("{}/.local/bin/", home),
            format!("{}/.nvm/", home),
            format!("{}/.bun/bin/", home),
        ];
        for prefix in &home_prefixes {
            if path.starts_with(Path::new(prefix)) {
                return true;
            }
        }
    }

    false
}

pub fn tail_chars(text: &str, max_chars: usize) -> String {
    let mut chars = text.chars().rev().take(max_chars).collect::<Vec<_>>();
    chars.reverse();
    chars.into_iter().collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Mutex, OnceLock};

    fn params() -> DispatchLaunchParams {
        DispatchLaunchParams::default()
    }

    fn env_lock() -> std::sync::MutexGuard<'static, ()> {
        static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        LOCK.get_or_init(|| Mutex::new(())).lock().unwrap()
    }

    #[test]
    fn claude_allowlist_uses_audited_allowed_tools_and_prompt_precedes_mcp_config() {
        let mut params = params();
        params.permission_profile = Some("allowlist".to_string());
        params.allowed_tools = vec!["Read".to_string(), "Bash(git status*)".to_string()];

        let cmd = build_claude_launch(&params, "hello", Some(Path::new("/tmp/mcp.json")))
            .expect("claude allowlist command");

        assert_eq!(cmd.program, "claude");
        assert!(cmd
            .args
            .windows(2)
            .any(|pair| pair == ["--allowedTools", "Read"]));
        assert!(cmd
            .args
            .windows(2)
            .any(|pair| pair == ["--allowedTools", "Bash(git status*)"]));
        let prompt_pos = cmd.args.iter().position(|arg| arg == "hello").unwrap();
        let mcp_pos = cmd
            .args
            .iter()
            .position(|arg| arg == "--mcp-config")
            .unwrap();
        assert!(
            prompt_pos < mcp_pos,
            "Claude prompt must precede variadic --mcp-config: {:?}",
            cmd.args
        );
    }

    #[test]
    fn allowlist_requires_tools_and_is_rejected_for_non_claude_agents() {
        let mut params = params();
        params.permission_profile = Some("allowlist".to_string());
        assert!(resolve_permission_profile(&params).is_err());

        params.allowed_tools = vec!["Read".to_string()];
        assert!(build_codex_launch(&params, "hello", None).is_err());
        assert!(build_grok_launch(&params, "hello", None).is_err());
        assert!(build_kimi_launch(&params, "hello").is_err());
    }

    #[test]
    fn dangerous_full_profile_requires_env_opt_in_and_adds_backend_flags() {
        let _guard = env_lock();
        let prev = std::env::var("TACHI_DISPATCH_ALLOW_FULL_PERMISSION_PROFILE").ok();
        std::env::remove_var("TACHI_DISPATCH_ALLOW_FULL_PERMISSION_PROFILE");

        let mut params = params();
        params.permission_profile = Some("full".to_string());
        assert!(resolve_permission_profile(&params).is_err());
        assert!(build_claude_launch(&params, "hello", None).is_err());

        std::env::set_var("TACHI_DISPATCH_ALLOW_FULL_PERMISSION_PROFILE", "true");
        assert_eq!(
            resolve_permission_profile(&params).unwrap(),
            PermissionProfile::Full
        );
        assert!(build_claude_launch(&params, "hello", None)
            .unwrap()
            .args
            .contains(&"--dangerously-skip-permissions".to_string()));
        assert!(build_codex_launch(&params, "hello", None)
            .unwrap()
            .args
            .contains(&"--dangerously-bypass-approvals-and-sandbox".to_string()));

        match prev {
            Some(value) => std::env::set_var("TACHI_DISPATCH_ALLOW_FULL_PERMISSION_PROFILE", value),
            None => std::env::remove_var("TACHI_DISPATCH_ALLOW_FULL_PERMISSION_PROFILE"),
        }
    }

    #[test]
    fn codex_default_command_uses_workspace_sandbox_and_preserves_cwd_arg() {
        let mut params = params();
        params.cwd = Some("/work/repo".to_string());
        params.max_turns = Some(4);
        params.model = Some("gpt-5-codex".to_string());

        let cmd = build_codex_launch(&params, "fix it", None).expect("codex command");

        assert_eq!(cmd.program, "codex");
        assert!(cmd
            .args
            .windows(2)
            .any(|pair| pair == ["--sandbox", "workspace-write"]));
        assert!(cmd
            .args
            .windows(2)
            .any(|pair| pair == ["-c", "max_turns=4"]));
        assert!(cmd
            .args
            .windows(2)
            .any(|pair| pair == ["-m", "gpt-5-codex"]));
        assert!(cmd.args.windows(2).any(|pair| pair == ["-C", "/work/repo"]));
    }

    #[test]
    fn custom_command_uses_trusted_allowlist_and_appends_prompt() {
        let mut params = params();
        params.command = vec![
            "python3".to_string(),
            "-m".to_string(),
            "worker".to_string(),
        ];

        let cmd = build_custom_launch(&params, "run task").expect("custom command");

        assert_eq!(cmd.program, "python3");
        assert_eq!(cmd.args, vec!["-m", "worker", "run task"]);

        params.command = vec!["/tmp/not-trusted".to_string()];
        assert!(build_custom_launch(&params, "run task").is_err());

        params.command = vec!["/tmp/python3".to_string()];
        assert!(
            build_custom_launch(&params, "run task").is_err(),
            "paths must live under trusted prefixes even when the basename is trusted"
        );

        for cmd in [
            "/usr/local/bin/../../../tmp/python3",
            "/bin/../tmp/evil",
            "/opt/homebrew/./bin/python3",
        ] {
            params.command = vec![cmd.to_string()];
            assert!(
                build_custom_launch(&params, "run task").is_err(),
                "trusted prefixes must not allow traversal components: {cmd}"
            );
        }
    }

    #[test]
    fn kimi_command_uses_supported_stream_json_output() {
        let cmd = build_kimi_launch(&params(), "hello").expect("kimi command");

        assert!(cmd.args.windows(2).any(|pair| pair == ["-p", "hello"]));
        assert!(cmd
            .args
            .windows(2)
            .any(|pair| pair == ["--output-format", "stream-json"]));
        assert!(
            !cmd.args
                .windows(2)
                .any(|pair| pair == ["--output-format", "json"]),
            "Kimi Code supports text/stream-json, not json: {:?}",
            cmd.args
        );
    }
}
