use std::path::PathBuf;
use tachi_dispatch::{
    build_claude_launch, build_codex_launch, build_custom_launch, build_grok_launch,
    build_kimi_launch, DispatchLaunchParams, LaunchCommand,
};
use tachi_params::{ExecutionGrant, ResolvedStaffAssignment};
use tokio::process::Command;

fn launch_params(
    assignment: &ResolvedStaffAssignment,
    grant: &ExecutionGrant,
    command: &[String],
) -> DispatchLaunchParams {
    DispatchLaunchParams {
        cwd: grant
            .allowed_cwd
            .as_ref()
            .map(|cwd| cwd.to_string_lossy().to_string()),
        model: assignment.selected_model.clone(),
        permission_profile: grant.permission_profile.clone(),
        allowed_tools: grant.allowed_tools.clone(),
        max_turns: grant.max_turns,
        sandbox: grant.sandbox.clone(),
        command: command.to_vec(),
    }
}

fn command_from_launch(spec: LaunchCommand) -> Command {
    let mut cmd = Command::new(spec.program);
    cmd.args(spec.args);
    if let Some(cwd) = spec.current_dir {
        cmd.current_dir(cwd);
    }
    cmd
}

pub(super) fn resolve_permission_profile(grant: &ExecutionGrant) -> Result<&str, String> {
    let resolved = tachi_dispatch::resolve_permission_profile(&DispatchLaunchParams {
        cwd: None,
        model: None,
        permission_profile: grant.permission_profile.clone(),
        allowed_tools: grant.allowed_tools.clone(),
        max_turns: grant.max_turns,
        sandbox: grant.sandbox.clone(),
        command: Vec::new(),
    })?;
    Ok(resolved.as_str())
}

pub(super) fn build_claude_command(
    assignment: &ResolvedStaffAssignment,
    grant: &ExecutionGrant,
    command: &[String],
    prompt: &str,
    mcp_config_path: Option<&PathBuf>,
) -> Result<Command, String> {
    build_claude_launch(
        &launch_params(assignment, grant, command),
        prompt,
        mcp_config_path.map(PathBuf::as_path),
    )
    .map(command_from_launch)
}

pub(super) fn build_codex_command(
    assignment: &ResolvedStaffAssignment,
    grant: &ExecutionGrant,
    command: &[String],
    prompt: &str,
    mcp_config_path: Option<&PathBuf>,
) -> Result<Command, String> {
    build_codex_launch(
        &launch_params(assignment, grant, command),
        prompt,
        mcp_config_path.map(PathBuf::as_path),
    )
    .map(command_from_launch)
}

pub(super) fn build_grok_command(
    assignment: &ResolvedStaffAssignment,
    grant: &ExecutionGrant,
    command: &[String],
    prompt: &str,
    mcp_config_path: Option<&PathBuf>,
) -> Result<Command, String> {
    build_grok_launch(
        &launch_params(assignment, grant, command),
        prompt,
        mcp_config_path.map(PathBuf::as_path),
    )
    .map(command_from_launch)
}

pub(super) fn build_kimi_command(
    assignment: &ResolvedStaffAssignment,
    grant: &ExecutionGrant,
    command: &[String],
    prompt: &str,
) -> Result<Command, String> {
    build_kimi_launch(&launch_params(assignment, grant, command), prompt).map(command_from_launch)
}

pub(super) fn build_custom_command(
    assignment: &ResolvedStaffAssignment,
    grant: &ExecutionGrant,
    command: &[String],
    prompt: &str,
) -> Result<Command, String> {
    build_custom_launch(&launch_params(assignment, grant, command), prompt).map(command_from_launch)
}

/// `agent='opencode'` shares its launch mechanics with `agent='custom'` (both
/// resolve to `build_custom_launch`), but that launcher has no notion of which
/// alias the caller actually used, so its errors are written in `custom`
/// vocabulary. That's fine when the caller said `custom`; it's an abstraction
/// leak when the caller said `opencode` (#1174) — `opencode` is a host adapter,
/// not a launchable backend on its own, and the caller needs pointing at the
/// profiles that resolve to it, not at an internal backend name they never
/// typed. Check the one precondition that trips this in practice (missing
/// `command`) before delegating, so the error stays in the caller's own words.
pub(super) fn build_opencode_command(
    assignment: &ResolvedStaffAssignment,
    grant: &ExecutionGrant,
    command: &[String],
    prompt: &str,
) -> Result<Command, String> {
    if command.is_empty() {
        return Err(opencode_missing_command_error());
    }
    build_custom_command(assignment, grant, command, prompt)
}

