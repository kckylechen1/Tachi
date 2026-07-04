use crate::tool_params::TachiDispatchParams;
use std::ffi::{OsStr, OsString};
use std::path::PathBuf;
use tokio::process::Command;

const FULL_PERMISSION_PROFILE_ENV: &str = "TACHI_DISPATCH_ALLOW_FULL_PERMISSION_PROFILE";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum LauncherAgent {
    Claude,
    Codex,
    Grok,
    Kimi,
}

impl LauncherAgent {
    fn name(self) -> &'static str {
        match self {
            LauncherAgent::Claude => "claude",
            LauncherAgent::Codex => "codex",
            LauncherAgent::Grok => "grok",
            LauncherAgent::Kimi => "kimi",
        }
    }
}

#[derive(Debug, Eq, PartialEq)]
enum LauncherProfile<'a> {
    Default,
    Allowlist(&'a [String]),
    Full,
}

impl<'a> LauncherProfile<'a> {
    fn resolve(params: &'a TachiDispatchParams) -> Result<Self, String> {
        let profile = params.permission_profile.as_deref().unwrap_or("default");
        match profile {
            "default" => Ok(Self::Default),
            "allowlist" if !params.allowed_tools.is_empty() => {
                Ok(Self::Allowlist(&params.allowed_tools))
            }
            "allowlist" => Err(
                "permission_profile 'allowlist' requires at least one allowed_tool".to_string(),
            ),
            "full" if full_permission_profile_allowed() => Ok(Self::Full),
            "full" => Err(format!(
                "permission_profile 'full' requires explicit opt-in via {FULL_PERMISSION_PROFILE_ENV}=true"
            )),
            other => Err(format!(
                "unsupported permission_profile '{other}'; allowed: default, allowlist, full (with opt-in)"
            )),
        }
    }

    fn as_str(&self) -> &'static str {
        match self {
            Self::Default => "default",
            Self::Allowlist(_) => "allowlist",
            Self::Full => "full",
        }
    }

    fn reject_allowlist_for(&self, agent: LauncherAgent) -> Result<(), String> {
        if matches!(self, Self::Allowlist(_)) && agent != LauncherAgent::Claude {
            return Err(format!(
                "permission_profile 'allowlist' is only supported for agent 'claude', not '{}'",
                agent.name()
            ));
        }
        Ok(())
    }
}

fn full_permission_profile_allowed() -> bool {
    std::env::var(FULL_PERMISSION_PROFILE_ENV)
        .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
        .unwrap_or(false)
}

#[derive(Debug, Eq, PartialEq)]
struct AgentCommandSpec {
    program: &'static str,
    args: Vec<OsString>,
    current_dir: Option<PathBuf>,
}

impl AgentCommandSpec {
    fn new(program: &'static str) -> Self {
        Self {
            program,
            args: Vec::new(),
            current_dir: None,
        }
    }

    fn arg(&mut self, arg: impl AsRef<OsStr>) {
        self.args.push(arg.as_ref().to_os_string());
    }

    fn args<I, S>(&mut self, args: I)
    where
        I: IntoIterator<Item = S>,
        S: AsRef<OsStr>,
    {
        for arg in args {
            self.arg(arg);
        }
    }

    fn build_command(self) -> Command {
        let mut cmd = Command::new(self.program);
        cmd.args(self.args);
        if let Some(cwd) = self.current_dir {
            cmd.current_dir(cwd);
        }
        cmd
    }
}

/// Resolve the effective permission profile.
///
/// Defaults to `"default"` (Claude prompts for confirmations; Codex uses the
/// configured sandbox). The `"allowlist"` profile is also accepted when
/// `allowed_tools` is non-empty. The `"full"` profile is rejected unless the
/// administrator explicitly opts in via `TACHI_DISPATCH_ALLOW_FULL_PERMISSION_PROFILE=true`.
pub(super) fn resolve_permission_profile(params: &TachiDispatchParams) -> Result<&str, String> {
    Ok(LauncherProfile::resolve(params)?.as_str())
}

