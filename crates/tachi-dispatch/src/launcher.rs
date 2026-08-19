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

    pub fn into_launch_spec(
        self,
        backend: impl Into<String>,
        prompt: impl Into<String>,
        timeout_secs: u64,
        env_vars: std::collections::HashMap<String, String>,
    ) -> tachi_params::LaunchSpec {
        let mut command = vec![self.program];
        command.extend(self.args);
        tachi_params::LaunchSpec {
            backend: backend.into(),
            command,
            cwd: self.current_dir.unwrap_or_else(|| PathBuf::from(".")),
            env_vars,
            prompt: prompt.into(),
            timeout_secs,
            harness_transport: None,
            harness_server_url: None,
        }
    }
}

/// Resolve the effective permission profile.
///
/// Defaults to `default`. `allowlist` requires at least one audited allowed
/// tool. `full` / `verify` are dangerous (headless non-interactive tool use)
/// and require explicit administrator opt-in via
/// `TACHI_DISPATCH_ALLOW_FULL_PERMISSION_PROFILE=true` (or the verify alias
/// `TACHI_DISPATCH_VERIFY_HEADLESS=true`).
///
/// #878-B: verification lanes that must run cargo/git without an interactive
/// TTY should use `permission_profile="verify"` with that opt-in. A plain
/// `allowlist` still maps only allowed tools but Claude may still block on
/// first-use permission UX depending on host CLI version — `verify` is the
/// documented non-interactive path.
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
        "full" | "verify" => {
            let full_ok = env_flag_true("TACHI_DISPATCH_ALLOW_FULL_PERMISSION_PROFILE");
            let verify_ok = env_flag_true("TACHI_DISPATCH_VERIFY_HEADLESS");
            if full_ok || (profile == "verify" && verify_ok) {
                Ok(PermissionProfile::Full)
            } else if profile == "verify" {
                Err(
                    "permission_profile 'verify' (headless verification lane) requires opt-in via TACHI_DISPATCH_VERIFY_HEADLESS=true or TACHI_DISPATCH_ALLOW_FULL_PERMISSION_PROFILE=true (#878-B)"
                        .to_string(),
                )
            } else {
                Err(
                    "permission_profile 'full' requires explicit opt-in via TACHI_DISPATCH_ALLOW_FULL_PERMISSION_PROFILE=true"
                        .to_string(),
                )
            }
        }
        other => Err(format!(
            "unsupported permission_profile '{other}'; allowed: default, allowlist, verify (headless, with opt-in), full (with opt-in)"
        )),
    }
}

fn env_flag_true(name: &str) -> bool {
    std::env::var(name)
        .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
        .unwrap_or(false)
}

fn reject_non_claude_allowlist(agent: &str, profile: PermissionProfile) -> Result<(), String> {
    if profile == PermissionProfile::Allowlist {
        return Err(format!(
            "permission_profile 'allowlist' is only supported for agent 'claude', not '{agent}'"
        ));
    }
    Ok(())
}

/// Codex CLI `--sandbox` accepts exactly these policy values. Any other value
/// must be rejected before spawn (#894 S0) rather than forwarded to the child,
/// where an unrecognized value is silently ineffective (fail-closed). `value`
/// must already be trimmed by the caller — this function does not trim.
/// Public so the dispatch entry point (`tachi-server`) can run the same
/// codex-specific check up front, before any stage/preflight/spawn work,
/// with the per-builder call in `build_codex_launch` remaining as
/// defense-in-depth.
pub const CODEX_SANDBOX_VALUES: &[&str] = &["read-only", "workspace-write", "danger-full-access"];

pub fn validate_codex_sandbox(value: &str) -> Result<&str, String> {
    if CODEX_SANDBOX_VALUES.contains(&value) {
        Ok(value)
    } else {
        Err(format!(
            "invalid codex --sandbox value '{value}'; allowed: {}",
            CODEX_SANDBOX_VALUES.join(", ")
        ))
    }
}

/// Backends with no sandbox primitive (claude, grok, kimi, custom/opencode,
/// acpx, native-ACP) must not silently drop a caller-supplied `sandbox`
/// request. Only codex consumes `--sandbox`; every other backend/transport
/// fail-closes with a receipt naming the backend and the requested sandbox
/// level (#894 S0). Public so every launch-path (subprocess adapters in this
/// crate, and the acpx/native-ACP spec builders in `tachi-server`) shares one
/// rejection function instead of re-deriving the receipt wording, and so the
/// dispatch entry point can run the same check before any stage/preflight/
/// spawn work happens (round-2 review: the per-builder calls below are now
/// defense-in-depth, not the only gate).
///
/// A present-but-blank/whitespace-only sandbox (`Some("")`, `Some("  ")`) is
/// treated as malformed input and rejected — NOT silently treated as
/// equivalent to `None` (that would let a caller bypass the "backend doesn't
/// support sandbox" signal by sending an empty string instead of omitting
/// the field).
pub fn reject_unsupported_sandbox(backend: &str, sandbox: Option<&str>) -> Result<(), String> {
    match sandbox {
        None => Ok(()),
        Some(raw) => {
            let trimmed = raw.trim();
            if trimmed.is_empty() {
                Err(format!(
                    "permission receipt: backend '{backend}' received a blank/whitespace-only sandbox request; a present-but-empty sandbox is malformed input, not equivalent to omitting it (fail-closed, #894 S0)"
                ))
            } else {
                Err(format!(
                    "permission receipt: backend '{backend}' has no sandbox concept and cannot honor requested sandbox '{trimmed}'; refusing to silently downgrade (fail-closed, #894 S0)"
                ))
            }
        }
    }
}