fn opencode_missing_command_error() -> String {
    let profiles: Vec<&str> = tachi_dispatch::DISPATCH_PROFILES
        .iter()
        .filter(|profile| tachi_dispatch::profile_uses_opencode_adapter(profile))
        .map(|profile| profile.name)
        .collect();
    format!(
        "agent='opencode' cannot be dispatched directly: opencode is a host adapter, not a launchable backend. Use profile dispatch instead (available: {}).",
        profiles.join(", ")
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dispatch_params(agent: &str) -> TachiDispatchParams {
        TachiDispatchParams {
            staffing_reason: tachi_params::TachiDispatchReason::ExplicitUserRequest,
            agent: Some(agent.to_string()),
            profile: None,
            task: "noop".to_string(),
            execution_level: None,
            cwd: None,
            env_id: None,
            unmanaged_cwd: None,
            skills: Vec::new(),
            context_query: None,
            model: None,
            timeout_secs: 5,
            permission_profile: None,
            allowed_tools: Vec::new(),
            completion_predicate: None,
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
            verbose: None,
            inject_card: None,
        }
    }

    fn command_args(cmd: &Command) -> Vec<String> {
        cmd.as_std()
            .get_args()
            .map(|arg| arg.to_string_lossy().to_string())
            .collect()
    }

    #[test]
    fn claude_adapter_preserves_allowlist_and_mcp_config_order() {
        let mut params = dispatch_params("claude");
        params.permission_profile = Some("allowlist".to_string());
        params.allowed_tools = vec!["Read".to_string(), "Bash(git status*)".to_string()];
        params.max_turns = Some(3);
        params.model = Some("sonnet".to_string());
        params.cwd = Some("/work/repo".to_string());
        let mcp_path = PathBuf::from("/tmp/tachi-mcp.json");

        let cmd =
            build_claude_command(&params, "inspect", Some(&mcp_path)).expect("claude command");
        let args = command_args(&cmd);

        assert!(args
            .windows(2)
            .any(|pair| pair == ["--allowedTools", "Read"]));
        assert!(args
            .windows(2)
            .any(|pair| pair == ["--allowedTools", "Bash(git status*)"]));
        assert!(args.windows(2).any(|pair| pair == ["--max-turns", "3"]));
        assert!(args.windows(2).any(|pair| pair == ["--model", "sonnet"]));
        let prompt_pos = args.iter().position(|arg| arg == "inspect").unwrap();
        let mcp_pos = args.iter().position(|arg| arg == "--mcp-config").unwrap();
        assert!(
            prompt_pos < mcp_pos,
            "prompt must precede --mcp-config: {args:?}"
        );
        assert_eq!(
            cmd.as_std().get_current_dir(),
            Some(std::path::Path::new("/work/repo"))
        );
    }

    #[test]
    fn codex_adapter_preserves_sandbox_turns_model_and_cwd_arg() {
        let mut params = dispatch_params("codex");
        params.cwd = Some("/work/repo".to_string());
        params.sandbox = Some("read-only".to_string());
        params.max_turns = Some(4);
        params.model = Some("gpt-5-codex".to_string());

        let cmd = build_codex_command(&params, "fix it", None).expect("codex command");
        let args = command_args(&cmd);

        assert!(args
            .windows(2)
            .any(|pair| pair == ["--sandbox", "read-only"]));
        assert!(args.windows(2).any(|pair| pair == ["-c", "max_turns=4"]));
        assert!(args.windows(2).any(|pair| pair == ["-m", "gpt-5-codex"]));
        assert!(args.windows(2).any(|pair| pair == ["-C", "/work/repo"]));
    }

    #[test]
    fn custom_adapter_uses_dispatch_trust_policy() {
        let mut params = dispatch_params("custom");
        params.command = vec![
            "python3".to_string(),
            "-m".to_string(),
            "worker".to_string(),
        ];

        let cmd = build_custom_command(&params, "run task").expect("custom command");
        assert_eq!(command_args(&cmd), vec!["-m", "worker", "run task"]);

        params.command = vec!["/tmp/python3".to_string()];
        assert!(
            build_custom_command(&params, "run task").is_err(),
            "trusted basenames should not bless untrusted paths"
        );
    }

    /// #1174: `agent='opencode'` with no `command` must not leak the internal
    /// `custom` backend name it happens to share launch mechanics with — the
    /// error must speak the caller's own vocabulary and point at profile
    /// dispatch, with the profile list sourced live from the registry (not
    /// hardcoded), so it can't drift from what `DISPATCH_PROFILES` actually has.
    #[test]
    fn opencode_agent_missing_command_keeps_user_vocabulary_and_points_at_profiles() {
        let params = dispatch_params("opencode");
        let err = build_opencode_command(&params, "run task")
            .expect_err("agent='opencode' with no command must fail");

        assert!(
            err.contains("agent='opencode'"),
            "error must keep the caller's own vocabulary, got: {err}"
        );
        assert!(
            !err.contains("custom"),
            "error must not leak the internal 'custom' backend name, got: {err}"
        );
        assert!(
            err.to_ascii_lowercase().contains("profile"),
            "error must point the caller at profile dispatch, got: {err}"
        );

        let expected_profiles: Vec<&str> = tachi_dispatch::DISPATCH_PROFILES
            .iter()
            .filter(|profile| tachi_dispatch::profile_uses_opencode_adapter(profile))
            .map(|profile| profile.name)
            .collect();
        assert!(
            !expected_profiles.is_empty(),
            "registry must have at least one opencode-adapter profile for this test to be meaningful"
        );
        for name in expected_profiles {
            assert!(
                err.contains(name),
                "error must list opencode-adapter profile '{name}' (dynamic from registry), got: {err}"
            );
        }
    }

    /// A non-empty `command` still delegates to the normal custom/opencode
    /// launch path untouched.
    #[test]
    fn opencode_agent_with_command_delegates_to_custom_launch() {
        let mut params = dispatch_params("opencode");
        params.command = vec!["opencode".to_string(), "run".to_string()];

        let cmd = build_opencode_command(&params, "run task").expect("opencode command");
        assert_eq!(command_args(&cmd), vec!["run", "run task"]);
    }
}