pub(super) fn build_claude_command(
    params: &TachiDispatchParams,
    prompt: &str,
    mcp_config_path: Option<&PathBuf>,
) -> Result<Command, String> {
    Ok(build_claude_command_spec(params, prompt, mcp_config_path)?.build_command())
}

fn build_claude_command_spec(
    params: &TachiDispatchParams,
    prompt: &str,
    mcp_config_path: Option<&PathBuf>,
) -> Result<AgentCommandSpec, String> {
    let profile = LauncherProfile::resolve(params)?;
    let mut spec = AgentCommandSpec::new("claude");
    spec.arg("-p"); // print mode
    spec.args(["--output-format", "json"]);

    match profile {
        LauncherProfile::Full => {
            spec.arg("--dangerously-skip-permissions");
        }
        LauncherProfile::Allowlist(tools) => {
            for tool in tools {
                spec.arg("--allowedTools");
                spec.arg(tool);
            }
        }
        LauncherProfile::Default => {}
    }

    if let Some(turns) = params.max_turns {
        spec.arg("--max-turns");
        spec.arg(turns.to_string());
    }

    if let Some(ref model) = params.model {
        spec.arg("--model");
        spec.arg(model);
    }

    // Prompt MUST come before --mcp-config because --mcp-config <configs...>
    // is a varadic arg that swallows all subsequent positional args.
    spec.arg(prompt);

    if let Some(path) = mcp_config_path {
        spec.arg("--mcp-config");
        spec.arg(path);
    }

    if let Some(ref cwd) = params.cwd {
        spec.current_dir = Some(PathBuf::from(cwd));
    }
    Ok(spec)
}

pub(super) fn build_codex_command(
    params: &TachiDispatchParams,
    prompt: &str,
    _mcp_config_path: Option<&PathBuf>,
) -> Result<Command, String> {
    Ok(build_codex_command_spec(params, prompt)?.build_command())
}

fn build_codex_command_spec(
    params: &TachiDispatchParams,
    prompt: &str,
) -> Result<AgentCommandSpec, String> {
    let profile = LauncherProfile::resolve(params)?;
    profile.reject_allowlist_for(LauncherAgent::Codex)?;
    let mut spec = AgentCommandSpec::new("codex");
    spec.arg("exec"); // non-interactive subcommand

    if profile == LauncherProfile::Full {
        spec.arg("--dangerously-bypass-approvals-and-sandbox");
    } else {
        let sandbox = params.sandbox.as_deref().unwrap_or("workspace-write");
        spec.arg("--sandbox");
        spec.arg(sandbox);
    }

    spec.arg("--json");

    if let Some(turns) = params.max_turns {
        // Codex doesn't have a direct --max-turns; use -c config override.
        spec.arg("-c");
        spec.arg(format!("max_turns={turns}"));
    }

    if let Some(ref model) = params.model {
        spec.arg("-m");
        spec.arg(model);
    }

    spec.arg(prompt);

    if let Some(ref cwd) = params.cwd {
        spec.arg("-C");
        spec.arg(cwd);
    }
    Ok(spec)
}

pub(super) fn build_grok_command(
    params: &TachiDispatchParams,
    prompt: &str,
    mcp_config_path: Option<&PathBuf>,
) -> Result<Command, String> {
    Ok(build_grok_command_spec(params, prompt, mcp_config_path)?.build_command())
}