pub fn build_claude_launch(
    params: &DispatchLaunchParams,
    prompt: &str,
    mcp_config_path: Option<&Path>,
) -> Result<LaunchCommand, String> {
    reject_unsupported_sandbox("claude", params.sandbox.as_deref())?;
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
        // A present-but-blank sandbox (`Some("")`/whitespace) is malformed
        // input, not equivalent to `None` — reject it rather than silently
        // falling back to the "workspace-write" default (#894 S0 round 2).
        let sandbox = match params.sandbox.as_deref().map(str::trim) {
            None => "workspace-write",
            Some("") => {
                return Err(
                    "invalid codex --sandbox value: blank/whitespace-only sandbox request is malformed input, not equivalent to omitting it (fail-closed, #894 S0)"
                        .to_string(),
                )
            }
            Some(value) => validate_codex_sandbox(value)?,
        };
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
    reject_unsupported_sandbox("grok", params.sandbox.as_deref())?;
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
    reject_unsupported_sandbox("kimi", params.sandbox.as_deref())?;
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

/// Trust boundary (#894 S0, explicitly out of scope for this slice to widen):
/// `is_trusted_dispatch_command` only authorizes the *binary* (`params.command[0]`)
/// against the fixed allowlist below. Everything after that — the rest of
/// `params.command` (argv) and `prompt` — is forwarded to the child verbatim,
/// unsanitized, exactly as it arrived from the dispatch caller. This function
/// does not, and is not meant to, defend against a malicious *caller*; that
/// authority boundary is the dispatch entrypoint (who is allowed to call
/// `agent='custom'` at all), not this launcher. Sanitizing argv content is a
/// separate, larger piece of work than the authorizer soundness this slice
/// fixes and is intentionally not attempted here.
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
    // The custom/opencode backend has no permission or sandbox primitive of its
    // own. A non-default permission_profile or any sandbox request cannot be
    // honored and must fail closed with a receipt rather than be silently
    // dropped (#894 S0).
    let profile = resolve_permission_profile(params)?;
    if profile != PermissionProfile::Default {
        return Err(format!(
            "permission receipt: backend '{binary}' (custom/opencode) has no permission mechanism and cannot honor permission_profile '{}'; refusing to silently downgrade (fail-closed, #894 S0)",
            profile.as_str()
        ));
    }
    reject_unsupported_sandbox(binary, params.sandbox.as_deref())?;
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
    fn claude_fails_closed_on_sandbox_request() {
        // Claude has no `--sandbox`-equivalent knob; a caller-supplied
        // sandbox request must fail closed with a receipt naming the backend
        // and requested level, never be silently discarded (#894 S0).
        let mut params = params();
        params.sandbox = Some("workspace-write".to_string());
        let err = build_claude_launch(&params, "hello", None)
            .expect_err("claude has no sandbox concept and must fail closed");
        assert!(
            err.contains("claude") && err.contains("workspace-write"),
            "receipt must name backend + requested level: {err}"
        );
        assert!(err.contains("fail-closed"), "{err}");
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
    fn verify_profile_requires_headless_opt_in_and_maps_to_full_flags() {
        let _guard = env_lock();
        let prev_full = std::env::var("TACHI_DISPATCH_ALLOW_FULL_PERMISSION_PROFILE").ok();
        let prev_verify = std::env::var("TACHI_DISPATCH_VERIFY_HEADLESS").ok();
        std::env::remove_var("TACHI_DISPATCH_ALLOW_FULL_PERMISSION_PROFILE");
        std::env::remove_var("TACHI_DISPATCH_VERIFY_HEADLESS");

        let mut params = params();
        params.permission_profile = Some("verify".to_string());
        assert!(resolve_permission_profile(&params).is_err());

        std::env::set_var("TACHI_DISPATCH_VERIFY_HEADLESS", "true");
        assert_eq!(
            resolve_permission_profile(&params).unwrap(),
            PermissionProfile::Full
        );
        let cmd = build_claude_launch(&params, "hello", None).expect("verify claude");
        assert!(cmd
            .args
            .iter()
            .any(|a| a == "--dangerously-skip-permissions"));

        match prev_full {
            Some(v) => std::env::set_var("TACHI_DISPATCH_ALLOW_FULL_PERMISSION_PROFILE", v),
            None => std::env::remove_var("TACHI_DISPATCH_ALLOW_FULL_PERMISSION_PROFILE"),
        }
        match prev_verify {
            Some(v) => std::env::set_var("TACHI_DISPATCH_VERIFY_HEADLESS", v),
            None => std::env::remove_var("TACHI_DISPATCH_VERIFY_HEADLESS"),
        }
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
    fn codex_rejects_unknown_sandbox_value_before_spawn() {
        let mut params = params();
        params.sandbox = Some("workspace-writ".to_string());
        let err = build_codex_launch(&params, "hello", None)
            .expect_err("typo sandbox value must be rejected pre-spawn");
        assert!(
            err.contains("invalid codex --sandbox value 'workspace-writ'"),
            "unexpected error: {err}"
        );
        // The unknown value must never reach the launch args.
        assert!(build_codex_launch(&params, "hello", None).is_err());
    }

    #[test]
    fn codex_accepts_every_valid_sandbox_value() {
        for value in ["read-only", "workspace-write", "danger-full-access"] {
            let mut params = params();
            params.sandbox = Some(value.to_string());
            let cmd = build_codex_launch(&params, "hello", None)
                .unwrap_or_else(|err| panic!("valid sandbox '{value}' rejected: {err}"));
            assert!(
                cmd.args.windows(2).any(|pair| pair == ["--sandbox", value]),
                "sandbox '{value}' missing from args: {:?}",
                cmd.args
            );
        }
    }

    #[test]
    fn codex_accepts_whitespace_padded_valid_sandbox_value() {
        // #894 S0 round 2: the value is trimmed before enum-checking, so a
        // caller who sends " workspace-write " (padding, not blank) is not
        // penalized for whitespace that has no bearing on validity.
        let mut params = params();
        params.sandbox = Some("  workspace-write  ".to_string());
        let cmd = build_codex_launch(&params, "hello", None)
            .expect("whitespace-padded valid sandbox value should still be accepted");
        assert!(cmd
            .args
            .windows(2)
            .any(|pair| pair == ["--sandbox", "workspace-write"]));
    }

    #[test]
    fn codex_rejects_blank_sandbox_value_as_malformed() {
        // #894 S0 round 2: `Some("")`/whitespace-only is malformed input, NOT
        // silently equivalent to `None` (which would fall back to the
        // "workspace-write" default) — reject it explicitly.
        for blank in ["", "   ", "\t\n"] {
            let mut params = params();
            params.sandbox = Some(blank.to_string());
            let err = build_codex_launch(&params, "hello", None)
                .expect_err("blank sandbox must be rejected as malformed, not defaulted");
            assert!(
                err.contains("blank") || err.contains("malformed"),
                "unexpected error for blank input {blank:?}: {err}"
            );
        }
    }

    #[test]
    fn grok_fails_closed_on_sandbox_request() {
        let mut params = params();
        params.sandbox = Some("workspace-write".to_string());
        let err = build_grok_launch(&params, "hello", None)
            .expect_err("grok has no sandbox concept and must fail closed");
        assert!(err.contains("grok"), "receipt must name backend: {err}");
        assert!(
            err.contains("workspace-write"),
            "receipt must name requested level: {err}"
        );
        assert!(
            err.contains("fail-closed"),
            "must be a fail-closed receipt: {err}"
        );
    }

    #[test]
    fn grok_rejects_blank_sandbox_value_as_malformed() {
        // #894 S0 round 2: a present-but-blank sandbox must not be silently
        // treated as "no sandbox requested" (None) for a backend that has no
        // sandbox concept either — it's still fail-closed rejected.
        let mut params = params();
        params.sandbox = Some("   ".to_string());
        let err = build_grok_launch(&params, "hello", None)
            .expect_err("blank sandbox must be rejected, not silently treated as None");
        assert!(
            err.contains("blank") || err.contains("malformed"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn kimi_fails_closed_on_sandbox_request() {
        let mut params = params();
        params.sandbox = Some("read-only".to_string());
        let err = build_kimi_launch(&params, "hello")
            .expect_err("kimi has no sandbox concept and must fail closed");
        assert!(err.contains("kimi") && err.contains("read-only"), "{err}");
    }

    #[test]
    fn custom_fails_closed_on_unhonorable_permission_profile_and_sandbox() {
        // Non-default permission_profile the custom backend cannot honor.
        {
            let mut p = params();
            p.command = vec!["python3".to_string()];
            p.permission_profile = Some("allowlist".to_string());
            p.allowed_tools = vec!["Read".to_string()];
            let err = build_custom_launch(&p, "hello")
                .expect_err("custom cannot honor allowlist and must fail closed");
            assert!(
                err.contains("custom/opencode") && err.contains("allowlist"),
                "receipt must name backend + level: {err}"
            );
        }

        // Sandbox request the custom backend cannot honor.
        {
            let mut p = params();
            p.command = vec!["python3".to_string()];
            p.sandbox = Some("workspace-write".to_string());
            let err = build_custom_launch(&p, "hello")
                .expect_err("custom has no sandbox concept and must fail closed");
            assert!(err.contains("workspace-write"), "{err}");
        }

        // Default profile with no sandbox still builds cleanly.
        {
            let mut p = params();
            p.command = vec![
                "python3".to_string(),
                "-m".to_string(),
                "worker".to_string(),
            ];
            assert!(build_custom_launch(&p, "hello").is_ok());
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