fn build_grok_command_spec(
    params: &TachiDispatchParams,
    prompt: &str,
    mcp_config_path: Option<&PathBuf>,
) -> Result<AgentCommandSpec, String> {
    let profile = LauncherProfile::resolve(params)?;
    profile.reject_allowlist_for(LauncherAgent::Grok)?;
    let mut spec = AgentCommandSpec::new("grok");
    spec.arg("-p");
    spec.arg(prompt);
    spec.args(["--output-format", "json"]);

    if profile == LauncherProfile::Full {
        spec.args(["--permission-mode", "bypassPermissions"]);
    }

    if let Some(turns) = params.max_turns {
        spec.arg("--max-turns");
        spec.arg(turns.to_string());
    }

    if let Some(ref model) = params.model {
        spec.arg("-m");
        spec.arg(model);
    }

    if let Some(path) = mcp_config_path {
        spec.arg("--mcp-config");
        spec.arg(path);
    }

    if let Some(ref cwd) = params.cwd {
        spec.current_dir = Some(PathBuf::from(cwd));
    }
    Ok(spec)
}

pub(super) fn build_kimi_command(
    params: &TachiDispatchParams,
    prompt: &str,
) -> Result<Command, String> {
    Ok(build_kimi_command_spec(params, prompt)?.build_command())
}

fn build_kimi_command_spec(
    params: &TachiDispatchParams,
    prompt: &str,
) -> Result<AgentCommandSpec, String> {
    let profile = LauncherProfile::resolve(params)?;
    profile.reject_allowlist_for(LauncherAgent::Kimi)?;
    let mut spec = AgentCommandSpec::new("kimi");
    spec.args(kimi_command_args(params, prompt, &profile));
    if let Some(ref cwd) = params.cwd {
        spec.current_dir = Some(PathBuf::from(cwd));
    }
    Ok(spec)
}

fn kimi_command_args(
    params: &TachiDispatchParams,
    prompt: &str,
    profile: &LauncherProfile<'_>,
) -> Vec<String> {
    let mut args = vec![
        "-p".to_string(),
        prompt.to_string(),
        "--output-format".to_string(),
        "stream-json".to_string(),
    ];
    if matches!(profile, LauncherProfile::Full) {
        args.push("-y".to_string());
    }

    if let Some(ref model) = params.model {
        args.push("-m".to_string());
        args.push(model.clone());
    }

    args
}

pub(super) fn build_custom_command(
    params: &TachiDispatchParams,
    prompt: &str,
) -> Result<Command, String> {
    if params.command.is_empty() {
        return Err("agent='custom' requires a non-empty 'command' array".to_string());
    }
    let binary = &params.command[0];
    if !crate::utils::is_trusted_command(binary) {
        return Err(format!(
            "Command '{}' is not in the trusted allowlist. Allowed: npx, node, bun, deno, python3, python, uv, cargo, rustup, docker, podman, tachi, opencode, acpx, or paths under /opt/homebrew/, /usr/local/bin/, ~/.cargo/bin/, ~/.local/bin/",
            binary
        ));
    }
    let mut cmd = Command::new(binary);
    for arg in &params.command[1..] {
        cmd.arg(arg);
    }
    cmd.arg(prompt);
    if let Some(ref cwd) = params.cwd {
        cmd.current_dir(cwd);
    }
    Ok(cmd)
}

#[cfg(test)]
mod tests {
    use super::*;

    struct EnvRestore {
        key: &'static str,
        old: Option<String>,
    }

    impl EnvRestore {
        fn set(key: &'static str, value: &str) -> Self {
            let old = std::env::var(key).ok();
            std::env::set_var(key, value);
            Self { key, old }
        }

        fn remove(key: &'static str) -> Self {
            let old = std::env::var(key).ok();
            std::env::remove_var(key);
            Self { key, old }
        }
    }

    impl Drop for EnvRestore {
        fn drop(&mut self) {
            if let Some(old) = &self.old {
                std::env::set_var(self.key, old);
            } else {
                std::env::remove_var(self.key);
            }
        }
    }

    fn dispatch_params(agent: &str) -> TachiDispatchParams {
        TachiDispatchParams {
            agent: Some(agent.to_string()),
            profile: None,
            task: "noop".to_string(),
            cwd: None,
            skills: Vec::new(),
            context_query: None,
            model: None,
            timeout_secs: 5,
            permission_profile: None,
            allowed_tools: Vec::new(),
            max_turns: None,
            sandbox: None,
            inject_tachi_mcp: None,
            inject_hub_mcps: None,
            command: Vec::new(),
            harness_transport: None,
            harness_server_url: None,
            project: None,
            stage: None,
            credential_profiles: Vec::new(),
            issue_ref: None,
            pr_ref: None,
            flow_id: None,
            tool_profile: None,
            auto_capability_bundle: None,
            mcp_access: None,
            allowed_mcp_servers: Vec::new(),
        }
    }

    fn spec_args(spec: &AgentCommandSpec) -> Vec<String> {
        spec.args
            .iter()
            .map(|arg| arg.to_string_lossy().to_string())
            .collect()
    }

    fn assert_no_dangerous_flags(args: &[String]) {
        for forbidden in [
            "--dangerously-skip-permissions",
            "--dangerously-bypass-approvals-and-sandbox",
            "bypassPermissions",
            "-y",
        ] {
            assert!(
                !args.iter().any(|arg| arg == forbidden),
                "unexpected dangerous flag {forbidden} in {args:?}"
            );
        }
    }

    #[test]
    fn kimi_command_uses_supported_stream_json_output() {
        let params = dispatch_params("kimi");
        let profile = LauncherProfile::Default;
        let args = kimi_command_args(&params, "hello", &profile);

        assert!(args.windows(2).any(|pair| pair == ["-p", "hello"]));
        assert!(args
            .windows(2)
            .any(|pair| pair == ["--output-format", "stream-json"]));
        assert!(
            !args
                .windows(2)
                .any(|pair| pair == ["--output-format", "json"]),
            "Kimi Code supports text/stream-json, not json: {args:?}"
        );
    }

    #[test]
    fn default_permission_profiles_do_not_emit_dangerous_flags() {
        let _guard = crate::shell_ops::tachi_run_root_env_lock()
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let _full_gate = EnvRestore::remove(FULL_PERMISSION_PROFILE_ENV);

        let claude_args = spec_args(
            &build_claude_command_spec(&dispatch_params("claude"), "hello", None)
                .expect("claude default spec"),
        );
        let codex_args = spec_args(
            &build_codex_command_spec(&dispatch_params("codex"), "hello")
                .expect("codex default spec"),
        );
        let grok_args = spec_args(
            &build_grok_command_spec(&dispatch_params("grok"), "hello", None)
                .expect("grok default spec"),
        );
        let kimi_args = spec_args(
            &build_kimi_command_spec(&dispatch_params("kimi"), "hello").expect("kimi default spec"),
        );

        assert_no_dangerous_flags(&claude_args);
        assert_no_dangerous_flags(&codex_args);
        assert_no_dangerous_flags(&grok_args);
        assert_no_dangerous_flags(&kimi_args);
        assert!(
            codex_args
                .windows(2)
                .any(|pair| pair == ["--sandbox", "workspace-write"]),
            "codex default should keep workspace-write sandbox: {codex_args:?}"
        );
    }

    #[test]
    fn full_permission_profile_requires_opt_in_env() {
        let _guard = crate::shell_ops::tachi_run_root_env_lock()
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let mut params = dispatch_params("claude");
        params.permission_profile = Some("full".to_string());
        let _full_gate = EnvRestore::remove(FULL_PERMISSION_PROFILE_ENV);

        assert!(
            resolve_permission_profile(&params).is_err(),
            "'full' should be rejected without opt-in env var"
        );
        assert!(
            build_claude_command(&params, "hello", None).is_err(),
            "command build should propagate full-profile rejection"
        );

        let _full_gate = EnvRestore::set(FULL_PERMISSION_PROFILE_ENV, "true");
        assert_eq!(
            resolve_permission_profile(&params).expect("opt-in should allow full"),
            "full"
        );
        assert!(
            build_claude_command(&params, "hello", None).is_ok(),
            "build should succeed after opt-in"
        );
    }

    #[test]
    fn full_permission_profile_emits_provider_flags_only_after_env_gate() {
        let _guard = crate::shell_ops::tachi_run_root_env_lock()
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let _full_gate = EnvRestore::remove(FULL_PERMISSION_PROFILE_ENV);

        let mut claude = dispatch_params("claude");
        claude.permission_profile = Some("full".to_string());
        let mut codex = dispatch_params("codex");
        codex.permission_profile = Some("full".to_string());
        let mut grok = dispatch_params("grok");
        grok.permission_profile = Some("full".to_string());
        let mut kimi = dispatch_params("kimi");
        kimi.permission_profile = Some("full".to_string());

        assert!(build_claude_command_spec(&claude, "hello", None).is_err());
        assert!(build_codex_command_spec(&codex, "hello").is_err());
        assert!(build_grok_command_spec(&grok, "hello", None).is_err());
        assert!(build_kimi_command_spec(&kimi, "hello").is_err());

        let _full_gate = EnvRestore::set(FULL_PERMISSION_PROFILE_ENV, "true");
        let claude_args =
            spec_args(&build_claude_command_spec(&claude, "hello", None).expect("claude full"));
        let codex_args = spec_args(&build_codex_command_spec(&codex, "hello").expect("codex full"));
        let grok_args =
            spec_args(&build_grok_command_spec(&grok, "hello", None).expect("grok full"));
        let kimi_args = spec_args(&build_kimi_command_spec(&kimi, "hello").expect("kimi full"));

        assert!(claude_args
            .iter()
            .any(|arg| arg == "--dangerously-skip-permissions"));
        assert!(codex_args
            .iter()
            .any(|arg| arg == "--dangerously-bypass-approvals-and-sandbox"));
        assert!(
            !codex_args.iter().any(|arg| arg == "--sandbox"),
            "full codex profile must not also pass sandbox flags: {codex_args:?}"
        );
        assert!(grok_args
            .windows(2)
            .any(|pair| pair == ["--permission-mode", "bypassPermissions"]));
        assert!(kimi_args.iter().any(|arg| arg == "-y"));
    }

    #[test]
    fn claude_allowlist_profile_emits_allowed_tools_without_full_flags() {
        let mut params = dispatch_params("claude");
        params.permission_profile = Some("allowlist".to_string());
        params.allowed_tools = vec!["Read".to_string(), "Glob".to_string()];

        let args =
            spec_args(&build_claude_command_spec(&params, "hello", None).expect("allowlist spec"));

        assert_no_dangerous_flags(&args);
        assert!(args
            .windows(2)
            .any(|pair| pair == ["--allowedTools", "Read"]));
        assert!(args
            .windows(2)
            .any(|pair| pair == ["--allowedTools", "Glob"]));
    }

    #[test]
    fn allowlist_permission_profile_rejected_for_non_claude_agents() {
        let mut params = dispatch_params("codex");
        params.permission_profile = Some("allowlist".to_string());
        params.allowed_tools = vec!["Read".to_string()];
        assert!(
            build_codex_command(&params, "hello", None).is_err(),
            "codex should reject allowlist profile"
        );

        params = dispatch_params("grok");
        params.permission_profile = Some("allowlist".to_string());
        params.allowed_tools = vec!["Read".to_string()];
        assert!(
            build_grok_command(&params, "hello", None).is_err(),
            "grok should reject allowlist profile"
        );

        params = dispatch_params("kimi");
        params.permission_profile = Some("allowlist".to_string());
        params.allowed_tools = vec!["Read".to_string()];
        assert!(
            build_kimi_command(&params, "hello").is_err(),
            "kimi should reject allowlist profile"
        );
    }
}
